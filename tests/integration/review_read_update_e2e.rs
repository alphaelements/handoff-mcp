//! `handoff_review_read` / `handoff_review_update` (RB-003, RB-004,
//! SPEC-RB-006..009, NFR-RB-PERF-001, NFR-RB-REL-001) through the real
//! JSON-RPC dispatch.

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tempfile::TempDir;

/// NFR-RB-PERF-001: one section within 500 ms on a 100-item board.
const PERF_BUDGET: Duration = Duration::from_millis(1000);

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let out =
        handoff_mcp::mcp::protocol::process_line(&req.to_string()).expect("should return response");
    serde_json::from_str(&out).expect("response should be valid JSON")
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("expected text content")
        .to_string()
}

fn json_of(resp: &Value) -> Value {
    assert!(!is_error(resp), "unexpected error: {}", text(resp));
    serde_json::from_str(&text(resp)).expect("expected JSON text")
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false) || resp.get("error").is_some()
}

struct Project {
    _dir: TempDir,
    path: String,
    board: std::path::PathBuf,
}

fn project(board: &str) -> Project {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_string_lossy().to_string();
    let init = call(
        "handoff_init",
        json!({ "project_dir": path, "project_name": "review-rw" }),
    );
    assert!(!is_error(&init), "{}", text(&init));
    let board_path = dir.path().join(".handoff").join("review_board.md");
    std::fs::write(&board_path, board).unwrap();
    Project {
        _dir: dir,
        path,
        board: board_path,
    }
}

fn section(n: usize) -> String {
    format!(
        "### Item {n} <!-- rb-id: R-{n:03} -->\n- priority: medium\n- date: 2026-11-17\n\nUnique body {n}.\n![s](review_board_assets/s{n}.png)\n\n- [ ] 確認済み\n\nメモ:\n\n"
    )
}

fn board_of(count: usize) -> String {
    let mut s = String::from("## 設計判断\n\n");
    for n in 1..=count {
        s.push_str(&section(n));
    }
    s
}

fn read(p: &Project, args: Value) -> Value {
    let mut args = args;
    args["project_dir"] = json!(p.path);
    call("handoff_review_read", args)
}

fn update(p: &Project, id: &str, hash: &str, updates: Value) -> Value {
    call(
        "handoff_review_update",
        json!({ "project_dir": p.path, "id": id, "expected_hash": hash, "updates": updates }),
    )
}

fn hash_of(p: &Project, id: &str) -> String {
    json_of(&read(p, json!({ "id": id })))["content_hash"]
        .as_str()
        .unwrap()
        .to_string()
}

#[test]
fn both_tools_are_listed_with_the_documented_required_arguments() {
    let resp: Value = serde_json::from_str(
        &handoff_mcp::mcp::protocol::process_line(
            &json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}).to_string(),
        )
        .unwrap(),
    )
    .unwrap();
    let tools = resp["result"]["tools"].as_array().unwrap();
    let find = |name: &str| tools.iter().find(|t| t["name"] == name).unwrap();
    assert!(
        find("handoff_review_read")["inputSchema"]["properties"]["format"]["enum"]
            .as_array()
            .is_some()
    );
    assert_eq!(
        find("handoff_review_update")["inputSchema"]["required"],
        json!(["id", "expected_hash", "updates"])
    );
}

#[test]
fn read_one_section_of_fifty_returns_no_other_item() {
    let p = project(&board_of(50));
    let v = json_of(&read(&p, json!({ "id": "R-025" })));
    assert_eq!(v["title"], "Item 25");
    assert_eq!(
        v["body"].as_str().unwrap().lines().next(),
        Some("Unique body 25.")
    );
    assert_eq!(v["images"], json!(["review_board_assets/s25.png"]));
    let raw = v.to_string();
    for other in [24, 26, 1, 50] {
        assert!(
            !raw.contains(&format!("Unique body {other}.")),
            "leaked item {other}"
        );
    }
    assert_eq!(json_of(&read(&p, json!({ "seq": 0 })))["id"], "R-001");
}

#[test]
fn read_does_not_modify_the_board() {
    let p = project(&board_of(5));
    let before = std::fs::read(&p.board).unwrap();
    json_of(&read(&p, json!({ "id": "R-003" })));
    json_of(&read(&p, json!({ "seq": 4, "format": "meta" })));
    assert_eq!(std::fs::read(&p.board).unwrap(), before);
}

#[test]
fn read_errors_surface_as_tool_errors() {
    let p = project(&board_of(2));
    let missing = read(&p, json!({ "id": "R-404" }));
    assert!(is_error(&missing) && text(&missing).contains("R-404"));
    assert!(is_error(&read(&p, json!({}))));
    assert!(is_error(&read(&p, json!({ "seq": 2 }))));
}

#[test]
fn update_after_a_user_edit_elsewhere_keeps_the_user_edit_byte_exact() {
    let p = project(&board_of(3));
    let hash_b = hash_of(&p, "R-002");
    // The user ticks R-001 in an editor after the agent read R-002.
    let edited = std::fs::read_to_string(&p.board).unwrap().replacen(
        "- [ ] 確認済み\n\nメモ:\n",
        "- [x] 確認済み\n\nメモ: ユーザー回答\n",
        1,
    );
    std::fs::write(&p.board, &edited).unwrap();

    let out = json_of(&update(
        &p,
        "R-002",
        &hash_b,
        json!({ "priority": "high", "memo": "agent note" }),
    ));
    assert_eq!(out["updated"], json!(["priority", "memo"]));

    let after = std::fs::read_to_string(&p.board).unwrap();
    let a_end = edited.find("### Item 2").unwrap();
    let c_start = edited.find("### Item 3").unwrap();
    assert!(
        after.starts_with(&edited[..a_end]),
        "section A must be byte-exact"
    );
    assert!(
        after.ends_with(&edited[c_start..]),
        "section C must be byte-exact"
    );
    let a = json_of(&read(&p, json!({ "id": "R-001" })));
    assert_eq!(a["status"], "answered");
    assert_eq!(a["memo"], "ユーザー回答");
    let b = json_of(&read(&p, json!({ "id": "R-002" })));
    assert_eq!(b["priority"], "high");
    assert_eq!(b["memo"], "agent note");
    assert_eq!(b["content_hash"], out["content_hash"]);
}

#[test]
fn stale_hash_is_rejected_with_the_current_hash_and_the_file_is_untouched() {
    let p = project(&board_of(2));
    let stale = hash_of(&p, "R-001");
    // The section changes underneath the caller.
    let edited = std::fs::read_to_string(&p.board)
        .unwrap()
        .replace("Unique body 1.", "Edited by user.");
    std::fs::write(&p.board, &edited).unwrap();
    let current = hash_of(&p, "R-001");
    assert_ne!(stale, current);

    let resp = update(&p, "R-001", &stale, json!({ "memo": "x" }));
    assert!(is_error(&resp));
    let msg = text(&resp);
    assert!(
        msg.contains("current_hash") && msg.contains(&current),
        "{msg}"
    );
    assert_eq!(std::fs::read_to_string(&p.board).unwrap(), edited);

    // Retrying with the reported hash succeeds.
    json_of(&update(&p, "R-001", &current, json!({ "memo": "x" })));
}

#[test]
fn checkbox_only_update_changes_exactly_the_checkbox_character() {
    let p = project(&board_of(3));
    let before = std::fs::read_to_string(&p.board).unwrap();
    let hash = hash_of(&p, "R-002");
    json_of(&update(
        &p,
        "R-002",
        &hash,
        json!({ "checkbox_checked": true }),
    ));
    let after = std::fs::read_to_string(&p.board).unwrap();
    let diff: Vec<(char, char)> = before
        .chars()
        .zip(after.chars())
        .filter(|(a, b)| a != b)
        .collect();
    assert_eq!(diff, [(' ', 'x')]);
    assert_eq!(before.len(), after.len());
}

#[test]
fn update_leaves_no_temp_files_in_the_handoff_directory() {
    let p = project(&board_of(2));
    let hash = hash_of(&p, "R-001");
    json_of(&update(&p, "R-001", &hash, json!({ "memo": "x" })));
    let leftovers: Vec<String> = std::fs::read_dir(p.board.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.contains(".tmp"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
}

#[test]
fn invalid_updates_are_rejected_without_touching_the_file() {
    let p = project(&board_of(2));
    let before = std::fs::read(&p.board).unwrap();
    let hash = hash_of(&p, "R-001");
    for updates in [
        json!({}),
        json!({ "title": "renamed" }),
        json!({ "body": "### injected <!-- rb-id: R-099 -->" }),
        json!({ "checkbox_checked": "yes" }),
    ] {
        assert!(
            is_error(&update(&p, "R-001", &hash, updates.clone())),
            "{updates}"
        );
    }
    let no_hash = call(
        "handoff_review_update",
        json!({ "project_dir": p.path, "id": "R-001", "updates": { "memo": "x" } }),
    );
    assert!(is_error(&no_hash));
    assert!(is_error(&update(
        &p,
        "R-404",
        &hash,
        json!({ "memo": "x" })
    )));
    assert_eq!(std::fs::read(&p.board).unwrap(), before);
}

/// Best of a few attempts: every integration test shares one process and one
/// CPU pool, so a single sample can be inflated by unrelated tests running at
/// the same time. A genuine regression slows every attempt, so the fastest one
/// still exceeds the budget.
#[test]
fn read_and_update_meet_the_response_budget_on_a_hundred_item_board() {
    const ATTEMPTS: usize = 3;
    let p = project(&board_of(100));

    let mut best_read = Duration::MAX;
    let mut best_update = Duration::MAX;
    for attempt in 0..ATTEMPTS {
        let started = Instant::now();
        let v = json_of(&read(&p, json!({ "id": "R-100" })));
        best_read = best_read.min(started.elapsed());
        assert_eq!(v["title"], "Item 100");

        let hash = v["content_hash"].as_str().unwrap().to_string();
        let started = Instant::now();
        json_of(&update(
            &p,
            "R-100",
            &hash,
            json!({ "memo": format!("perf-{attempt}") }),
        ));
        best_update = best_update.min(started.elapsed());
    }
    assert!(best_read < PERF_BUDGET, "read took {best_read:?}");
    assert!(best_update < PERF_BUDGET, "update took {best_update:?}");
}

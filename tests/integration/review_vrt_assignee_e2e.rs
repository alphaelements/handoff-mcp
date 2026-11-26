//! Image comparison metadata and assignee validation of the review board
//! (RB-011, RB-012 / SPEC-RB-031..039) through the real JSON-RPC dispatch.

use serde_json::{json, Value};
use tempfile::TempDir;

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

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false) || resp.get("error").is_some()
}

fn json_of(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("expected text content");
    assert!(!is_error(resp), "unexpected error: {text}");
    serde_json::from_str(text).expect("expected JSON text")
}

struct Project {
    dir: TempDir,
    path: String,
}

impl Project {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().to_string_lossy().to_string();
        let init = call(
            "handoff_init",
            json!({ "project_dir": path, "project_name": "review-vrt" }),
        );
        assert!(!is_error(&init), "{init}");
        Self { dir, path }
    }

    fn board(&self) -> String {
        std::fs::read_to_string(self.dir.path().join(".handoff/review_board.md")).unwrap()
    }

    fn write_board(&self, text: &str) {
        std::fs::write(self.dir.path().join(".handoff/review_board.md"), text).unwrap();
    }

    /// Appends `extra` to the config `handoff_init` wrote (a valid config
    /// needs its `[project]` table).
    fn write_config(&self, extra: &str) {
        let path = self.dir.path().join(".handoff/config.toml");
        let base = std::fs::read_to_string(&path).unwrap();
        std::fs::write(path, format!("{base}\n{extra}")).unwrap();
    }

    fn replace_config(&self, text: &str) {
        std::fs::write(self.dir.path().join(".handoff/config.toml"), text).unwrap();
    }

    fn call(&self, name: &str, mut args: Value) -> Value {
        args["project_dir"] = json!(self.path);
        call(name, args)
    }

    fn add(&self, mut args: Value) -> Value {
        args["category"] = json!("テスト確認");
        if args.get("title").is_none() {
            args["title"] = json!("Compare login screen");
        }
        json_of(&self.call("handoff_review_add", args))
    }

    fn read(&self, id: &str) -> Value {
        json_of(&self.call("handoff_review_read", json!({ "id": id })))
    }

    fn list(&self, args: Value) -> Value {
        json_of(&self.call("handoff_review_list", args))
    }
}

const COMPARISON_KEYS: [&str; 5] = [
    "expected_image",
    "actual_image",
    "diff_image",
    "similarity_pct",
    "threshold",
];

fn warnings(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .map(|w| w.as_str().unwrap().to_string())
        .collect()
}

// --- RB-011 -------------------------------------------------------------

#[test]
fn add_writes_all_five_comparison_lines_in_order_before_the_body() {
    let p = Project::new();
    let out = p.add(json!({
        "body": "Looks off by two pixels.",
        "expected_image": "review_board_assets/golden.png",
        "actual_image": "review_board_assets/actual.png",
        "diff_image": "review_board_assets/diff.png",
        "similarity_pct": 97.2,
        "threshold": 99
    }));
    assert_eq!(out["id"], "R-001");
    let board = p.board();
    assert!(
        board.contains(
            "<!-- rb-id: R-001 -->\n- expected_image: review_board_assets/golden.png\n- actual_image: review_board_assets/actual.png\n- diff_image: review_board_assets/diff.png\n- similarity_pct: 97.2\n- threshold: 99\n- date: "
        ),
        "{board}"
    );
    assert!(board.find("- threshold: 99").unwrap() < board.find("Looks off").unwrap());
    // Attribute paths are neither copied nor turned into image links.
    assert!(!board.contains("!["));
    assert!(!p.dir.path().join(".handoff/review_board_assets").exists());
    let gitignore =
        std::fs::read_to_string(p.dir.path().join(".handoff/.gitignore")).unwrap_or_default();
    assert!(!gitignore.contains("review_board_assets"));
}

#[test]
fn add_without_comparison_parameters_writes_no_comparison_line() {
    let p = Project::new();
    p.add(json!({ "priority": "high" }));
    let board = p.board();
    for key in COMPARISON_KEYS {
        assert!(!board.contains(&format!("- {key}:")), "{key}: {board}");
    }
}

#[test]
fn add_writes_only_the_provided_comparison_parameters() {
    let p = Project::new();
    p.add(json!({ "similarity_pct": 97.2, "threshold": 99 }));
    let board = p.board();
    assert!(board.contains("- similarity_pct: 97.2\n- threshold: 99\n"));
    assert!(!board.contains("expected_image"));
}

#[test]
fn add_rejects_a_non_numeric_similarity_or_a_multiline_path() {
    let p = Project::new();
    for args in [
        json!({"similarity_pct": "high"}),
        json!({"threshold": "99"}),
        json!({"expected_image": "a\nb.png"}),
        json!({"actual_image": 5}),
    ] {
        let mut args = args;
        args["title"] = json!("T");
        args["category"] = json!("C");
        let resp = p.call("handoff_review_add", args);
        assert!(is_error(&resp), "{resp}");
    }
    assert!(!p.dir.path().join(".handoff/review_board.md").exists());
}

#[test]
fn read_returns_paths_and_numbers_in_full_and_meta_formats() {
    let p = Project::new();
    p.add(json!({
        "expected_image": "review_board_assets/golden.png",
        "actual_image": "review_board_assets/actual.png",
        "diff_image": "review_board_assets/diff.png",
        "similarity_pct": 98.5,
        "threshold": 99
    }));
    for format in ["full", "meta"] {
        let v = json_of(&p.call(
            "handoff_review_read",
            json!({ "id": "R-001", "format": format }),
        ));
        assert_eq!(v["expected_image"], "review_board_assets/golden.png");
        assert_eq!(v["actual_image"], "review_board_assets/actual.png");
        assert_eq!(v["diff_image"], "review_board_assets/diff.png");
        assert_eq!(v["similarity_pct"], json!(98.5));
        assert_eq!(v["threshold"], json!(99));
        assert!(v["threshold"].is_i64(), "99 stays an integer: {v}");
        assert_eq!(v["warnings"], json!([]));
    }
}

#[test]
fn read_of_an_item_without_comparison_lines_has_five_null_keys() {
    let p = Project::new();
    p.add(json!({}));
    let v = p.read("R-001");
    for key in COMPARISON_KEYS {
        assert!(v.get(key).is_some() && v[key].is_null(), "{key}: {v}");
    }
}

#[test]
fn a_comparison_line_in_the_body_is_not_an_attribute() {
    let p = Project::new();
    p.write_board(
        "## C\n\n### A <!-- rb-id: R-001 -->\n- date: 2026-11-17\n\nBody text\n- similarity_pct: 10\n\n- [ ] 確認済み\n\nメモ:\n",
    );
    assert!(p.read("R-001")["similarity_pct"].is_null());
    let listed = p.list(json!({}));
    assert!(listed["items"][0]["similarity_pct"].is_null());
}

#[test]
fn non_numeric_values_read_as_null_with_a_warning_and_out_of_range_is_kept() {
    let p = Project::new();
    p.write_board(
        "## C\n\n### A <!-- rb-id: R-001 -->\n- similarity_pct: high\n- threshold:\n- date: 2026-11-17\n\n- [ ] 確認済み\n\nメモ:\n\n### B <!-- rb-id: R-002 -->\n- similarity_pct: 105\n- threshold: NaN\n\n- [ ] 確認済み\n\nメモ:\n",
    );
    let a = json_of(&p.call("handoff_review_read", json!({"id": "R-001"})));
    assert!(a["similarity_pct"].is_null() && a["threshold"].is_null());
    let w = warnings(&a);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("R-001") && w[0].contains("similarity_pct"));

    let b = json_of(&p.call("handoff_review_read", json!({"id": "R-002"})));
    assert_eq!(b["similarity_pct"], json!(105));
    assert!(b["threshold"].is_null());
    let w = warnings(&b);
    assert_eq!(w.len(), 1, "{w:?}");
    assert!(w[0].contains("threshold"));
}

#[test]
fn list_carries_comparison_values_equal_to_read_and_has_comparison() {
    let p = Project::new();
    p.add(json!({
        "title": "Full",
        "expected_image": "e.png",
        "actual_image": "a.png",
        "diff_image": "d.png",
        "similarity_pct": 98.5,
        "threshold": 99
    }));
    p.add(json!({ "title": "Expected only", "expected_image": "e.png" }));
    p.add(json!({ "title": "Numbers only", "similarity_pct": 90, "threshold": 95 }));
    p.add(json!({ "title": "Plain" }));
    let listed = p.list(json!({}));
    let items = listed["items"].as_array().unwrap();
    assert_eq!(listed["summary"]["total"], 4);

    let read = p.read("R-001");
    for key in COMPARISON_KEYS {
        assert_eq!(items[0][key], read[key], "{key}");
    }
    assert_eq!(items[0]["has_comparison"], true);
    // has_comparison needs both images; it is independent of has_images.
    assert_eq!(items[0]["has_images"], false);
    assert_eq!(items[1]["has_comparison"], false);
    assert_eq!(items[2]["has_comparison"], false);
    assert_eq!(items[3]["has_comparison"], false);
    for key in COMPARISON_KEYS {
        assert!(items[3][key].is_null());
    }
}

#[test]
fn comparison_lines_survive_update_and_a_user_edit_is_read_back() {
    let p = Project::new();
    p.add(json!({
        "expected_image": "e.png",
        "actual_image": "a.png",
        "similarity_pct": 97.2,
        "threshold": 99
    }));
    let lines_before: Vec<String> = p
        .board()
        .lines()
        .filter(|l| {
            COMPARISON_KEYS
                .iter()
                .any(|k| l.starts_with(&format!("- {k}:")))
        })
        .map(str::to_string)
        .collect();
    assert_eq!(lines_before.len(), 4);

    let hash = p.read("R-001")["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let upd = json_of(&p.call(
        "handoff_review_update",
        json!({"id": "R-001", "expected_hash": hash, "updates": {"checkbox_checked": true, "priority": "low"}}),
    ));
    assert_eq!(upd["status"], "answered");
    let board = p.board();
    for l in &lines_before {
        assert!(board.contains(&format!("{l}\n")), "{l} lost: {board}");
    }
    assert!(board.contains("- priority: low\n"));
    assert!(board.contains("<!-- rb-id: R-001 -->"));

    p.write_board(&board.replace("- threshold: 99", "- threshold: 95"));
    let v = p.read("R-001");
    assert_eq!(v["threshold"], json!(95));
    assert_eq!(v["id"], "R-001");
}

// --- RB-012 -------------------------------------------------------------

fn assignee_warnings(v: &Value) -> Vec<String> {
    warnings(v)
        .into_iter()
        .filter(|w| w.contains("assignee"))
        .collect()
}

#[test]
fn a_roster_assignee_gets_no_warning() {
    let p = Project::new();
    p.write_config("[assignees.ryoma]\ndisplay_name = \"Ryoma\"\n");
    let out = p.add(json!({ "assignee": "ryoma" }));
    assert!(warnings(&out).is_empty(), "{out}");
    assert!(p.board().contains("- assignee: ryoma\n"));
}

#[test]
fn an_unknown_assignee_is_written_with_a_warning_naming_the_key() {
    let p = Project::new();
    p.write_config("[assignees.ryoma]\ndisplay_name = \"Ryoma\"\n");
    let out = p.add(json!({ "assignee": "nobody" }));
    let w = assignee_warnings(&out);
    assert_eq!(w.len(), 1, "{out}");
    assert!(w[0].contains("nobody") && w[0].contains("[assignees]"));
    assert!(p.board().contains("- assignee: nobody\n"));
}

#[test]
fn assignee_keys_are_case_sensitive() {
    let p = Project::new();
    p.write_config("[assignees.ryoma]\n");
    let out = p.add(json!({ "assignee": "Ryoma" }));
    assert_eq!(assignee_warnings(&out).len(), 1, "{out}");
}

#[test]
fn a_missing_roster_or_config_makes_every_key_unknown_but_the_write_succeeds() {
    let p = Project::new();
    // The config `handoff_init` wrote has no `[assignees]` table.
    let out = p.add(json!({ "assignee": "ryoma" }));
    assert_eq!(assignee_warnings(&out).len(), 1, "{out}");
    assert!(p.board().contains("- assignee: ryoma\n"));

    std::fs::remove_file(p.dir.path().join(".handoff/config.toml")).unwrap();
    let out = p.add(json!({ "assignee": "ryoma" }));
    assert_eq!(
        warnings(&out).len(),
        1,
        "no config is not a load warning: {out}"
    );
    assert_eq!(assignee_warnings(&out).len(), 1, "{out}");
}

#[test]
fn an_unreadable_config_never_turns_a_write_into_an_error() {
    let p = Project::new();
    p.replace_config("this is = = not toml [[[");
    let out = p.add(json!({ "assignee": "ryoma" }));
    assert!(p.board().contains("- assignee: ryoma\n"));
    assert!(assignee_warnings(&out).iter().any(|w| w.contains("ryoma")));
}

#[test]
fn add_without_or_with_an_empty_assignee_returns_no_assignee_warning() {
    let p = Project::new();
    p.write_config("[assignees.ryoma]\n");
    assert!(assignee_warnings(&p.add(json!({}))).is_empty());
    assert!(assignee_warnings(&p.add(json!({ "assignee": "" }))).is_empty());
    assert!(assignee_warnings(&p.add(json!({ "assignee": "   " }))).is_empty());
}

#[test]
fn update_validates_the_assignee_and_empty_removal_is_not_validated() {
    let p = Project::new();
    p.write_config("[assignees.ryoma]\n");
    p.add(json!({ "assignee": "ryoma" }));
    let update = |assignee: &str| {
        let hash = p.read("R-001")["content_hash"]
            .as_str()
            .unwrap()
            .to_string();
        json_of(&p.call(
            "handoff_review_update",
            json!({"id": "R-001", "expected_hash": hash, "updates": {"assignee": assignee}}),
        ))
    };
    let known = update("ryoma");
    assert!(assignee_warnings(&known).is_empty(), "{known}");
    let unknown = update("nobody");
    assert_eq!(assignee_warnings(&unknown).len(), 1, "{unknown}");
    assert!(p.board().contains("- assignee: nobody\n"));
    let removed = update("");
    assert!(assignee_warnings(&removed).is_empty(), "{removed}");
    assert!(!p.board().contains("- assignee:"));

    // An update that does not touch the assignee never validates it.
    p.write_board(
        &p.board()
            .replacen("- date:", "- assignee: ghost\n- date:", 1),
    );
    let hash = p.read("R-001")["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let out = json_of(&p.call(
        "handoff_review_update",
        json!({"id": "R-001", "expected_hash": hash, "updates": {"memo": "x"}}),
    ));
    assert!(assignee_warnings(&out).is_empty(), "{out}");
}

#[test]
fn read_and_list_do_not_warn_about_an_assignee_outside_the_roster() {
    let p = Project::new();
    p.write_config("[assignees.ryoma]\n");
    p.add(json!({ "assignee": "nobody" }));
    assert!(assignee_warnings(&p.read("R-001")).is_empty());
    assert!(assignee_warnings(&p.list(json!({}))).is_empty());
}

#[test]
fn assignee_filter_matches_exactly_combines_with_and_and_never_writes() {
    let p = Project::new();
    p.write_config("[assignees.ryoma]\n[assignees.alice]\n");
    p.add(json!({ "title": "A", "assignee": "ryoma" }));
    p.add(json!({ "title": "B", "assignee": "alice" }));
    p.add(json!({ "title": "C" }));
    p.add(json!({ "title": "D", "assignee": "ryoma" }));
    let hash = p.read("R-004")["content_hash"]
        .as_str()
        .unwrap()
        .to_string();
    json_of(&p.call(
        "handoff_review_update",
        json!({"id": "R-004", "expected_hash": hash, "updates": {"checkbox_checked": true}}),
    ));

    let before = p.board();
    let ids = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["id"].as_str().unwrap().to_string())
            .collect()
    };
    let ryoma = p.list(json!({ "assignee_filter": "ryoma" }));
    assert_eq!(ids(&ryoma), ["R-001", "R-004"]);
    let both = p.list(json!({ "assignee_filter": "ryoma", "status_filter": "answered" }));
    assert_eq!(ids(&both), ["R-004"]);
    // Unassigned items never match, nor does a different case or an unknown key.
    for filter in ["Ryoma", "nobody", ""] {
        let none = p.list(json!({ "assignee_filter": filter }));
        assert_eq!(
            none["summary"],
            json!({"total": 0, "pending": 0, "answered": 0}),
            "{filter}"
        );
        assert_eq!(none["items"], json!([]));
        assert!(assignee_warnings(&none).is_empty());
    }
    assert_eq!(p.board(), before);
}

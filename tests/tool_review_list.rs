//! `handoff_review_list` tool (RB-005 / SPEC-RB-010, SPEC-RB-011, SPEC-RB-013).

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

fn text_of(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("expected text content");
    serde_json::from_str(text).expect("expected valid JSON text")
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false) || resp.get("error").is_some()
}

fn init(dir: &TempDir) -> String {
    let project_dir = dir.path().to_string_lossy().to_string();
    call(
        "handoff_init",
        json!({ "project_dir": project_dir, "project_name": "review-list-test" }),
    );
    project_dir
}

fn item(
    id: &str,
    title: &str,
    attrs: &[(&str, &str)],
    body: &str,
    checked: bool,
    memo: &str,
) -> String {
    let mut s = format!("### {title} <!-- rb-id: {id} -->\n");
    for (k, v) in attrs {
        s.push_str(&format!("- {k}: {v}\n"));
    }
    s.push_str("- date: 2026-11-17\n");
    if !body.is_empty() {
        s.push_str(body);
        s.push('\n');
    }
    s.push_str(if checked {
        "- [x] 確認済み\n"
    } else {
        "- [ ] 確認済み\n"
    });
    s.push_str(&format!("メモ: {memo}\n\n"));
    s
}

/// 8 items: 3 pending, 5 answered (checked x2, memo-only x2, both x1),
/// spread over two categories, with assignees and one image.
fn sample_board() -> String {
    let mut b = String::from("## 設計判断\n\n");
    b.push_str(&item(
        "R-001",
        "Pending A",
        &[("priority", "high"), ("assignee", "ryoma")],
        "",
        false,
        "",
    ));
    b.push_str(&item(
        "R-002",
        "Checked B",
        &[("assignee", "ryoma")],
        "",
        true,
        "",
    ));
    b.push_str(&item(
        "R-003",
        "Memo only C",
        &[("assignee", "bob")],
        "",
        false,
        "looks fine",
    ));
    b.push_str(&item(
        "R-004",
        "Pending D",
        &[],
        "see ![shot](img/a.png)",
        false,
        "",
    ));
    b.push_str("## 実装確認\n\n");
    b.push_str(&item(
        "R-005",
        "Both E",
        &[("assignee", "ryoma")],
        "",
        true,
        "ok",
    ));
    b.push_str(&item(
        "R-006",
        "Pending F",
        &[("assignee", "ryoma")],
        "",
        false,
        "",
    ));
    b.push_str(&item("R-007", "Checked G", &[], "", true, ""));
    b.push_str(&item(
        "R-008",
        "Memo H",
        &[("assignee", "bob")],
        "",
        false,
        "needs work",
    ));
    b
}

fn setup_with_board() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let project_dir = init(&dir);
    std::fs::write(dir.path().join(".handoff/review_board.md"), sample_board()).unwrap();
    (dir, project_dir)
}

fn list(project_dir: &str, extra: Value) -> Value {
    let mut args = json!({ "project_dir": project_dir });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    call("handoff_review_list", args)
}

fn ids(parsed: &Value) -> Vec<String> {
    parsed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

/// SPEC-RB-010 AC1 / SPEC-RB-011 AC4: all items in file order with summary.
#[test]
fn list_returns_all_items_in_file_order_with_summary() {
    let (_d, p) = setup_with_board();
    let resp = list(&p, json!({}));
    assert!(!is_error(&resp), "{resp}");
    let parsed = text_of(&resp);
    assert_eq!(
        ids(&parsed),
        ["R-001", "R-002", "R-003", "R-004", "R-005", "R-006", "R-007", "R-008"]
    );
    assert_eq!(
        parsed["summary"],
        json!({ "total": 8, "pending": 3, "answered": 5 })
    );
    let first = &parsed["items"][0];
    assert_eq!(first["title"], "Pending A");
    assert_eq!(first["status"], "pending");
    assert_eq!(first["priority"], "high");
    assert_eq!(first["assignee"], "ryoma");
    assert_eq!(first["date"], "2026-11-17");
    assert_eq!(first["category"], "設計判断");
    assert_eq!(first["has_images"], false);
    assert_eq!(parsed["items"][4]["category"], "実装確認");
}

/// SPEC-RB-011 AC1-AC3: status detection rules.
#[test]
fn list_status_detection_rules() {
    let (_d, p) = setup_with_board();
    let parsed = text_of(&list(&p, json!({})));
    let status: Vec<&str> = parsed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["status"].as_str().unwrap())
        .collect();
    assert_eq!(
        status,
        [
            "pending", "answered", "answered", "pending", "answered", "pending", "answered",
            "answered"
        ]
    );
}

/// SPEC-RB-010 AC2: status_filter.
#[test]
fn list_status_filter_pending_and_answered() {
    let (_d, p) = setup_with_board();
    let pending = text_of(&list(&p, json!({ "status_filter": "pending" })));
    assert_eq!(ids(&pending), ["R-001", "R-004", "R-006"]);
    assert_eq!(
        pending["summary"],
        json!({ "total": 3, "pending": 3, "answered": 0 })
    );
    let answered = text_of(&list(&p, json!({ "status_filter": "answered" })));
    assert_eq!(
        ids(&answered),
        ["R-002", "R-003", "R-005", "R-007", "R-008"]
    );
    assert_eq!(
        answered["summary"],
        json!({ "total": 5, "pending": 0, "answered": 5 })
    );
}

/// SPEC-RB-011: resolved items live in the decisions file, so none are listed.
#[test]
fn list_status_filter_resolved_returns_empty() {
    let (_d, p) = setup_with_board();
    let resp = list(&p, json!({ "status_filter": "resolved" }));
    assert!(!is_error(&resp));
    let parsed = text_of(&resp);
    assert!(parsed["items"].as_array().unwrap().is_empty());
    assert_eq!(
        parsed["summary"],
        json!({ "total": 0, "pending": 0, "answered": 0 })
    );
}

/// An unknown status_filter value is an error, not a silent "everything".
#[test]
fn list_invalid_status_filter_is_error() {
    let (_d, p) = setup_with_board();
    let resp = list(&p, json!({ "status_filter": "bogus" }));
    assert!(is_error(&resp), "{resp}");
}

/// SPEC-RB-010 AC3: assignee and category filters combine with AND.
#[test]
fn list_assignee_and_category_filters_combine() {
    let (_d, p) = setup_with_board();
    let by_assignee = text_of(&list(&p, json!({ "assignee_filter": "ryoma" })));
    assert_eq!(ids(&by_assignee), ["R-001", "R-002", "R-005", "R-006"]);
    let by_cat = text_of(&list(&p, json!({ "category_filter": "実装確認" })));
    assert_eq!(ids(&by_cat), ["R-005", "R-006", "R-007", "R-008"]);
    let both = text_of(&list(
        &p,
        json!({ "assignee_filter": "ryoma", "category_filter": "実装確認" }),
    ));
    assert_eq!(ids(&both), ["R-005", "R-006"]);
    let all_three = text_of(&list(
        &p,
        json!({ "assignee_filter": "ryoma", "category_filter": "実装確認", "status_filter": "pending" }),
    ));
    assert_eq!(ids(&all_three), ["R-006"]);
}

/// SPEC-RB-010 AC4: has_images.
#[test]
fn list_has_images_flag() {
    let (_d, p) = setup_with_board();
    let parsed = text_of(&list(&p, json!({})));
    let flags: Vec<bool> = parsed["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["has_images"].as_bool().unwrap())
        .collect();
    assert_eq!(
        flags,
        [false, false, false, true, false, false, false, false]
    );
}

/// SPEC-RB-010 AC5: a filter matching nothing is an empty list, not an error.
#[test]
fn list_filter_matching_nothing_is_empty_not_error() {
    let (_d, p) = setup_with_board();
    let resp = list(&p, json!({ "assignee_filter": "nobody" }));
    assert!(!is_error(&resp));
    let parsed = text_of(&resp);
    assert!(parsed["items"].as_array().unwrap().is_empty());
    assert_eq!(parsed["summary"]["total"], 0);
}

/// SPEC-RB-013 AC1: no board file -> empty result, file not created.
#[test]
fn list_without_board_file_is_empty_and_creates_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let p = init(&dir);
    let resp = list(&p, json!({}));
    assert!(!is_error(&resp), "{resp}");
    let parsed = text_of(&resp);
    assert!(parsed["items"].as_array().unwrap().is_empty());
    assert_eq!(
        parsed["summary"],
        json!({ "total": 0, "pending": 0, "answered": 0 })
    );
    assert!(!dir.path().join(".handoff/review_board.md").exists());
}

/// SPEC-RB-010: listing never modifies the file.
#[test]
fn list_does_not_modify_board() {
    let (d, p) = setup_with_board();
    let path = d.path().join(".handoff/review_board.md");
    let before = std::fs::read(&path).unwrap();
    list(&p, json!({}));
    assert_eq!(std::fs::read(&path).unwrap(), before);
}

/// `##`/`###`-looking lines inside fenced code blocks are body text, and
/// `- [x]` task-list lines in the body do not decide the status.
#[test]
fn list_ignores_headings_in_code_fences_and_body_checkboxes() {
    let dir = tempfile::tempdir().unwrap();
    let p = init(&dir);
    let board = "## Cat\n\n### Real <!-- rb-id: R-001 -->\n- date: 2026-11-17\n```\n### fake <!-- rb-id: R-999 -->\n## fake cat\n```\n- [x] body task\n- [ ] 確認済み\nメモ:\n";
    std::fs::write(dir.path().join(".handoff/review_board.md"), board).unwrap();
    let parsed = text_of(&list(&p, json!({})));
    assert_eq!(ids(&parsed), ["R-001"]);
    assert_eq!(parsed["items"][0]["status"], "pending");
    assert_eq!(parsed["items"][0]["category"], "Cat");
}

/// Items written by `handoff_review_add` list as pending with the attributes
/// the add call gave, and a user ticking the checkbox flips them to answered.
#[test]
fn list_reads_back_items_written_by_review_add() {
    let dir = tempfile::tempdir().unwrap();
    let p = init(&dir);
    let add = call(
        "handoff_review_add",
        json!({
            "project_dir": p,
            "title": "Pick layout",
            "category": "設計判断",
            "body": "Which one?",
            "priority": "high",
            "assignee": "ryoma",
            "image_paths": ["shots/a.png"],
        }),
    );
    assert!(!is_error(&add), "{add}");
    call(
        "handoff_review_add",
        json!({ "project_dir": p, "title": "Other", "category": "新カテゴリ" }),
    );

    let parsed = text_of(&list(&p, json!({})));
    assert_eq!(ids(&parsed), ["R-001", "R-002"]);
    let first = &parsed["items"][0];
    assert_eq!(first["title"], "Pick layout");
    assert_eq!(first["status"], "pending");
    assert_eq!(first["priority"], "high");
    assert_eq!(first["assignee"], "ryoma");
    assert_eq!(first["category"], "設計判断");
    assert_eq!(first["has_images"], true);
    assert_eq!(parsed["items"][1]["category"], "新カテゴリ");
    assert_eq!(parsed["items"][1]["has_images"], false);
    assert_eq!(
        parsed["summary"],
        json!({ "total": 2, "pending": 2, "answered": 0 })
    );

    // The user ticks the first item's checkbox in an editor.
    let path = dir.path().join(".handoff/review_board.md");
    let text = std::fs::read_to_string(&path).unwrap();
    std::fs::write(&path, text.replacen("- [ ] 確認済み", "- [x] 確認済み", 1)).unwrap();
    let parsed = text_of(&list(&p, json!({ "status_filter": "answered" })));
    assert_eq!(ids(&parsed), ["R-001"]);
}

/// Regression: a body line that starts with `メモ:` (or a `- [x]` task) must
/// not make list and read disagree about an item written by
/// `handoff_review_add`; both read the item through one parser.
#[test]
fn list_and_read_agree_on_status_for_an_add_body_with_memo_and_checkbox_lines() {
    let dir = tempfile::tempdir().unwrap();
    let p = init(&dir);
    for (title, body) in [
        ("Memo line", "hello\nメモ: sneaky body line"),
        ("Checked task", "hello\n- [x] sneaky task"),
    ] {
        let add = call(
            "handoff_review_add",
            json!({ "project_dir": p, "title": title, "category": "設計判断", "body": body }),
        );
        assert!(!is_error(&add), "{add}");
    }

    let listed = text_of(&list(&p, json!({})));
    assert_eq!(ids(&listed), ["R-001", "R-002"]);
    for (n, id) in ["R-001", "R-002"].into_iter().enumerate() {
        let read = text_of(&call(
            "handoff_review_read",
            json!({ "project_dir": p, "id": id }),
        ));
        assert_eq!(read["status"], "pending", "read of {id}: {read}");
        assert_eq!(listed["items"][n]["status"], read["status"], "{id}");
    }
    // Filtering by pending keeps both unanswered items.
    let pending = text_of(&list(&p, json!({ "status_filter": "pending" })));
    assert_eq!(ids(&pending), ["R-001", "R-002"]);
}

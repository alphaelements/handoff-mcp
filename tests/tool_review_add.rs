//! `handoff_review_init` / `handoff_review_add` tools (RB-001, RB-002 /
//! SPEC-RB-001..005, SPEC-RB-012).

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
        json!({ "project_dir": project_dir, "project_name": "review-add-test" }),
    );
    project_dir
}

fn board(dir: &TempDir) -> String {
    std::fs::read_to_string(dir.path().join(".handoff/review_board.md")).unwrap()
}

#[test]
fn review_tools_are_registered_with_required_params() {
    let defs = handoff_mcp::mcp::tools::all_tool_definitions();
    let add = defs
        .iter()
        .find(|t| t.name == "handoff_review_add")
        .expect("handoff_review_add registered");
    assert_eq!(add.input_schema["required"], json!(["title", "category"]));
    for key in [
        "title",
        "body",
        "category",
        "priority",
        "assignee",
        "image_paths",
        "task_id",
        "doc_id",
    ] {
        assert!(add.input_schema["properties"].get(key).is_some(), "{key}");
    }
    assert!(defs.iter().any(|t| t.name == "handoff_review_init"));
}

#[test]
fn init_creates_the_board_once() {
    let dir = tempfile::tempdir().unwrap();
    let pd = init(&dir);
    assert!(!dir.path().join(".handoff/review_board.md").exists());

    let first = text_of(&call("handoff_review_init", json!({ "project_dir": pd })));
    assert_eq!(first["created"], json!(true));
    let text = board(&dir);
    assert!(!text.starts_with("---"));
    assert!(text.contains("## 設計判断\n"));

    let second = text_of(&call("handoff_review_init", json!({ "project_dir": pd })));
    assert_eq!(second["created"], json!(false));
    assert_eq!(board(&dir), text);
}

#[test]
fn add_appends_an_item_and_returns_its_id() {
    let dir = tempfile::tempdir().unwrap();
    let pd = init(&dir);
    let resp = call(
        "handoff_review_add",
        json!({
            "project_dir": pd,
            "title": "Choose the storage layout",
            "body": "Option A keeps one file.",
            "category": "設計判断",
            "priority": "high",
            "assignee": "ryoma",
            "task_id": "t-urb.3",
            "doc_id": "urb-rb-core",
            "image_paths": ["review_board_assets/a.png"]
        }),
    );
    assert!(!is_error(&resp), "{resp}");
    let out = text_of(&resp);
    assert_eq!(out["id"], json!("R-001"));
    assert_eq!(out["category"], json!("設計判断"));

    let text = board(&dir);
    assert!(text.contains("### Choose the storage layout <!-- rb-id: R-001 -->\n- priority: high\n- assignee: ryoma\n- task_id: t-urb.3\n- doc_id: urb-rb-core\n- date: "));
    assert!(text.contains("Option A keeps one file."));
    assert!(text.contains("![a.png](review_board_assets/a.png)"));
    assert!(text.contains("- [ ] 確認済み\n\nメモ:\n"));
}

#[test]
fn consecutive_adds_get_unique_increasing_ids() {
    let dir = tempfile::tempdir().unwrap();
    let pd = init(&dir);
    let add = |title: &str, category: &str| {
        text_of(&call(
            "handoff_review_add",
            json!({ "project_dir": pd, "title": title, "category": category }),
        ))["id"]
            .as_str()
            .unwrap()
            .to_string()
    };
    assert_eq!(add("A", "その他"), "R-001");
    assert_eq!(add("B", "新カテゴリ"), "R-002");
    assert_eq!(add("C", "その他"), "R-003");
    let text = board(&dir);
    assert_eq!(text.matches("## 新カテゴリ").count(), 1);
}

#[test]
fn add_creates_the_board_when_missing() {
    let dir = tempfile::tempdir().unwrap();
    let pd = init(&dir);
    let out = text_of(&call(
        "handoff_review_add",
        json!({ "project_dir": pd, "title": "A", "category": "実装確認" }),
    ));
    assert_eq!(out["board_created"], json!(true));
    assert!(board(&dir).contains("## テスト確認"));
}

#[test]
fn missing_title_or_category_is_an_error_and_the_board_is_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let pd = init(&dir);
    call("handoff_review_init", json!({ "project_dir": pd }));
    let before = board(&dir);

    for args in [
        json!({ "project_dir": pd, "category": "その他" }),
        json!({ "project_dir": pd, "title": "A" }),
        json!({ "project_dir": pd, "title": "", "category": "その他" }),
        json!({ "project_dir": pd, "title": "A", "category": "その他", "image_paths": "a.png" }),
        json!({ "project_dir": pd, "title": "A", "category": "その他", "body": 5 }),
        json!({ "project_dir": pd, "title": "A", "category": "その他", "prioriti": "high" }),
    ] {
        let resp = call("handoff_review_add", args.clone());
        assert!(is_error(&resp), "should fail: {args}");
        assert_eq!(board(&dir), before);
    }
}

#[test]
fn structure_breaking_body_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let pd = init(&dir);
    let resp = call(
        "handoff_review_add",
        json!({ "project_dir": pd, "title": "A", "category": "その他", "body": "x\n## Oops" }),
    );
    assert!(is_error(&resp));
    assert!(!dir.path().join(".handoff/review_board.md").exists());
}

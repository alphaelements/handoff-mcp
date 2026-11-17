//! RB-010 image embedding through the MCP tools (SPEC-RB-027..030): add with
//! both path types, read them back, list `has_images`, and the
//! `.handoff/.gitignore` entry for `review_board_assets/` (NFR-RB-SEC-001).

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

fn json_of(resp: &Value) -> Value {
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "{resp}"
    );
    serde_json::from_str(resp["result"]["content"][0]["text"].as_str().unwrap()).unwrap()
}

fn project() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "review-images" }),
    );
    (dir, pd)
}

fn add(pd: &str, title: &str, paths: Value) -> String {
    let v = json_of(&call(
        "handoff_review_add",
        json!({ "project_dir": pd, "title": title, "category": "その他", "image_paths": paths }),
    ));
    v["id"].as_str().unwrap().to_string()
}

fn gitignore_count(dir: &TempDir) -> usize {
    std::fs::read_to_string(dir.path().join(".handoff/.gitignore"))
        .unwrap_or_default()
        .lines()
        .filter(|l| l.trim() == "review_board_assets/")
        .count()
}

#[test]
fn mixed_image_types_round_trip_through_add_read_and_list() {
    let (dir, pd) = project();
    let id = add(
        &pd,
        "Mixed",
        json!([
            ".handoff/review_board_assets/shot.png",
            "../design/mockup.png"
        ]),
    );
    let read = json_of(&call(
        "handoff_review_read",
        json!({ "project_dir": pd, "id": id }),
    ));
    assert_eq!(
        read["images"],
        json!(["review_board_assets/shot.png", "../design/mockup.png"])
    );
    let list = json_of(&call("handoff_review_list", json!({ "project_dir": pd })));
    assert_eq!(list["items"][0]["has_images"], true);

    let board = std::fs::read_to_string(dir.path().join(".handoff/review_board.md")).unwrap();
    assert!(board.contains("![shot.png](review_board_assets/shot.png)"));
    assert!(board.contains("![mockup.png](../design/mockup.png)"));
}

#[test]
fn project_relative_link_resolves_from_the_handoff_dir() {
    let (dir, pd) = project();
    std::fs::create_dir_all(dir.path().join("design")).unwrap();
    std::fs::write(dir.path().join("design/mockup.png"), b"png").unwrap();
    add(&pd, "Rel", json!(["../design/mockup.png"]));
    let board = std::fs::read_to_string(dir.path().join(".handoff/review_board.md")).unwrap();
    let target = board
        .lines()
        .find_map(|l| l.strip_prefix("![mockup.png](")?.strip_suffix(')'))
        .unwrap();
    assert!(dir.path().join(".handoff").join(target).is_file());
}

#[test]
fn asset_image_ignores_the_assets_dir_exactly_once() {
    let (dir, pd) = project();
    assert_eq!(gitignore_count(&dir), 0);
    let before = std::fs::read_to_string(dir.path().join(".handoff/.gitignore")).unwrap();
    add(&pd, "A", json!(["review_board_assets/a.png"]));
    add(&pd, "B", json!(["review_board_assets/b.png"]));
    assert_eq!(gitignore_count(&dir), 1);
    let after = std::fs::read_to_string(dir.path().join(".handoff/.gitignore")).unwrap();
    assert!(after.starts_with(&before), "existing entries are kept");
}

#[test]
fn project_relative_only_leaves_gitignore_unchanged() {
    let (dir, pd) = project();
    let before = std::fs::read_to_string(dir.path().join(".handoff/.gitignore")).unwrap();
    add(&pd, "R", json!(["../design/mockup.png"]));
    assert_eq!(
        std::fs::read_to_string(dir.path().join(".handoff/.gitignore")).unwrap(),
        before
    );
    assert!(!dir.path().join(".handoff/review_board_assets").exists());
}

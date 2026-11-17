//! `handoff_review_resolve` (RBD-001, RBD-002, SPEC-RBD-001..009) through the
//! real JSON-RPC dispatch, together with the other review tools.

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
            json!({ "project_dir": path, "project_name": "review-resolve" }),
        );
        assert!(!is_error(&init), "{}", text(&init));
        Self { dir, path }
    }

    fn file(&self, name: &str) -> std::path::PathBuf {
        self.dir.path().join(".handoff").join(name)
    }

    fn call(&self, name: &str, mut args: Value) -> Value {
        args["project_dir"] = json!(self.path);
        call(name, args)
    }

    fn add(&self, title: &str, category: &str) -> String {
        let r = self.call(
            "handoff_review_add",
            json!({ "title": title, "category": category }),
        );
        json_of(&r)["id"].as_str().unwrap().to_string()
    }

    fn resolve(&self, id: &str, decision: &str) -> Value {
        self.call(
            "handoff_review_resolve",
            json!({ "id": id, "decision": decision }),
        )
    }
}

#[test]
fn resolve_moves_the_item_and_every_other_tool_agrees() {
    let p = Project::new();
    let r1 = p.add("Choose layout", "設計判断");
    let r2 = p.add("Keep me", "設計判断");

    let out = json_of(&p.resolve(&r1, "Use two columns"));
    assert_eq!(out["id"], "R-001");

    let dec = std::fs::read_to_string(p.file("review_board_decisions.md")).unwrap();
    assert!(dec.starts_with("# Decisions\n"), "{dec}");
    assert!(dec.contains("### Choose layout <!-- rb-id: R-001 -->"));
    assert!(dec.contains("- original_id: R-001"));
    assert!(dec.contains("Use two columns"));

    // read: not found; list: one item left.
    assert!(is_error(
        &p.call("handoff_review_read", json!({ "id": r1 }))
    ));
    let list = json_of(&p.call("handoff_review_list", json!({})));
    let items = list["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], r2.as_str());
    assert_eq!(list["summary"]["total"], 1);

    // The resolved id is never reused.
    assert_eq!(p.add("New", "設計判断"), "R-003");
}

#[test]
fn rejected_calls_change_neither_file() {
    let p = Project::new();
    let id = p.add("A", "X");
    let before = std::fs::read(p.file("review_board.md")).unwrap();
    for args in [
        json!({ "id": "R-404", "decision": "x" }),
        json!({ "id": id }),
        json!({ "decision": "x" }),
        json!({ "id": id, "decision": "   " }),
    ] {
        assert!(
            is_error(&p.call("handoff_review_resolve", args.clone())),
            "{args}"
        );
    }
    assert_eq!(std::fs::read(p.file("review_board.md")).unwrap(), before);
    assert!(!p.file("review_board_decisions.md").exists());
}

#[test]
fn other_review_tools_never_create_the_decisions_file() {
    let p = Project::new();
    p.call("handoff_review_init", json!({}));
    p.add("A", "X");
    p.call("handoff_review_list", json!({}));
    p.call("handoff_load_context", json!({}));
    assert!(!p.file("review_board_decisions.md").exists());
}

#[test]
fn concurrent_resolves_of_one_id_leave_one_entry() {
    let p = Project::new();
    let id = p.add("A", "X");
    let results: Vec<Value> = std::thread::scope(|s| {
        let hs: Vec<_> = (0..4).map(|_| s.spawn(|| p.resolve(&id, "ok"))).collect();
        hs.into_iter().map(|h| h.join().unwrap()).collect()
    });
    assert_eq!(results.iter().filter(|r| !is_error(r)).count(), 1);
    let dec = std::fs::read_to_string(p.file("review_board_decisions.md")).unwrap();
    assert_eq!(dec.matches("original_id: R-001").count(), 1);
}

#[test]
fn the_tool_is_listed_with_id_and_decision_required() {
    let resp: Value = serde_json::from_str(
        &handoff_mcp::mcp::protocol::process_line(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}).to_string(),
        )
        .unwrap(),
    )
    .unwrap();
    let tool = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "handoff_review_resolve")
        .expect("handoff_review_resolve must be listed");
    assert_eq!(tool["inputSchema"]["required"], json!(["id", "decision"]));
}

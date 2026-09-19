//! Integration tests for `handoff_doc_req_impact` — reverse-trace impact
//! analysis (requirements-traceability P2 §5.2,
//! `.handoff/docs/_doc.req-traceability-mcp-plan.md`), exercised end-to-end
//! through the JSON-RPC `process_line` entry point — the same path the MCP
//! server runs in production (mirrors `tests/tool_doc_req_status.rs`).

use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn unique_slug(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{n}")
}

fn setup_project() -> (tempfile::TempDir, std::path::PathBuf) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let req = json!({
        "jsonrpc": "2.0", "id": 0,
        "method": "tools/call",
        "params": {
            "name": "handoff_init",
            "arguments": {
                "project_dir": dir.to_string_lossy(),
                "project_name": "doc-req-impact-test"
            }
        }
    });
    send(&req.to_string()).unwrap();
    (tmp, dir)
}

fn call(dir: &std::path::Path, name: &str, mut args: Value) -> Value {
    args["project_dir"] = json!(dir.to_string_lossy());
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    send(&req.to_string()).unwrap()
}

fn payload(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    serde_json::from_str(text).expect("payload should be a JSON string")
}

fn payload_text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

/// Saves a document (optionally with `scope_paths`), generates its
/// verification matrix, adds one sub_item (auto-deriving a `stable_id`),
/// then optionally sets impl_refs/test_refs on it.
#[allow(clippy::too_many_arguments)]
fn make_req_doc(
    dir: &std::path::Path,
    slug: &str,
    description: &str,
    scope_paths: &[&str],
    impl_ref_path: Option<&str>,
    test_ref_path: Option<&str>,
) -> String {
    let body = "Intro.\n\n## 1. 要件\n\nBody.\n";
    let resp = call(
        dir,
        "handoff_doc_save",
        json!({
            "slug": slug,
            "title": format!("Doc {slug}"),
            "body": body,
            "doc_type": "spec",
            "tags": ["requirements"],
            "scope_paths": scope_paths,
        }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", payload_text(&resp));
    let doc_id = payload(&resp)["doc_id"].as_str().unwrap().to_string();

    let resp = call(
        dir,
        "handoff_doc_verify",
        json!({ "doc_id": doc_id, "action": "generate" }),
    );
    assert!(!is_error(&resp), "generate failed: {}", payload_text(&resp));

    let resp = call(
        dir,
        "handoff_doc_verify",
        json!({
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 1,
            "description": description,
        }),
    );
    assert!(!is_error(&resp), "add_item failed: {}", payload_text(&resp));

    if impl_ref_path.is_some() || test_ref_path.is_some() {
        let mut args = json!({
            "doc_id": doc_id,
            "action": "set_refs",
            "fragment_seq": 1,
            "sub_item_index": 0,
        });
        if let Some(p) = impl_ref_path {
            args["impl_refs"] = json!([{ "path": p }]);
        }
        if let Some(p) = test_ref_path {
            args["test_refs"] = json!([{ "path": p }]);
        }
        let resp = call(dir, "handoff_doc_verify", args);
        assert!(!is_error(&resp), "set_refs failed: {}", payload_text(&resp));
    }

    doc_id
}

// ---------------------------------------------------------------------
// impl_refs match
// ---------------------------------------------------------------------

#[test]
fn req_impact_file_matches_impl_ref() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        &[],
        Some("src/pcb/board.rs"),
        None,
    );
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        "差動ペア間隔",
        &[],
        Some("src/pcb/routing.rs"),
        None,
    );

    let resp = call(
        &dir,
        "handoff_doc_req_impact",
        json!({ "file": "src/pcb/board.rs" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(
        p["total"], 1,
        "only the board.rs sub_item should match: {p}"
    );
    assert_eq!(p["affected_requirements"][0]["stable_id"], "C01-1");
    assert_eq!(p["affected_requirements"][0]["match_type"], "impl_ref");
}

// ---------------------------------------------------------------------
// test_refs match
// ---------------------------------------------------------------------

#[test]
fn req_impact_file_matches_test_ref() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        &[],
        None,
        Some("tests/board.rs"),
    );

    let resp = call(
        &dir,
        "handoff_doc_req_impact",
        json!({ "file": "tests/board.rs" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 1);
    assert_eq!(p["affected_requirements"][0]["match_type"], "test_ref");
}

// ---------------------------------------------------------------------
// scope_paths indirect match
// ---------------------------------------------------------------------

#[test]
fn req_impact_file_matches_scope_paths_indirectly() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c07"),
        "オートルーター",
        &["crates/aelm-pcb/src/"],
        None,
        None,
    );

    let resp = call(
        &dir,
        "handoff_doc_req_impact",
        json!({ "file": "crates/aelm-pcb/src/autoroute.rs" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 1, "scope_paths prefix match should count: {p}");
    assert_eq!(p["affected_requirements"][0]["match_type"], "scope_path");
}

// ---------------------------------------------------------------------
// no match
// ---------------------------------------------------------------------

#[test]
fn req_impact_no_match_returns_empty() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        &[],
        Some("src/pcb/board.rs"),
        None,
    );

    let resp = call(
        &dir,
        "handoff_doc_req_impact",
        json!({ "file": "src/unrelated/file.rs" }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(p["total"], 0);
    assert_eq!(p["affected_requirements"].as_array().unwrap().len(), 0);
}

// ---------------------------------------------------------------------
// missing both file and git_diff -> error
// ---------------------------------------------------------------------

#[test]
fn req_impact_requires_file_or_git_diff() {
    let (_tmp, dir) = setup_project();
    let resp = call(&dir, "handoff_doc_req_impact", json!({}));
    assert!(
        is_error(&resp),
        "expected an error when neither file nor git_diff is given"
    );
}

// ---------------------------------------------------------------------
// file takes priority over git_diff when both given
// ---------------------------------------------------------------------

#[test]
fn req_impact_file_takes_priority_over_git_diff() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        &[],
        Some("src/pcb/board.rs"),
        None,
    );

    // git_diff=true would fail/be empty (no git repo here), but `file` is
    // explicitly given, so it should be used and must not error out.
    let resp = call(
        &dir,
        "handoff_doc_req_impact",
        json!({ "file": "src/pcb/board.rs", "git_diff": true }),
    );
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);
    assert_eq!(p["total"], 1);
}

// ---------------------------------------------------------------------
// git_diff=true auto-detects changed files
// ---------------------------------------------------------------------

#[test]
fn req_impact_git_diff_detects_changed_files() {
    let (_tmp, dir) = setup_project();
    make_req_doc(
        &dir,
        &unique_slug("req-c01"),
        "矩形外形",
        &[],
        Some("src/pcb/board.rs"),
        None,
    );

    // Turn the project dir into a git repo with one committed file, then
    // modify it so `git diff HEAD --name-only` reports it as changed.
    let run = |args: &[&str]| {
        let status = std::process::Command::new("git")
            .args(args)
            .current_dir(&dir)
            .status()
            .expect("git command should run");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);

    let src_dir = dir.join("src/pcb");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("board.rs"), "fn a() {}\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "init"]);

    std::fs::write(src_dir.join("board.rs"), "fn a() { /* changed */ }\n").unwrap();

    let resp = call(&dir, "handoff_doc_req_impact", json!({ "git_diff": true }));
    assert!(!is_error(&resp), "{}", payload_text(&resp));
    let p = payload(&resp);

    assert_eq!(
        p["total"], 1,
        "git diff should surface src/pcb/board.rs as changed: {p}"
    );
    assert_eq!(p["affected_requirements"][0]["match_type"], "impl_ref");
}

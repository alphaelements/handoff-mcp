//! E2E: t341 — `doc_save` must fail loudly when an existing document's body
//! file has vanished (instead of masking it as an empty body), and
//! t360.20.21 — an unparsable `config.toml` surfaces as a `warnings[]` entry
//! on trace tools / `doc_save` while a missing one stays silent.

use serde_json::{json, Value};
use tempfile::TempDir;

fn call_tool(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).unwrap();
    serde_json::from_str(&out).expect("response should be valid JSON")
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .unwrap_or("")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    let resp = call_tool(
        "handoff_init",
        json!({ "project_dir": &pd, "project_name": "test" }),
    );
    assert!(!is_error(&resp), "init failed: {}", text(&resp));
    (dir, pd)
}

fn save_doc(pd: &str, slug: &str, body: &str) -> String {
    let resp = call_tool(
        "handoff_doc_save",
        json!({ "project_dir": pd, "slug": slug, "title": "T", "body": body }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", text(&resp));
    let v: Value = serde_json::from_str(&text(&resp)).unwrap();
    v["doc_id"].as_str().unwrap().to_string()
}

fn body_path(pd: &str, slug: &str) -> std::path::PathBuf {
    std::path::Path::new(pd)
        .join(".handoff/docs")
        .join(format!("_doc.{slug}.md"))
}

#[test]
fn metadata_only_save_fails_when_document_file_is_missing() {
    let (_d, pd) = setup();
    let id = save_doc(&pd, "gone", "hello");
    std::fs::remove_file(body_path(&pd, "gone")).unwrap();
    let resp = call_tool(
        "handoff_doc_save",
        json!({ "project_dir": &pd, "doc_id": id, "title": "T2" }),
    );
    // The body file carries the frontmatter too, so a vanished file means the
    // document no longer resolves at all: the save must fail, never succeed
    // with an empty body, and never resurrect the file.
    assert!(is_error(&resp), "must fail: {}", text(&resp));
    assert!(!body_path(&pd, "gone").exists());
}

#[test]
fn append_body_save_fails_when_document_file_is_missing() {
    let (_d, pd) = setup();
    let id = save_doc(&pd, "gone2", "hello");
    std::fs::remove_file(body_path(&pd, "gone2")).unwrap();
    let resp = call_tool(
        "handoff_doc_save",
        json!({ "project_dir": &pd, "doc_id": id, "append_body": "more" }),
    );
    assert!(is_error(&resp), "must fail: {}", text(&resp));
    assert!(!body_path(&pd, "gone2").exists());
}

#[test]
fn metadata_only_and_append_still_work_with_body_present() {
    let (_d, pd) = setup();
    let id = save_doc(&pd, "ok", "hello");
    let resp = call_tool(
        "handoff_doc_save",
        json!({ "project_dir": &pd, "doc_id": &id, "title": "T2" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    let resp = call_tool(
        "handoff_doc_save",
        json!({ "project_dir": &pd, "doc_id": &id, "append_body": "more" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    let on_disk = std::fs::read_to_string(body_path(&pd, "ok")).unwrap();
    assert!(on_disk.contains("hello\n\nmore"), "{on_disk}");
}

#[test]
fn empty_body_document_can_still_be_appended_to() {
    let (_d, pd) = setup();
    let id = save_doc(&pd, "empty", "");
    let resp = call_tool(
        "handoff_doc_save",
        json!({ "project_dir": &pd, "doc_id": &id, "append_body": "more" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
}

fn warnings_of(resp: &Value) -> Vec<String> {
    let v: Value = serde_json::from_str(&text(resp)).unwrap();
    v["warnings"]
        .as_array()
        .map(|a| a.iter().map(|w| w.to_string()).collect())
        .unwrap_or_default()
}

fn config_warning_count(resp: &Value) -> usize {
    warnings_of(resp)
        .iter()
        .filter(|w| w.contains("config.toml could not be loaded"))
        .count()
}

fn break_config(pd: &str) {
    std::fs::write(
        std::path::Path::new(pd).join(".handoff/config.toml"),
        "this is = = not toml [",
    )
    .unwrap();
}

#[test]
fn trace_report_warns_on_unparsable_config() {
    let (_d, pd) = setup();
    break_config(&pd);
    let resp = call_tool("handoff_trace_report", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 1, "{}", text(&resp));
}

#[test]
fn trace_report_is_silent_when_config_is_missing() {
    let (_d, pd) = setup();
    std::fs::remove_file(std::path::Path::new(&pd).join(".handoff/config.toml")).unwrap();
    let resp = call_tool("handoff_trace_report", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 0, "{}", text(&resp));
}

#[test]
fn trace_report_is_silent_when_config_is_valid() {
    let (_d, pd) = setup();
    let resp = call_tool("handoff_trace_report", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 0, "{}", text(&resp));
}

fn save_layer_doc(pd: &str) {
    let resp = call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": pd, "slug": "spec", "title": "S", "layer": "basic_spec",
            "body": "### SPEC-001 Lockout\n\nLock after 5 failures.\n"
        }),
    );
    assert!(!is_error(&resp), "doc_save failed: {}", text(&resp));
}

#[test]
fn trace_slice_warns_on_unparsable_config() {
    let (_d, pd) = setup();
    save_layer_doc(&pd);
    break_config(&pd);
    let resp = call_tool(
        "handoff_trace_slice",
        json!({ "project_dir": &pd, "item": "SPEC-001" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 1, "{}", text(&resp));
}

#[test]
fn trace_update_warns_on_unparsable_config() {
    let (_d, pd) = setup();
    save_layer_doc(&pd);
    break_config(&pd);
    let resp = call_tool(
        "handoff_trace_update",
        json!({
            "project_dir": &pd,
            "dry_run": true,
            "ops": [{
                "op": "upsert_item", "doc": "spec", "id": "SPEC-002",
                "title": "Timeout", "statement": "Idle sessions expire."
            }],
        }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 1, "{}", text(&resp));
}

#[test]
fn trace_update_applied_upsert_warns_exactly_once_on_unparsable_config() {
    // The apply phase re-reads the config and re-syncs the layer document;
    // each of those sites reports the same problem, which must be deduped.
    let (_d, pd) = setup();
    save_layer_doc(&pd);
    break_config(&pd);
    let resp = call_tool(
        "handoff_trace_update",
        json!({
            "project_dir": &pd,
            "ops": [{
                "op": "upsert_item", "doc": "spec", "id": "SPEC-002",
                "title": "Timeout", "statement": "Idle sessions expire."
            }],
        }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 1, "{}", text(&resp));
}

#[test]
fn trace_update_is_silent_when_config_is_valid() {
    let (_d, pd) = setup();
    save_layer_doc(&pd);
    let resp = call_tool(
        "handoff_trace_update",
        json!({
            "project_dir": &pd,
            "dry_run": true,
            "ops": [{
                "op": "upsert_item", "doc": "spec", "id": "SPEC-002",
                "title": "Timeout", "statement": "Idle sessions expire."
            }],
        }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 0, "{}", text(&resp));
}

#[test]
fn trace_propose_warns_on_unparsable_config() {
    let (_d, pd) = setup();
    save_layer_doc(&pd);
    break_config(&pd);
    let resp = call_tool(
        "handoff_trace_propose",
        json!({ "project_dir": &pd, "title": "Account lockout after failed logins" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 1, "{}", text(&resp));
}

#[test]
fn trace_suspect_list_warns_on_unparsable_config() {
    let (_d, pd) = setup();
    save_layer_doc(&pd);
    break_config(&pd);
    let resp = call_tool("handoff_trace_suspect", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 1, "{}", text(&resp));
}

#[test]
fn trace_suspect_list_is_silent_when_config_is_valid() {
    let (_d, pd) = setup();
    save_layer_doc(&pd);
    let resp = call_tool("handoff_trace_suspect", json!({ "project_dir": &pd }));
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 0, "{}", text(&resp));
}

#[test]
fn plain_doc_save_warns_once_on_unparsable_config() {
    // Non-layer document: neither layer-tree validation nor layer-item sync
    // runs, so this pins doc_save's own config warning.
    let (_d, pd) = setup();
    break_config(&pd);
    let resp = call_tool(
        "handoff_doc_save",
        json!({ "project_dir": &pd, "slug": "plain", "title": "P", "body": "hello" }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 1, "{}", text(&resp));
}

#[test]
fn layer_doc_save_warns_once_on_unparsable_config() {
    let (_d, pd) = setup();
    break_config(&pd);
    let resp = call_tool(
        "handoff_doc_save",
        json!({
            "project_dir": &pd, "slug": "spec", "title": "S", "layer": "basic_spec",
            "body": "### SPEC-001 Lockout\n\nLock after 5 failures.\n"
        }),
    );
    assert!(!is_error(&resp), "{}", text(&resp));
    assert_eq!(config_warning_count(&resp), 1, "{}", text(&resp));
}

//! `[settings] schedule_stale_notice` / `layer_missing_notice` and the
//! dangling-dependency warning text (t439.15, FR-V0473-018).
//!
//! Both settings default to `true` (today's behaviour); setting one to `false`
//! only silences its own advisory.

use serde_json::{json, Value};
use tempfile::TempDir;

fn send(input: &str) -> Value {
    let result = handoff_mcp::mcp::protocol::process_line(input).expect("response");
    serde_json::from_str(&result).expect("response should be valid JSON")
}

fn call(dir: &TempDir, name: &str, mut arguments: Value) -> Value {
    arguments["project_dir"] = json!(dir.path().to_string_lossy());
    send(
        &json!({
            "jsonrpc": "2.0", "id": 1, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        })
        .to_string(),
    )
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

fn ok(dir: &TempDir, name: &str, arguments: Value) -> String {
    let resp = call(dir, name, arguments);
    assert!(!is_error(&resp), "{name} failed: {}", text(&resp));
    text(&resp)
}

fn project() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    ok(
        &dir,
        "handoff_init",
        json!({ "project_name": "noise", "description": "d" }),
    );
    dir
}

fn set(dir: &TempDir, key: &str, value: Value) -> Value {
    call(
        dir,
        "handoff_update_config",
        json!({ "updates": { key: value } }),
    )
}

/// Creates the task, then re-sends its dependencies as an update (the
/// `schedule_stale` signal is for changes to an existing task); returns the
/// update's response.
fn create_task(dir: &TempDir, id: &str, deps: Value) -> String {
    ok(
        dir,
        "handoff_update_task",
        json!({ "task": { "id": id, "title": format!("Task {id}"), "status": "todo" } }),
    );
    ok(
        dir,
        "handoff_update_task",
        json!({ "task": { "id": id, "dependencies": deps } }),
    )
}

// ---- schedule_stale_notice -------------------------------------------------

#[test]
fn update_task_marks_the_schedule_stale_by_default() {
    let dir = project();
    create_task(&dir, "t1", json!([]));
    let msg = create_task(&dir, "t2", json!(["t1"]));
    assert!(msg.contains("schedule_stale: true"), "{msg}");
}

#[test]
fn update_task_omits_schedule_stale_when_the_notice_is_off() {
    let dir = project();
    assert!(!is_error(&set(
        &dir,
        "settings.schedule_stale_notice",
        json!(false)
    )));
    create_task(&dir, "t1", json!([]));
    let msg = create_task(&dir, "t2", json!(["t1"]));
    assert!(!msg.contains("schedule_stale"), "{msg}");
}

#[test]
fn update_task_keeps_schedule_stale_when_the_notice_is_explicitly_on() {
    let dir = project();
    assert!(!is_error(&set(
        &dir,
        "settings.schedule_stale_notice",
        json!(true)
    )));
    create_task(&dir, "t1", json!([]));
    let msg = create_task(&dir, "t2", json!(["t1"]));
    assert!(msg.contains("schedule_stale: true"), "{msg}");
}

#[test]
fn the_notice_off_does_not_hide_other_advisories() {
    let dir = project();
    set(&dir, "settings.schedule_stale_notice", json!(false));
    let msg = create_task(&dir, "t1", json!(["t999"]));
    assert!(!msg.contains("schedule_stale"), "{msg}");
    assert!(msg.contains("DANGLING_DEPENDENCY"), "{msg}");
}

#[test]
fn doc_save_reports_schedule_stale_false_when_the_notice_is_off() {
    let dir = project();
    create_task(&dir, "t1", json!([]));
    let save = |slug: &str| -> Value {
        serde_json::from_str(&ok(
            &dir,
            "handoff_doc_save",
            json!({ "slug": slug, "title": slug, "body": format!("# {slug}\n\nbody\n"),
                    "task_ids": ["t1"] }),
        ))
        .unwrap()
    };
    assert_eq!(save("on")["schedule_stale"], json!(true));

    set(&dir, "settings.schedule_stale_notice", json!(false));
    assert_eq!(save("off")["schedule_stale"], json!(false));
}

#[test]
fn update_config_rejects_a_non_boolean_notice() {
    let dir = project();
    for key in [
        "settings.schedule_stale_notice",
        "settings.layer_missing_notice",
    ] {
        let resp = set(&dir, key, json!("no"));
        assert!(is_error(&resp), "{key} accepted a string");
        assert!(
            text(&resp).contains(&format!("{key} must be a boolean")),
            "{}",
            text(&resp)
        );
    }
    let raw = std::fs::read_to_string(dir.path().join(".handoff/config.toml")).unwrap();
    assert!(
        raw.contains("schedule_stale_notice = true") && !raw.contains("\"no\""),
        "a rejected value must not be written: {raw}"
    );
}

#[test]
fn get_config_reports_the_notice_settings() {
    let dir = project();
    let cfg: Value = serde_json::from_str(&ok(&dir, "handoff_get_config", json!({}))).unwrap();
    assert_eq!(cfg["settings"]["schedule_stale_notice"], json!(true));
    assert_eq!(cfg["settings"]["layer_missing_notice"], json!(true));
}

// ---- layer_missing_notice (DIAG-D001) --------------------------------------

fn warning_codes(saved: &str) -> Vec<String> {
    let v: Value = serde_json::from_str(saved).unwrap();
    v["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str().map(str::to_string))
        .collect()
}

fn save_spec(dir: &TempDir, slug: &str, extra: Value) -> String {
    let mut args = json!({ "slug": slug, "title": slug, "doc_type": "spec",
                           "body": format!("# {slug}\n\nbody\n") });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    ok(dir, "handoff_doc_save", args)
}

#[test]
fn layerless_spec_warns_diag_d001_by_default() {
    let dir = project();
    let saved = save_spec(&dir, "plain", json!({}));
    assert!(
        warning_codes(&saved).contains(&"DIAG-D001".to_string()),
        "{saved}"
    );
}

#[test]
fn layerless_spec_does_not_warn_when_the_notice_is_off() {
    let dir = project();
    set(&dir, "settings.layer_missing_notice", json!(false));
    let saved = save_spec(&dir, "plain", json!({}));
    assert!(
        !warning_codes(&saved).contains(&"DIAG-D001".to_string()),
        "{saved}"
    );
    // The document itself is saved as before.
    let v: Value = serde_json::from_str(&saved).unwrap();
    assert_eq!(v["slug"], "plain");
}

#[test]
fn explicit_layer_save_is_unaffected_by_the_notice_setting() {
    let dir = project();
    let on = save_spec(&dir, "with-layer-on", json!({ "layer": "requirement" }));
    set(&dir, "settings.layer_missing_notice", json!(false));
    let off = save_spec(&dir, "with-layer-off", json!({ "layer": "requirement" }));
    assert_eq!(warning_codes(&on), warning_codes(&off));
    assert!(!warning_codes(&off).contains(&"DIAG-D001".to_string()));
}

// ---- dangling dependency text ----------------------------------------------

#[test]
fn dangling_dependency_warning_mentions_a_possible_cycle() {
    let dir = project();
    let msg = create_task(&dir, "t1", json!(["t999"]));
    assert!(
        msg.contains(
            "Dependency 't999' does not match any existing task. Check the task ID spelling \
             or create the task first."
        ),
        "{msg}"
    );
    assert!(msg.contains("may become circular"), "{msg}");
    // The task is still created.
    ok(&dir, "handoff_get_task", json!({ "task_id": "t1" }));
}

#[test]
fn existing_dependency_gets_no_dangling_warning() {
    let dir = project();
    create_task(&dir, "t1", json!([]));
    let msg = create_task(&dir, "t2", json!(["t1"]));
    assert!(!msg.contains("DANGLING_DEPENDENCY"), "{msg}");
}

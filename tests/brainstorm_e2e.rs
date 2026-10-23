//! Real-binary E2E tests for the `handoff_brainstorm_*` tools
//! (wiki/360-brainstorm-feature-spec.md §4-§6, §10).
//!
//! Every scenario drives the actual `handoff-mcp` binary over line-delimited
//! JSON-RPC on stdio. Each [`Session`] is one server process, so dropping a
//! session and opening another proves state survives a process restart.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

const BINARY: &str = env!("CARGO_BIN_EXE_handoff-mcp");

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Session {
    fn spawn() -> Self {
        let mut child = Command::new(BINARY)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn handoff-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Session {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    /// Call a tool; returns `(is_error, text)`.
    fn call(&mut self, project: &Path, name: &str, mut arguments: Value) -> (bool, String) {
        arguments["project_dir"] = json!(project.to_string_lossy());
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments }
        });
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let resp: Value = serde_json::from_str(line.trim()).expect("response is JSON");
        assert_eq!(resp["id"], id, "response id mismatch: {resp}");
        let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        (is_error, text)
    }

    /// Call a tool that must succeed and return its JSON body.
    fn ok(&mut self, project: &Path, name: &str, arguments: Value) -> Value {
        let (is_error, text) = self.call(project, name, arguments);
        assert!(!is_error, "{name} failed: {text}");
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} not JSON ({e}): {text}"))
    }

    /// Call a tool that must fail and return the error text.
    fn err(&mut self, project: &Path, name: &str, arguments: Value) -> String {
        let (is_error, text) = self.call(project, name, arguments);
        assert!(is_error, "{name} unexpectedly succeeded: {text}");
        text
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn init_project(s: &mut Session) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let (is_error, text) = s.call(
        dir.path(),
        "handoff_init",
        json!({ "project_name": "brainstorm-e2e" }),
    );
    assert!(!is_error, "init failed: {text}");
    dir
}

fn doc_body(s: &mut Session, project: &Path, doc_id: &str) -> String {
    let doc = s.ok(project, "handoff_doc_get", json!({ "doc_id": doc_id }));
    doc["body"]
        .as_str()
        .unwrap_or_else(|| panic!("doc_get has no body: {doc}"))
        .to_string()
}

fn start(s: &mut Session, project: &Path) -> Value {
    s.ok(
        project,
        "handoff_brainstorm_start",
        json!({
            "topic": "IoT 温度監視システム",
            "initial_context": "工場の冷蔵庫の温度を遠隔で見たい"
        }),
    )
}

#[test]
fn tools_are_listed() {
    let mut s = Session::spawn();
    writeln!(
        s.stdin,
        r#"{{"jsonrpc":"2.0","id":99,"method":"tools/list","params":{{}}}}"#
    )
    .unwrap();
    let mut line = String::new();
    s.stdout.read_line(&mut line).unwrap();
    let resp: Value = serde_json::from_str(line.trim()).unwrap();
    let names: Vec<&str> = resp["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    for tool in [
        "handoff_brainstorm_start",
        "handoff_brainstorm_turn",
        "handoff_brainstorm_status",
        "handoff_brainstorm_finalize",
    ] {
        assert!(names.contains(&tool), "missing {tool}");
    }
}

#[test]
fn full_flow_start_turn_x3_status_finalize() {
    let mut s = Session::spawn();
    let project = init_project(&mut s);
    let p = project.path();

    // start
    let started = start(&mut s, p);
    let id = started["brainstorm_id"].as_str().unwrap().to_string();
    assert!(id.starts_with("bs-"), "{started}");
    let log_id = started["log_doc_id"].as_str().unwrap().to_string();
    let report_id = started["report_doc_id"].as_str().unwrap().to_string();
    assert_ne!(log_id, report_id);
    for k in [
        "vision",
        "motivation",
        "target-users",
        "core-value",
        "scope",
    ] {
        assert_eq!(started["coverage"][k], "not_started", "{started}");
    }
    assert!(p
        .join(".handoff/brainstorm")
        .join(format!("{id}.json"))
        .is_file());
    assert!(doc_body(&mut s, p, &log_id).contains("IoT 温度監視システム"));
    assert!(doc_body(&mut s, p, &report_id).contains("エグゼクティブサマリー"));

    // turn 1: discussion only -> in_progress
    let t1 = s.ok(
        p,
        "handoff_brainstorm_turn",
        json!({
            "brainstorm_id": id, "topic_tag": "vision",
            "summary": "冷蔵庫の温度逸脱を早期に検知したい",
            "open_questions": ["対象は何台か"]
        }),
    );
    assert_eq!(t1["turn_number"], 1);
    assert_eq!(t1["coverage"]["vision"], "in_progress");
    assert!(t1["uncovered_topics"]
        .as_array()
        .unwrap()
        .contains(&json!("vision")));

    // turn 2: decisions -> covered, mirrored into the report
    let t2 = s.ok(
        p,
        "handoff_brainstorm_turn",
        json!({
            "brainstorm_id": id, "topic_tag": "vision",
            "summary": "ビジョンを確定",
            "decisions": ["温度逸脱を5分以内に通知する"]
        }),
    );
    assert_eq!(t2["turn_number"], 2);
    assert_eq!(t2["coverage"]["vision"], "covered");
    assert!(!t2["uncovered_topics"]
        .as_array()
        .unwrap()
        .contains(&json!("vision")));

    // turn 3: optional topic is added dynamically
    let t3 = s.ok(
        p,
        "handoff_brainstorm_turn",
        json!({
            "brainstorm_id": id, "topic_tag": "hardware",
            "summary": "センサーは既製品を使う"
        }),
    );
    assert_eq!(t3["turn_number"], 3);
    assert_eq!(t3["coverage"]["hardware"], "in_progress");

    let log = doc_body(&mut s, p, &log_id);
    assert!(log.contains("Turn 1"), "{log}");
    assert!(log.contains("Turn 2"), "{log}");
    assert!(log.contains("Turn 3"), "{log}");
    assert!(log.contains("[hardware]"), "{log}");
    assert!(log.contains("温度逸脱を5分以内に通知する"), "{log}");
    assert!(log.contains("対象は何台か"), "{log}");
    let report = doc_body(&mut s, p, &report_id);
    assert!(report.contains("温度逸脱を5分以内に通知する"), "{report}");

    // status
    let st = s.ok(
        p,
        "handoff_brainstorm_status",
        json!({ "brainstorm_id": id }),
    );
    assert_eq!(st["phase"], "dialogue");
    assert_eq!(st["turn_count"], 3);
    assert_eq!(st["coverage"]["vision"], "covered");
    assert_eq!(st["decisions"], json!(["温度逸脱を5分以内に通知する"]));
    assert_eq!(st["open_questions"], json!(["対象は何台か"]));
    assert!(st["topics"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["topic"] == "hardware" && t["status"] == "in_progress"));

    // finalize
    let fin = s.ok(
        p,
        "handoff_brainstorm_finalize",
        json!({
            "brainstorm_id": id,
            "executive_summary": "冷蔵庫の温度を遠隔監視するシステム",
            "scope_in": ["温度ログ", "アラート通知"],
            "scope_out": ["発注自動化"],
            "undecided": ["クラウド基盤"],
            "priorities": ["アラート通知", "温度ログ"],
            "research_topics": ["食品衛生法の温度管理基準"],
            "requirement_hints": ["通知遅延は5分以内"]
        }),
    );
    assert_eq!(fin["status"], "finalized");
    assert_eq!(fin["report_doc_id"], report_id);
    assert_eq!(fin["log_doc_id"], log_id);

    let report = doc_body(&mut s, p, &report_id);
    for needle in [
        "冷蔵庫の温度を遠隔監視するシステム",
        "温度ログ",
        "発注自動化",
        "クラウド基盤",
        "食品衛生法の温度管理基準",
        "通知遅延は5分以内",
    ] {
        assert!(report.contains(needle), "report lacks {needle}: {report}");
    }
    let log = doc_body(&mut s, p, &log_id);
    assert!(log.contains("完了"), "{log}");

    let st = s.ok(
        p,
        "handoff_brainstorm_status",
        json!({ "brainstorm_id": id }),
    );
    assert_eq!(st["phase"], "finalized");

    // a finalized session is closed
    let e = s.err(
        p,
        "handoff_brainstorm_turn",
        json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "x" }),
    );
    assert!(e.contains("finalized"), "{e}");
}

#[test]
fn status_is_restored_after_process_restart() {
    let project;
    let id;
    {
        let mut s = Session::spawn();
        project = init_project(&mut s);
        let started = start(&mut s, project.path());
        id = started["brainstorm_id"].as_str().unwrap().to_string();
        s.ok(
            project.path(),
            "handoff_brainstorm_turn",
            json!({
                "brainstorm_id": id, "topic_tag": "scope",
                "summary": "スコープ方向性", "decisions": ["温度のみ扱う"]
            }),
        );
    } // server process killed here

    let mut s2 = Session::spawn();
    let st = s2.ok(
        project.path(),
        "handoff_brainstorm_status",
        json!({ "brainstorm_id": id }),
    );
    assert_eq!(st["turn_count"], 1);
    assert_eq!(st["coverage"]["scope"], "covered");
    assert_eq!(st["decisions"], json!(["温度のみ扱う"]));

    // ...and the session can continue where it left off
    let t = s2.ok(
        project.path(),
        "handoff_brainstorm_turn",
        json!({ "brainstorm_id": id, "topic_tag": "vision", "summary": "続き" }),
    );
    assert_eq!(t["turn_number"], 2);
}

#[test]
fn rejects_bad_input() {
    let mut s = Session::spawn();
    let project = init_project(&mut s);
    let p = project.path();

    let e = s.err(p, "handoff_brainstorm_start", json!({}));
    assert!(e.contains("topic"), "{e}");

    let e = s.err(
        p,
        "handoff_brainstorm_status",
        json!({ "brainstorm_id": "bs-does-not-exist" }),
    );
    assert!(e.contains("not found"), "{e}");

    // path traversal in the id must never reach the filesystem
    let e = s.err(
        p,
        "handoff_brainstorm_status",
        json!({ "brainstorm_id": "../../config" }),
    );
    assert!(e.contains("brainstorm_id"), "{e}");

    let started = start(&mut s, p);
    let id = started["brainstorm_id"].as_str().unwrap();
    let e = s.err(
        p,
        "handoff_brainstorm_turn",
        json!({ "brainstorm_id": id, "topic_tag": "vision" }),
    );
    assert!(e.contains("summary"), "{e}");
    let e = s.err(
        p,
        "handoff_brainstorm_finalize",
        json!({ "brainstorm_id": id, "executive_summary": "x" }),
    );
    assert!(e.contains("scope_in"), "{e}");
}

/// Appends the `brainstorm` origin layer (wiki/360 §9.1) and a profile that
/// includes it to the project's `config.toml`.
fn declare_brainstorm_layer(project: &Path) {
    let path = project.join(".handoff/config.toml");
    let mut config = std::fs::read_to_string(&path).expect("read config.toml");
    config.push_str(
        r#"
[trace]
profile = "with-brainstorm"

[trace.profiles.with-brainstorm]
extends = "minimal"
layers = ["brainstorm", "requirement", "acceptance"]

[[trace.layer]]
id = "brainstorm"
name = "ブレインストーミング"
side = "left"
position = 0
level = 0
verification = false
id_prefixes = ["BS"]
"#,
    );
    std::fs::write(&path, config).expect("write config.toml");
}

fn finalize_scope(s: &mut Session, project: &Path, id: &str) -> Value {
    s.ok(
        project,
        "handoff_brainstorm_finalize",
        json!({
            "brainstorm_id": id,
            "executive_summary": "冷蔵庫の温度を遠隔監視する",
            "scope_in": ["温度ログ", "アラート通知"],
            "priorities": ["アラート通知"],
            "requirement_hints": ["通知遅延は5分以内"]
        }),
    )
}

/// wiki/360 §9.2-§9.3 seam: the report `handoff_brainstorm_finalize` itself
/// produces (no hand-written BS doc) carries `layer: brainstorm` and real
/// `BS-*` items, a requirement doc can `refines` one, and `trace_report`
/// shows the brainstorm -> requirement chain.
#[test]
fn finalize_output_is_traceable_from_a_requirement_via_refines() {
    let mut s = Session::spawn();
    let project = init_project(&mut s);
    let p = project.path();
    declare_brainstorm_layer(p);

    let started = start(&mut s, p);
    let id = started["brainstorm_id"].as_str().unwrap().to_string();
    let report_id = started["report_doc_id"].as_str().unwrap().to_string();
    s.ok(
        p,
        "handoff_brainstorm_turn",
        json!({ "brainstorm_id": id, "topic_tag": "scope", "summary": "s", "decisions": ["温度のみ"] }),
    );
    let fin = finalize_scope(&mut s, p, &id);
    assert_eq!(fin["layer"], "brainstorm", "{fin}");
    assert_eq!(
        fin["item_ids"],
        json!([
            "BS-SCOPE-001",
            "BS-SCOPE-002",
            "BS-PRIORITY-001",
            "BS-HINT-001"
        ])
    );

    let doc = s.ok(p, "handoff_doc_get", json!({ "doc_id": report_id }));
    assert_eq!(doc["layer"], "brainstorm", "{doc}");

    s.ok(
        p,
        "handoff_doc_save",
        json!({
            "slug": "requirements-from-brainstorm",
            "title": "Requirements",
            "doc_type": "spec",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 温度ログ\n\n- refines: BS-SCOPE-001\n\n温度を記録する。\n"
        }),
    );

    let report = s.ok(p, "handoff_trace_report", json!({ "include_items": true }));
    assert_eq!(
        report["trace_layers"]["in_use"],
        json!(["brainstorm", "requirement", "acceptance"]),
        "{report}"
    );
    let items = report["items"].as_array().expect("items");
    let find = |item_id: &str| {
        items
            .iter()
            .find(|i| i["id"] == item_id)
            .unwrap_or_else(|| panic!("{item_id} missing: {report}"))
    };
    assert_eq!(find("BS-SCOPE-001")["layer"], "brainstorm");
    assert_eq!(find("BS-HINT-001")["layer"], "brainstorm");
    assert_eq!(find("REQ-001")["refines"], json!(["BS-SCOPE-001"]));
    let gaps = report["gaps"].as_array().expect("gaps");
    assert!(
        gaps.iter().all(|g| g["kind"] != "invalid_link"),
        "refines edge must be valid: {gaps:?}"
    );
}

/// Without a declared `brainstorm` layer the report stays layerless, and
/// finalize says exactly how to fix it instead of failing or pretending.
#[test]
fn finalize_without_declared_layer_warns_and_leaves_report_layerless() {
    let mut s = Session::spawn();
    let project = init_project(&mut s);
    let p = project.path();
    let started = start(&mut s, p);
    let id = started["brainstorm_id"].as_str().unwrap().to_string();
    let report_id = started["report_doc_id"].as_str().unwrap().to_string();

    let fin = finalize_scope(&mut s, p, &id);
    assert!(fin["layer"].is_null(), "{fin}");
    assert!(fin["warnings"].to_string().contains("trace.layer"), "{fin}");
    let doc = s.ok(p, "handoff_doc_get", json!({ "doc_id": report_id }));
    assert!(doc["layer"].is_null(), "{doc}");
}

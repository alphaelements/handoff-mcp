//! Real-binary MCP-tool E2E for `handoff_trace_lint` (wiki/260-vmodel-m2-design.md
//! §4.3, t360.20.8/M2-08): `[[trace.lint.require]]` policy rules read from
//! `config.toml`, and `frontmatter_invalid` surfacing an unreadable document.
//! CLI exit-code 0/1/2 and format=text/never-writes contracts are covered by
//! `tests/cli_trace.rs`'s `cli_trace_lint_*` tests.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

use handoff_mcp::storage::config::{
    read_config, write_config, TraceLintRequireRule, TraceLintRequireWhen,
};
use handoff_mcp::storage::docs::layer::CustomLayerConfig;
use handoff_mcp::storage::docs::write_doc_body;

fn binary() -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("parent")
        .parent()
        .expect("parent")
        .to_path_buf();
    path.push("handoff-mcp");
    path
}

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Server {
    fn spawn() -> Self {
        let mut cmd = Command::new(binary());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = cmd.spawn().expect("failed to spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = String::new();
                match reader.read_line(&mut buf) {
                    Ok(0) => break,
                    Ok(_) => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Server {
            child,
            stdin,
            lines: rx,
            next_id: 1,
        }
    }

    fn call_raw(&mut self, name: &str, arguments: Value) -> (bool, String) {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        writeln!(self.stdin, "{req}").expect("write to server stdin");
        self.stdin.flush().expect("flush server stdin");

        let line = self
            .lines
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("no response for {name} within 10s"));
        let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
        assert_eq!(resp["id"], id, "response id must match request id");
        let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (is_error, text)
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let (is_error, text) = self.call_raw(name, arguments);
        assert!(!is_error, "{name} failed: {text}");
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// A project-defined `[[trace.lint.require]]` policy rule ("approved P0/P1
/// requirements need a verifier", §4.3's own worked example) must produce a
/// finding for an item that matches `when` but doesn't satisfy `need`.
#[test]
fn require_rule_from_config_toml_flags_an_unverified_p0_requirement() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-lint-require-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-lint-require-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-700 Needs verification\n\n\
                - priority: P0\n\nBody.\n",
        }),
    );

    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.lint.require.push(TraceLintRequireRule {
        id: "p0-needs-verification".to_string(),
        when: TraceLintRequireWhen {
            layer: Some("requirement".to_string()),
            priority: vec!["P0".to_string(), "P1".to_string()],
            method: None,
            doc: None,
            approval: None,
        },
        need: "verified_by".to_string(),
        severity: Some("error".to_string()),
    });
    write_config(&config_path, &config).expect("write config");

    let lint = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    assert_eq!(lint["exit_code"], 1, "{lint}");
    let findings = lint["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f["rule"] == "p0-needs-verification" && f["item"] == "REQ-700"),
        "{lint}"
    );

    // Satisfying the policy (adding a verifier) must clear the finding.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "at-lint-require-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-700 Confirms REQ-700\n\n\
                - verifies: REQ-700\n- method: manual\n\nBody.\n",
        }),
    );
    let lint2 = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    assert!(
        !lint2["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["rule"] == "p0-needs-verification"),
        "{lint2}"
    );
}

/// FR-804/E11: a document whose frontmatter fails to parse must surface as a
/// `frontmatter_invalid` finding instead of silently vanishing from the
/// corpus this read-only load scans.
#[test]
fn frontmatter_invalid_reports_an_unreadable_document() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-lint-frontmatter-e2e" }),
    );
    let docs_dir = dir.join(".handoff").join("docs");
    std::fs::create_dir_all(&docs_dir).unwrap();
    std::fs::write(
        docs_dir.join("_doc.broken-lint-e2e.md"),
        "---\nid: doc-broken\ntitle: T\ndoc_type: spec\nscope_paths:\n[]\n\
         created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n---\nbody\n",
    )
    .unwrap();

    let lint = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    assert_eq!(lint["exit_code"], 1, "{lint}");
    let findings = lint["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f["rule"] == "frontmatter_invalid" && f["doc"] == "broken-lint-e2e"),
        "{lint}"
    );
}

/// `rules` filter: restricting to a single rule id must exclude every other
/// finding kind from the response.
#[test]
fn rules_filter_restricts_the_response_to_the_named_rule() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-lint-rules-filter-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "spec-lint-rules-filter-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-800 Lockout\n\n- refines: REQ-999\n\nBody.\n",
        }),
    );

    let lint = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": pd, "rules": ["unverified"] }),
    );
    let findings = lint["findings"].as_array().unwrap();
    assert!(
        findings.iter().all(|f| f["rule"] == "unverified"),
        "rules filter must exclude dangling (error): {lint}"
    );
}

/// M2-08 rework (reviewer round 1 MAJOR finding): a layer-registry warning
/// (here, a `[[trace.layer]]` entry whose `id` duplicates a built-in layer)
/// must reach `trace lint`'s own `warnings` array — wiki/260 §2.1: "warning
/// は `trace_report` / `trace_lint` の `warnings` に出す" — exactly once, even
/// though every document in the project is already in sync (so the warning
/// cannot ride in by accident through an in-memory resync's own re-derived
/// `LayerRegistry`, `sync_layer_items_local`'s warnings).
#[test]
fn a_layer_registry_warning_reaches_trace_lints_warnings_exactly_once_with_every_doc_synced() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-lint-config-warning-dedup-e2e" }),
    );
    // This doc is saved (and therefore synced, stamp recorded) before the
    // config change below, so `trace lint`'s subsequent read-only load must
    // not need to resync it in memory at all.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-lint-config-warning-dedup-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-950 Already synced\n\nBody.\n",
        }),
    );

    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.layer.push(CustomLayerConfig {
        id: "requirement".to_string(), // duplicates the built-in id, disabled
        display_name: None,
        side: "left".to_string(),
        level: 1,
        pair: "acceptance".to_string(),
        id_prefixes: Vec::new(),
    });
    write_config(&config_path, &config).expect("write config");

    let lint = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    let warnings = lint["warnings"].as_array().unwrap();
    let matching: Vec<&Value> = warnings
        .iter()
        .filter(|w| w.as_str().is_some_and(|s| s.contains("duplicate")))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the duplicate-layer-id warning must appear exactly once: {lint}"
    );
}

/// t360.20.32 (M2-S8 reviewer): the dedup test above exercises only a single
/// document, and that document is deliberately left already-synced (its own
/// comment says so) — so it never actually re-enters
/// `load_trace_input_fully_read_only`'s per-document resync loop
/// (`sync_layer_items_local`, which is what re-extends `warnings` with a
/// fresh copy of the registry's warnings on every call). A dedup bug that
/// only manifests with *more than one* resynced document would pass that
/// test by accident. Here two layer documents are each directly edited on
/// disk (bypassing `doc_save`, same technique as
/// `cli_trace_lint_never_writes_to_handoff`), so both must genuinely go
/// through an in-memory resync this call — confirmed via the `unsynced_body`
/// finding naming both slugs — while the single duplicate-layer-id registry
/// warning still appears exactly once, not twice.
#[test]
fn warnings_stay_deduped_when_two_documents_are_resynced_in_memory_in_one_call() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "trace-lint-two-doc-resync-dedup-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-lint-two-doc-resync-dedup-e2e-a",
            "title": "Requirements A",
            "layer": "requirement",
            "body": "# Requirements A\n\n### REQ-960 First\n\nBody.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-lint-two-doc-resync-dedup-e2e-b",
            "title": "Requirements B",
            "layer": "requirement",
            "body": "# Requirements B\n\n### REQ-961 Second\n\nBody.\n",
        }),
    );

    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.layer.push(CustomLayerConfig {
        id: "requirement".to_string(), // duplicates the built-in id, disabled
        display_name: None,
        side: "left".to_string(),
        level: 1,
        pair: "acceptance".to_string(),
        id_prefixes: Vec::new(),
    });
    write_config(&config_path, &config).expect("write config");

    // Direct body edits (bypassing doc_save/sync) force both documents
    // through the read-only load's per-document in-memory resync.
    write_doc_body(
        &handoff,
        "req-lint-two-doc-resync-dedup-e2e-a",
        "# Requirements A\n\n### REQ-960 First\n\nDirectly edited text.\n",
    )
    .expect("write_doc_body a");
    write_doc_body(
        &handoff,
        "req-lint-two-doc-resync-dedup-e2e-b",
        "# Requirements B\n\n### REQ-961 Second\n\nDirectly edited text.\n",
    )
    .expect("write_doc_body b");

    let lint = server.call("handoff_trace_lint", json!({ "project_dir": pd }));

    // Both documents really were resynced in memory for this call (not a
    // no-op — otherwise this test would prove nothing beyond the
    // single-document case above).
    let unsynced_slugs: Vec<&str> = lint["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == "unsynced_body")
        .filter_map(|f| f["doc"].as_str())
        .collect();
    assert!(
        unsynced_slugs.contains(&"req-lint-two-doc-resync-dedup-e2e-a")
            && unsynced_slugs.contains(&"req-lint-two-doc-resync-dedup-e2e-b"),
        "fixture precondition: both documents must be reported as resynced: {lint}"
    );

    let warnings = lint["warnings"].as_array().unwrap();
    let matching: Vec<&Value> = warnings
        .iter()
        .filter(|w| w.as_str().is_some_and(|s| s.contains("duplicate")))
        .collect();
    assert_eq!(
        matching.len(),
        1,
        "the duplicate-layer-id warning must appear exactly once even with two \
         documents resynced in memory: {lint}"
    );
}

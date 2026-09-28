//! Real-binary E2E tests for M2-01 (wiki/260-vmodel-m2-design.md §2.1):
//! `[[trace.layer]]` custom layer declarations flow through the layer
//! registry into `handoff_doc_save`/`handoff_trace_report`'s body parsing
//! and layer sync, and the "使用層の優先順位" (`layers` ＞ profile ＞ auto)
//! is honored by `handoff_trace_report`'s `trace_layers`.
//!
//! Same harness style as `tests/trace_report_slice_e2e.rs`: spawns the real
//! `handoff-mcp` binary and drives it over stdio JSON-RPC, mutating
//! `.handoff/config.toml` directly between calls via
//! `handoff_mcp::storage::config` (mirrors `tests/storage_worktree.rs`).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use handoff_mcp::storage::config::{read_config, write_config, TraceProfileConfig};
use handoff_mcp::storage::docs::layer::CustomLayerConfig;
use serde_json::{json, Value};

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

/// A custom left/right layer pair (`ux_spec`/`usability_test`, wiki/260
/// §2.1's own example) declared via `[[trace.layer]]`.
fn declare_ux_layers(dir: &std::path::Path) {
    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config after init");
    config.trace.layer = vec![
        CustomLayerConfig {
            id: "ux_spec".to_string(),
            display_name: Some("UX 仕様".to_string()),
            side: "left".to_string(),
            level: 2,
            pair: "usability_test".to_string(),
            id_prefixes: vec!["UX".to_string()],
        },
        CustomLayerConfig {
            id: "usability_test".to_string(),
            display_name: None,
            side: "right".to_string(),
            level: 2,
            pair: "ux_spec".to_string(),
            id_prefixes: vec!["UXT".to_string()],
        },
    ];
    write_config(&config_path, &config).expect("write config with custom layers");
}

/// A project-defined document is parsed with a custom layer's own ID
/// prefix, and `handoff_trace_report` (which goes through
/// `crate::trace::engine`, not just body parsing) resolves the custom
/// layer's side/level correctly end to end.
#[test]
fn custom_layer_declaration_is_recognized_by_doc_save_and_trace_report() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "custom-layer-e2e" }),
    );
    declare_ux_layers(dir.path());

    let ux = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "ux-spec-e2e",
            "title": "UX spec",
            "layer": "ux_spec",
            "body": "# UX spec\n\n### UX-001 Onboarding flow\n\nA new user completes onboarding in 3 steps.\n",
        }),
    );
    assert!(ux.get("doc_id").is_some(), "doc_save ux_spec failed: {ux}");

    let usability = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "usability-test-e2e",
            "title": "Usability test",
            "layer": "usability_test",
            "body": "# Usability test\n\n### UXT-001 Onboarding usability\n\n- verifies: UX-001\n\nObserve a new user complete onboarding.\n",
        }),
    );
    assert!(
        usability.get("doc_id").is_some(),
        "doc_save usability_test failed: {usability}"
    );

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy(), "include_items": true }),
    );
    assert_eq!(
        report["warnings"],
        json!([]),
        "unexpected warnings: {report}"
    );

    let items = report["items"].as_array().expect("items array");
    let ux_item = items
        .iter()
        .find(|i| i["id"] == "UX-001")
        .unwrap_or_else(|| panic!("UX-001 missing from items: {report}"));
    assert_eq!(ux_item["layer"], "ux_spec");
    assert_eq!(
        ux_item["side"], "left",
        "custom layer side must resolve through the registry, not just parse"
    );

    // UXT-001 verifies UX-001, so it must be resolved as `passing`'s
    // prerequisite: the coverage for ux_spec should show 0 uncovered/gaps
    // for a horizontally-verified item — checked indirectly via `state`,
    // which requires the refines/verifies graph to have actually linked the
    // two custom-layer items (not just parsed their headings).
    assert_eq!(ux_item["verifies"], json!([]));
    assert_eq!(
        items.iter().find(|i| i["id"] == "UXT-001").unwrap()["verifies"],
        json!(["UX-001"])
    );
    assert!(
        report["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|g| g["item"] != "UX-001" || g["kind"] != "unverified"),
        "UX-001 must not be unverified: {report}"
    );
}

/// An invalid `[[trace.layer]]` declaration (non-reciprocal `pair`) is
/// disabled with a warning rather than failing the whole request, and the
/// offending layer id is then treated as unknown by body parsing.
#[test]
fn invalid_custom_layer_is_disabled_with_a_warning_not_a_hard_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "invalid-layer-e2e" }),
    );

    let config_path = dir.path().join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.layer = vec![CustomLayerConfig {
        id: "ux_spec".to_string(),
        display_name: None,
        side: "left".to_string(),
        level: 2,
        pair: "acceptance".to_string(), // acceptance.pair == "requirement", not "ux_spec" -> not reciprocal
        id_prefixes: vec!["UX".to_string()],
    }];
    write_config(&config_path, &config).expect("write config");

    let doc = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "ux-spec-invalid-e2e",
            "title": "UX spec",
            "layer": "ux_spec",
            "body": "# UX spec\n\n### UX-001 Onboarding flow\n\nSome text.\n",
        }),
    );
    assert!(
        doc.get("doc_id").is_some(),
        "doc_save must still succeed: {doc}"
    );

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    let warnings = report["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("not reciprocal")),
        "expected a non-reciprocal-pair warning in {warnings:?}"
    );
}

/// `layers` config (explicit) takes priority over the project default
/// profile's own `layers`, which in turn takes priority over
/// auto-detection — wiki/260 §2.1's "layers ＞ profile ＞ auto".
#[test]
fn used_layers_priority_is_explicit_config_then_profile_then_auto() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "profile-priority-e2e" }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "requirements-priority-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Something\n\nBody.\n",
        }),
    );

    // Tier 3: no [trace] layers, no profile -> auto-detected from items
    // actually present (just "requirement" here).
    let auto = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert_eq!(auto["trace_layers"]["source"], "auto");
    assert_eq!(auto["trace_layers"]["in_use"], json!(["requirement"]));

    // Tier 2: set a project default profile ("full") with no explicit
    // [trace] layers -> the profile's own layer list is used, not auto's
    // single-layer detection.
    let config_path = dir.path().join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("full".to_string());
    write_config(&config_path, &config).expect("write config");

    let via_profile = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert_eq!(via_profile["trace_layers"]["source"], "profile");
    let profile_layers = via_profile["trace_layers"]["in_use"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        profile_layers,
        vec![
            "requirement",
            "basic_spec",
            "detailed_spec",
            "acceptance",
            "system_test",
            "unit_test",
        ]
    );

    // Tier 1: an explicit [trace] layers overrides the profile entirely.
    let mut config = read_config(&config_path).expect("read config");
    config.trace.layers = vec!["requirement".to_string(), "acceptance".to_string()];
    write_config(&config_path, &config).expect("write config");

    let via_config = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert_eq!(via_config["trace_layers"]["source"], "config");
    assert_eq!(
        via_config["trace_layers"]["in_use"],
        json!(["requirement", "acceptance"])
    );
    // §2.1: both `[trace] layers` and `[trace] profile` are set at this
    // point — `layers` wins, but the caller must be told the profile is
    // being ignored.
    let via_config_warnings = via_config["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .map(|w| w.as_str().unwrap_or("").to_string())
        .collect::<Vec<_>>();
    assert!(
        via_config_warnings
            .iter()
            .any(|w| w.contains("layers") && w.contains("profile")),
        "expected a layers-and-profile-both-set warning: {via_config_warnings:?}"
    );
}

/// A custom `[trace.profiles.<name>]` entry extending a built-in one, used
/// as the project default.
#[test]
fn custom_profile_extending_a_builtin_resolves_as_project_default() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "custom-profile-e2e" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "requirements-custom-profile-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Something\n\nBody.\n",
        }),
    );

    let config_path = dir.path().join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("web".to_string());
    config.trace.profiles.insert(
        "web".to_string(),
        TraceProfileConfig {
            extends: Some("standard".to_string()),
            layers: vec!["requirement".to_string(), "acceptance".to_string()],
            implicit_acceptance: Some(false),
        },
    );
    write_config(&config_path, &config).expect("write config");

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.path().to_string_lossy() }),
    );
    assert_eq!(report["trace_layers"]["source"], "profile");
    assert_eq!(
        report["trace_layers"]["in_use"],
        json!(["requirement", "acceptance"])
    );
}

/// `doc_save`'s `trace_profile` argument round-trips through frontmatter
/// (persists across a metadata-only re-save, clears on empty string).
#[test]
fn doc_save_trace_profile_argument_round_trips_and_clears() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.path().to_string_lossy(), "project_name": "trace-profile-arg-e2e" }),
    );

    let created = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.path().to_string_lossy(),
            "slug": "bugfix-req-e2e",
            "title": "Bugfix requirements",
            "layer": "requirement",
            "trace_profile": "bugfix",
            "body": "# Bugfix requirements\n\n### REQ-900 Repro\n\nBody.\n",
        }),
    );
    let doc_id = created["doc_id"].as_str().expect("doc_id").to_string();

    let got = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": doc_id }),
    );
    assert_eq!(got["trace_profile"], "bugfix");

    // Metadata-only re-save (no body) must keep it untouched.
    server.call(
        "handoff_doc_save",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": doc_id, "tags": ["x"] }),
    );
    let got2 = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": doc_id }),
    );
    assert_eq!(got2["trace_profile"], "bugfix");

    // Empty string clears it.
    server.call(
        "handoff_doc_save",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": doc_id, "trace_profile": "" }),
    );
    let got3 = server.call(
        "handoff_doc_get",
        json!({ "project_dir": dir.path().to_string_lossy(), "doc_id": doc_id }),
    );
    assert!(got3.get("trace_profile").is_none() || got3["trace_profile"].is_null());
}

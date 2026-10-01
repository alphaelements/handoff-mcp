//! Contract-fixture E2E test for `.handoff/docs/_trace_report.json` (t360.13,
//! wiki/220-vmodel-integration-design.md §3.4, NFR-005): `tests/fixtures/trace/`
//! is the shared contract between handoff-mcp (Rust, this test) and
//! handoff-vscode (TypeScript) — see `tests/fixtures/trace/README.md`. This
//! test copies a committed fixture project into a tempdir, runs the real
//! `handoff-mcp` binary against it (both the CLI `trace report` subcommand
//! and the `handoff_trace_report` MCP tool over stdio), and asserts the
//! output matches that fixture's `expected_output.json` -- the same
//! byte-for-byte content contract handoff-vscode's TS reader must honor.
//!
//! M2-07 (wiki/260-vmodel-m2-design.md §5.1): two fixture directories are
//! exercised, `v1/` (the original, minimal project -- relocated here
//! unchanged in content) and `v2/` (a comprehensive project covering every
//! v2-only feature: a custom layer pair, a per-document `trace_profile`
//! override with its display-name overrides, an implicit acceptance item, a
//! waiver, a `derived` item, horizontal/vertical `partial`, all 3 suspect
//! kinds, an `unbaselined` link, `reverify`, and task blockers -- see
//! `tests/fixtures/trace/v2/README.md`). The binary has no "v1 mode", so the
//! live comparisons use `v1/expected_output_v2.json` and
//! `v2/expected_output.json` (both `schema_version: 2`);
//! `v1/expected_output.json` itself is the frozen `schema_version: 1` sample,
//! guarded by `v1_frozen_fixture_is_schema_version_1_and_additively_preserved_in_v2`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::Value;

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

fn fixture_dir(version: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/trace")
        .join(version)
}

/// Copies `src` to `dst` recursively (`std::fs` has no built-in equivalent).
fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else {
            std::fs::copy(entry.path(), &dst_path).unwrap();
        }
    }
}

/// Sets up a fresh project directory from the committed fixture (`project/
/// handoff/` -- named without a leading dot so the repo's blanket `.handoff/`
/// `.gitignore` rule doesn't swallow it -- copied to `<tmp>/proj/.handoff`,
/// the real directory name every handoff-mcp command expects).
fn setup_project(tmp: &Path, version: &str) -> PathBuf {
    let proj = tmp.join("proj");
    copy_dir_recursive(
        &fixture_dir(version).join("project/handoff"),
        &proj.join(".handoff"),
    );
    proj
}

/// Loads one of a fixture version's expected-output files -- `v1/` keeps two
/// (round-2 rework, see `tests/fixtures/trace/README.md`'s "M2-07" section):
/// the frozen, real `schema_version: 1` sample (`expected_output.json`
/// itself, restored verbatim from the pre-M2-07 commit) that handoff-vscode's
/// planned v1+v2 dual reader needs, and today's binary's actual
/// `schema_version: 2` output for that same unchanged project
/// (`expected_output_v2.json`), which is what the live CLI/MCP tests below
/// actually compare against. `v2/` only ever uses the default
/// `expected_output.json`.
fn expected_output_named(version: &str, file_name: &str) -> Value {
    serde_json::from_str(&std::fs::read_to_string(fixture_dir(version).join(file_name)).unwrap())
        .unwrap()
}

/// Zeroes out the mtime-derived `inputs` fields that necessarily differ
/// across a fresh copy of the fixture (the copy's on-disk mtimes are "now",
/// not the fixture-generation timestamp baked into `expected_output.json`) --
/// every other `inputs` field (`docs_count`/`tasks_count`/`runs_count`/
/// `runs_max_id`/`config_fnv`, all structural properties fixed by the
/// fixture's file set, not by copy time) and `schema_version` are compared
/// exactly.
fn normalize_inputs_mtimes(mut value: Value) -> Value {
    if let Some(inputs) = value.get_mut("inputs").and_then(|v| v.as_object_mut()) {
        inputs.insert("docs_max_mtime_ns".to_string(), serde_json::json!(0));
        inputs.insert("tasks_max_mtime_ns".to_string(), serde_json::json!(0));
    }
    value
}

fn assert_cli_output_matches_fixture(version: &str, file_name: &str) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let proj = setup_project(tmp.path(), version);

    let output = Command::new(binary())
        .args(["trace", "report", "--project-dir", proj.to_str().unwrap()])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "trace report failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(proj.join(".handoff/docs/_trace_report.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(
        normalize_inputs_mtimes(persisted),
        normalize_inputs_mtimes(expected_output_named(version, file_name)),
        "_trace_report.json must match tests/fixtures/trace/{version}/{file_name} \
         (mtime fields normalized)"
    );
}

fn assert_mcp_output_matches_fixture(version: &str, file_name: &str) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let proj = setup_project(tmp.path(), version);

    let mut server = Server::spawn();
    server.call(
        "handoff_trace_report",
        serde_json::json!({ "project_dir": proj.to_string_lossy() }),
    );

    let persisted: Value = serde_json::from_str(
        &std::fs::read_to_string(proj.join(".handoff/docs/_trace_report.json")).unwrap(),
    )
    .unwrap();

    assert_eq!(
        normalize_inputs_mtimes(persisted),
        normalize_inputs_mtimes(expected_output_named(version, file_name)),
        "_trace_report.json must match the {version}/{file_name} fixture regardless of entry \
         point (CLI vs MCP tool)"
    );
}

#[test]
fn cli_trace_report_output_matches_the_committed_v1_contract_fixture() {
    // `v1/expected_output.json` itself is the frozen `schema_version: 1`
    // sample (see `tests/fixtures/trace/README.md`'s "M2-07" section) -- the
    // live binary now produces `schema_version: 2` for every project, so the
    // E2E comparison target is `expected_output_v2.json` instead.
    assert_cli_output_matches_fixture("v1", "expected_output_v2.json");
}

#[test]
fn cli_trace_report_output_matches_the_committed_v2_contract_fixture() {
    assert_cli_output_matches_fixture("v2", "expected_output.json");
}

/// Same fixtures, driven over the `handoff_trace_report` MCP tool (stdio
/// JSON-RPC) instead of the CLI -- both entry points call the same handler,
/// so both must produce byte-identical `_trace_report.json` content.
#[test]
fn mcp_trace_report_output_matches_the_committed_v1_contract_fixture() {
    assert_mcp_output_matches_fixture("v1", "expected_output_v2.json");
}

#[test]
fn mcp_trace_report_output_matches_the_committed_v2_contract_fixture() {
    assert_mcp_output_matches_fixture("v2", "expected_output.json");
}

/// Round-2 rework (MAJOR): guards the relationship between `v1/`'s two
/// expected-output files mechanically rather than by inspection alone --
/// `v1/expected_output.json` must stay a genuine `schema_version: 1` sample
/// (not get silently regenerated into a `schema_version: 2` one again), and
/// every field it carries (aside from `schema_version` and the two
/// always-differing mtime fields, already excluded by
/// [`normalize_inputs_mtimes`] elsewhere in this file) must still be present,
/// unchanged, in `expected_output_v2.json` -- i.e. the M1->M2 schema change
/// really was additive for this project.
#[test]
fn v1_frozen_fixture_is_schema_version_1_and_additively_preserved_in_v2() {
    let frozen = expected_output_named("v1", "expected_output.json");
    assert_eq!(
        frozen["schema_version"],
        serde_json::json!(1),
        "v1/expected_output.json must stay the frozen schema_version 1 sample"
    );

    let current = expected_output_named("v1", "expected_output_v2.json");
    assert_additively_preserved(&frozen, &current, "");
}

/// Recursively asserts every key/value under `old` is present, unchanged,
/// under `new` at the same path -- `new` may carry additional keys `old`
/// does not have (that is exactly what "additive" means), but never the
/// reverse, and never a changed value for a key `old` already had.
/// `schema_version`/`docs_max_mtime_ns`/`tasks_max_mtime_ns` are the only
/// keys allowed to differ (a real version bump, and timestamps that
/// necessarily differ between two separately-generated fixture files).
fn assert_additively_preserved(old: &Value, new: &Value, path: &str) {
    const ALWAYS_DIFFERS: &[&str] = &["schema_version", "docs_max_mtime_ns", "tasks_max_mtime_ns"];
    match old {
        Value::Object(old_map) => {
            let new_map = new.as_object().unwrap_or_else(|| {
                panic!("{path}: expected an object in expected_output_v2.json, got {new}")
            });
            for (key, old_value) in old_map {
                if ALWAYS_DIFFERS.contains(&key.as_str()) {
                    continue;
                }
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                let new_value = new_map
                    .get(key)
                    .unwrap_or_else(|| panic!("{child_path}: present in v1 but missing in v2"));
                assert_additively_preserved(old_value, new_value, &child_path);
            }
        }
        Value::Array(old_items) => {
            let new_items = new.as_array().unwrap_or_else(|| {
                panic!("{path}: expected an array in expected_output_v2.json, got {new}")
            });
            assert_eq!(
                old_items.len(),
                new_items.len(),
                "{path}: array length changed between v1 and v2"
            );
            for (i, (old_item, new_item)) in old_items.iter().zip(new_items).enumerate() {
                assert_additively_preserved(old_item, new_item, &format!("{path}[{i}]"));
            }
        }
        _ => {
            assert_eq!(old, new, "{path}: value changed between v1 and v2");
        }
    }
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

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = serde_json::json!({
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

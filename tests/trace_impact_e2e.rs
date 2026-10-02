//! Real-binary E2E tests for M2-06 `handoff_trace_impact`
//! (wiki/260-vmodel-m2-design.md §4.2) and `doc_save`'s `suspect_introduced`
//! response summary (§4.11). Spawns the actual `handoff-mcp` binary and
//! drives it over real stdio JSON-RPC (same harness style as
//! `tests/trace_suspect_e2e.rs`), plus one CLI (`std::process::Command`, no
//! shell) invocation of `trace impact`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

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

/// Same 4-document V as `tests/trace_suspect_e2e.rs`'s `build_v_project`:
/// requirement REQ-100 <- basic_spec SPEC-100 <- system_test ST-100,
/// requirement REQ-100 <- acceptance AT-100.
fn build_v_project(server: &mut Server, dir: &std::path::Path) -> (String, String) {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-impact-e2e" }),
    );

    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-impact-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\n\
                Original requirement statement text.\n",
        }),
    );
    let req_doc_id = req["doc_id"].as_str().expect("doc_id").to_string();

    let spec = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "spec-impact-e2e",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-100 Lockout counter\n\n\
                - refines: REQ-100\n\n\
                Original spec statement text.\n",
        }),
    );
    let spec_doc_id = spec["doc_id"].as_str().expect("doc_id").to_string();

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "at-impact-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-100 Lockout after 5 failures\n\n\
                - verifies: REQ-100\n\
                - method: manual\n\n\
                Fail login 5 times, then confirm the account is locked.\n",
        }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "st-impact-e2e",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System tests\n\n### ST-100 Counter increments\n\n\
                - verifies: SPEC-100\n\
                - method: auto\n\n\
                Assert the counter increments on each failed login.\n",
        }),
    );

    (req_doc_id, spec_doc_id)
}

fn sorted_strings(v: &Value) -> Vec<String> {
    let mut out: Vec<String> = v
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s.as_str().unwrap().to_string())
        .collect();
    out.sort();
    out
}

/// Entry point 1 (`item` + `proposed`): a hypothetical new definition for
/// REQ-100 flags both its 1-hop dependents as `would_suspect.links`, lists
/// AT-100 (a verifying item) as a `rerun_candidates` entry, and reaches
/// ST-100 (2 hops away) only via `potential`.
#[test]
fn item_mode_with_proposed_text_reports_would_suspect_and_potential() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    let out = server.call(
        "handoff_trace_impact",
        json!({
            "project_dir": dir.to_string_lossy(),
            "item": "REQ-100",
            "proposed": "### REQ-100 Account lockout\n\n\
                - priority: P1\n\n\
                A materially different requirement statement.\n",
        }),
    );

    assert_eq!(out["changed"], json!(["REQ-100"]), "{out}");
    let link_children = sorted_strings(&json!(out["would_suspect"]["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["child"].clone())
        .collect::<Vec<_>>()));
    assert_eq!(
        link_children,
        vec!["AT-100".to_string(), "SPEC-100".to_string()],
        "REQ-100's own 2 direct dependents must be flagged: {out}"
    );
    for l in out["would_suspect"]["links"].as_array().unwrap() {
        assert_eq!(l["upstream"], "REQ-100", "{l}");
    }
    assert!(
        out["rerun_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "AT-100"),
        "AT-100 verifies REQ-100 directly, so it must be a rerun candidate: {out}"
    );
    let potential_ids: Vec<&str> = out["potential"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(
        potential_ids,
        vec!["ST-100"],
        "ST-100 is 2 hops from REQ-100 (via SPEC-100) — reported as potential only: {out}"
    );
    assert_eq!(out["potential"][0]["depth"], 2, "{out}");
    assert_eq!(out["truncated"], false, "{out}");
}

/// `item` mode with neither `proposed` nor `proposed_file`: "assume changed"
/// (§4.2) still flags both direct dependents.
#[test]
fn item_mode_without_proposed_assumes_changed() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    let out = server.call(
        "handoff_trace_impact",
        json!({ "project_dir": dir.to_string_lossy(), "item": "REQ-100" }),
    );
    assert_eq!(out["changed"], json!(["REQ-100"]), "{out}");
    let link_children = sorted_strings(&json!(out["would_suspect"]["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["child"].clone())
        .collect::<Vec<_>>()));
    assert_eq!(
        link_children,
        vec!["AT-100".to_string(), "SPEC-100".to_string()],
        "{out}"
    );
}

/// Entry point 2 (`doc` + `proposed_body`): the whole document's proposed
/// text is parsed at once; every item in it whose def_hash would change is
/// reported.
#[test]
fn doc_mode_with_proposed_body_reports_every_changed_item() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    let out = server.call(
        "handoff_trace_impact",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc": "req-impact-e2e",
            "proposed_body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P0\n\n\
                A materially different requirement statement.\n",
        }),
    );
    assert_eq!(out["changed"], json!(["REQ-100"]), "{out}");
    let link_children = sorted_strings(&json!(out["would_suspect"]["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["child"].clone())
        .collect::<Vec<_>>()));
    assert_eq!(
        link_children,
        vec!["AT-100".to_string(), "SPEC-100".to_string()],
        "{out}"
    );
}

/// Rework round 2 reviewer finding: a proposed body that drops a currently
/// defined item entirely (not just changes it) must be reported as `removed`,
/// together with its direct downstream references (which would become
/// dangling, never `would_suspect` — there is no current hash on the removed
/// side to compare a baseline against) and any task links. Before this fix,
/// `trace_impact` silently reported "no impact" for this exact case.
#[test]
fn doc_mode_reports_removed_item_with_downstream_refs() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    let out = server.call(
        "handoff_trace_impact",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc": "req-impact-e2e",
            "proposed_body": "# Requirements\n\n### REQ-102 Something else entirely\n\n\
                An unrelated new requirement replacing REQ-100.\n",
        }),
    );
    assert_eq!(
        out["changed"],
        json!([]),
        "REQ-100 is gone, not changed: {out}"
    );
    assert_eq!(out["removed"][0]["id"], "REQ-100", "{out}");
    let downstream = sorted_strings(&json!(out["removed"][0]["downstream_refs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["child"].clone())
        .collect::<Vec<_>>()));
    assert_eq!(
        downstream,
        vec!["AT-100".to_string(), "SPEC-100".to_string()],
        "REQ-100's 2 direct dependents must be listed as downstream_refs of the removal: {out}"
    );
    assert!(
        out["removed"][0]["tasks"].as_array().unwrap().is_empty(),
        "no task links REQ-100 in this fixture: {out}"
    );
    assert!(
        out["warnings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|w| w.as_str().unwrap().contains("REQ-100")),
        "the removal must also be surfaced in warnings: {out}"
    );
    assert_eq!(
        out["would_suspect"]["links"].as_array().unwrap().len(),
        0,
        "a removed item is dangling, not suspect: {out}"
    );
}

/// Entry point 3 (`file`): matches the M1-era req_impact behavior — the
/// matched requirement is "implementation changed" (no def_hash simulation,
/// so would_suspect stays empty), and its own verifying item is a rerun
/// candidate.
#[test]
fn file_mode_reports_rerun_candidates_without_would_suspect() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let (req_doc_id, _spec_doc_id) = build_v_project(&mut server, &dir);

    server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "action": "set_refs",
            "sub_item_id": "REQ-100",
            "impl_refs": [{"path": "src/lockout.rs"}],
        }),
    );

    let out = server.call(
        "handoff_trace_impact",
        json!({ "project_dir": dir.to_string_lossy(), "file": "src/lockout.rs" }),
    );
    assert_eq!(out["changed"], json!(["REQ-100"]), "{out}");
    assert_eq!(
        out["would_suspect"]["links"].as_array().unwrap().len(),
        0,
        "a plain implementation-file change must never simulate a def_hash change: {out}"
    );
    assert_eq!(
        out["would_suspect"]["tasks"].as_array().unwrap().len(),
        0,
        "{out}"
    );
    assert!(
        out["rerun_candidates"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "AT-100"),
        "AT-100 verifies REQ-100, so it must be suggested for rerun: {out}"
    );
}

/// Entry point 4 (`git_diff: true`): same matching core as `file`, driven by
/// `git diff HEAD --name-only` in `project_dir`.
#[test]
fn git_diff_mode_reports_the_same_matched_requirement() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let (req_doc_id, _spec_doc_id) = build_v_project(&mut server, &dir);

    server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "action": "set_refs",
            "sub_item_id": "REQ-100",
            "impl_refs": [{"path": "src/lockout.rs"}],
        }),
    );

    let run = |args: &[&str]| {
        let status = Command::new("git")
            .args(args)
            .current_dir(&dir)
            .status()
            .expect("git command should run");
        assert!(status.success(), "git {args:?} failed");
    };
    run(&["init", "-q"]);
    run(&["config", "user.email", "test@example.com"]);
    run(&["config", "user.name", "Test"]);
    let src_dir = dir.join("src");
    std::fs::create_dir_all(&src_dir).unwrap();
    std::fs::write(src_dir.join("lockout.rs"), "fn a() {}\n").unwrap();
    run(&["add", "-A"]);
    run(&["commit", "-q", "-m", "init"]);
    std::fs::write(src_dir.join("lockout.rs"), "fn a() { /* changed */ }\n").unwrap();

    let out = server.call(
        "handoff_trace_impact",
        json!({ "project_dir": dir.to_string_lossy(), "git_diff": true }),
    );
    assert_eq!(out["changed"], json!(["REQ-100"]), "{out}");
}

/// Giving more than one entry point at once is rejected outright (never a
/// silent priority pick — this tool's 4 entry points are documented as
/// mutually exclusive, unlike `handoff_doc_req_impact`'s 2-way `file`/
/// `git_diff` priority rule).
#[test]
fn conflicting_entry_points_are_rejected() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_impact",
        json!({ "project_dir": dir.to_string_lossy(), "item": "REQ-100", "file": "src/x.rs" }),
    );
    assert!(is_error, "expected an error, got: {text}");
}

/// Neither entry point given is likewise rejected.
#[test]
fn no_entry_point_is_rejected() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    let (is_error, text) = server.call_raw(
        "handoff_trace_impact",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    assert!(is_error, "expected an error, got: {text}");
}

/// `doc` mode is defined over layer documents only (§4.2). A non-layer
/// document's stable-id SubItems live in frontmatter, not body notation, so
/// analyzing one would misreport every such item as `removed` — it must be
/// rejected instead (session review round 2).
#[test]
fn doc_mode_rejects_a_non_layer_document() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "plain-notes",
            "title": "Plain notes",
            "body": "# Plain notes\n\n## Section\n\nNot a layer document.\n",
        }),
    );

    let (is_error, text) = server.call_raw(
        "handoff_trace_impact",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc": "plain-notes",
            "proposed_body": "# Plain notes\n\n## Section\n\nNot a layer document.\n",
        }),
    );
    assert!(is_error, "expected an error, got: {text}");
    assert!(text.contains("not a layer document"), "{text}");
}

/// `handoff_trace_impact` must never write a single byte under `.handoff/`
/// (E6, router::READ_ONLY_TOOLS) — mirrors
/// `tests/trace_suspect_e2e.rs`'s `list_and_baseline_dry_run_never_write_to_handoff`.
#[test]
fn trace_impact_never_writes_to_handoff() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);

    fn snapshot(handoff: &std::path::Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        for entry in walkdir_lite(handoff) {
            if entry.is_file() {
                out.push((entry.clone(), std::fs::read(&entry).unwrap()));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        out
    }

    fn walkdir_lite(dir: &std::path::Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    out.extend(walkdir_lite(&path));
                } else {
                    out.push(path);
                }
            }
        }
        out
    }

    // No warm-up call, deliberately: `handle_trace_impact` uses
    // `load_trace_input_read_only` (rework round 2 fix), never
    // `runs::sync` — the very first call, from a project with zero runs
    // recorded and no `runs/_latest.json` on disk yet, must already be
    // byte-invariant.
    let before = snapshot(&handoff);
    server.call(
        "handoff_trace_impact",
        json!({
            "project_dir": dir.to_string_lossy(),
            "item": "REQ-100",
            "proposed": "### REQ-100 Account lockout\n\nSomething else entirely.\n",
        }),
    );
    server.call(
        "handoff_trace_impact",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc": "req-impact-e2e",
            "proposed_body": "# Requirements\n\n### REQ-100 Account lockout\n\nYet more text.\n",
        }),
    );
    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "handoff_trace_impact must never change any byte under .handoff/, including on its \
         very first call before runs/_latest.json exists on disk"
    );
}

/// §4.11: `doc_save`'s own `suspect_introduced` response summary, exercised
/// over the real binary's stdio transport (unit-level coverage of the same
/// behavior lives in `src/mcp/handlers/docs.rs`'s `suspect_introduced_tests`).
#[test]
fn doc_save_response_includes_suspect_introduced_summary_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    let (req_doc_id, _spec_doc_id) = build_v_project(&mut server, &dir);

    let out = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\n\
                Updated requirement statement text.\n",
        }),
    );
    let si = &out["suspect_introduced"];
    assert_eq!(si["changed"], json!(["REQ-100"]), "{out}");
    let link_children = sorted_strings(&json!(si["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["child"].clone())
        .collect::<Vec<_>>()));
    assert_eq!(
        link_children,
        vec!["AT-100".to_string(), "SPEC-100".to_string()],
        "{out}"
    );
}

/// CLI E2E (no shell — `std::process::Command::new` + `.args`, wiki/260
/// §5.3): `trace impact --item ID --proposed-file F`.
#[test]
fn cli_trace_impact_with_proposed_file() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_v_project(&mut server, &dir);
    drop(server);

    let proposed_path = tmp.path().join("proposed.md");
    std::fs::write(
        &proposed_path,
        "### REQ-100 Account lockout\n\n- priority: P1\n\nA materially different statement.\n",
    )
    .unwrap();

    let output = Command::new(binary())
        .args([
            "trace",
            "impact",
            "--project-dir",
            dir.to_str().unwrap(),
            "--item",
            "REQ-100",
            "--proposed-file",
            proposed_path.to_str().unwrap(),
        ])
        .output()
        .expect("failed to run binary");
    assert!(
        output.status.success(),
        "cli trace impact failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let v: Value = serde_json::from_str(stdout.trim()).expect("valid JSON stdout");
    assert_eq!(v["changed"], json!(["REQ-100"]), "{v}");
    let link_children = sorted_strings(&json!(v["would_suspect"]["links"]
        .as_array()
        .unwrap()
        .iter()
        .map(|l| l["child"].clone())
        .collect::<Vec<_>>()));
    assert_eq!(
        link_children,
        vec!["AT-100".to_string(), "SPEC-100".to_string()],
        "{v}"
    );
}

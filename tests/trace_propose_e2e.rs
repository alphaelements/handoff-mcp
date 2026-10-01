//! Real-binary E2E tests for M2-17 `handoff_trace_propose`
//! (wiki/260-vmodel-m2-design.md §4.10): spawns the actual `handoff-mcp`
//! binary and drives it over real stdio JSON-RPC (same harness style as
//! `tests/trace_impact_e2e.rs`), plus one CLI (`std::process::Command`, no
//! shell) invocation of `trace propose`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use handoff_mcp::storage::config::{read_config, write_config};
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

fn run_cli(args: &[&str]) -> (String, String, i32) {
    let output = Command::new(binary())
        .args(args)
        .output()
        .expect("failed to run binary");
    (
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
        output.status.code().unwrap_or(-1),
    )
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

/// A `minimal`-profile project with one requirement document (`REQ-100`,
/// `scope_paths: ["src/auth/"]`) and one task (`t1`, title "Account
/// lockout", `scope_paths: ["src/auth/login.rs"]`) whose requirement links
/// already cover `REQ-100` — a realistic "did we already write this down?"
/// setup for `handoff_trace_propose`.
fn build_minimal_project(server: &mut Server, dir: &std::path::Path) -> String {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-propose-e2e" }),
    );

    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("minimal".to_string());
    write_config(&config_path, &config).expect("write config");

    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "auth-requirements",
            "title": "Auth requirements",
            "doc_type": "spec",
            "layer": "requirement",
            "scope_paths": ["src/auth/"],
            "body": "# Requirements\n\n\
                ### REQ-100 Account lockout after failed logins\n\n\
                5 consecutive failed logins locks the account for 15 minutes.\n\n\
                受入基準:\n\
                - AC1: Given 4 prior failures When a 5th fails Then the account is locked\n",
        }),
    );
    let req_doc_id = req["doc_id"].as_str().unwrap().to_string();

    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": {
                "id": "t1",
                "title": "Account lockout",
                "scope_paths": ["src/auth/login.rs"],
            },
        }),
    );

    req_doc_id
}

#[test]
fn task_id_input_returns_a_similar_existing_candidate_and_no_new_proposal_needed_warning() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_minimal_project(&mut server, &dir);

    let out = server.call(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1" }),
    );

    let candidates = out["candidates"].as_array().expect("candidates array");
    assert!(
        candidates.iter().any(|c| c["id"] == "REQ-100"),
        "expected REQ-100 among candidates: {out}"
    );
    let top = &candidates[0];
    assert_eq!(top["id"], "REQ-100", "{out}");
    assert_eq!(top["layer"], "requirement", "{out}");
    assert!(top["score"].as_f64().unwrap() > 0.0, "{out}");

    // minimal profile: proposal is a single REQ- item with an inline
    // acceptance-criteria block, placed in the scope-overlapping
    // `auth-requirements` document.
    assert_eq!(out["proposal"]["profile"], "minimal", "{out}");
    assert_eq!(out["proposal"]["doc"], "auth-requirements", "{out}");
    let next_ids = out["proposal"]["next_ids"].as_array().unwrap();
    assert_eq!(next_ids.len(), 1, "{out}");
    assert!(next_ids[0].as_str().unwrap().starts_with("REQ-"), "{out}");
    let markdown = out["proposal"]["markdown"].as_str().unwrap();
    assert!(markdown.contains("受入基準"), "{markdown}");
}

#[test]
fn title_only_input_with_no_task_suggests_a_new_document_slug() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_minimal_project(&mut server, &dir);

    // No `task_id` -> no scope_paths to compare against any document, so the
    // placement always falls back to a suggested (never created) new slug —
    // even though a scope-overlapping document would exist for a real task.
    let out = server.call(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "title": "Session timeout" }),
    );
    assert_ne!(out["proposal"]["doc"], "auth-requirements", "{out}");
    assert!(
        out["warnings"].as_array().unwrap().iter().any(|w| w
            .as_str()
            .unwrap()
            .contains("suggesting a new document slug")),
        "{out}"
    );
}

#[test]
fn rejects_both_task_id_and_title() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-propose-e2e-err" }),
    );

    let (is_error, text) = server.call_raw(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1", "title": "x" }),
    );
    assert!(is_error, "expected an error, got: {text}");
    assert!(text.contains("mutually exclusive"), "{text}");
}

#[test]
fn cli_trace_propose_returns_candidates_and_proposal() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    build_minimal_project(&mut server, &dir);
    drop(server); // release the project dir before the CLI touches it too

    let (stdout, stderr, code) = run_cli(&[
        "trace",
        "propose",
        "--project-dir",
        dir.to_str().unwrap(),
        "--task-id",
        "t1",
    ]);
    assert_eq!(code, 0, "stdout={stdout}\nstderr={stderr}");
    let out: Value = serde_json::from_str(&stdout).expect("valid JSON stdout");
    assert_eq!(out["proposal"]["profile"], "minimal", "{out}");
}

/// `handoff_trace_propose` must never write a single byte under `.handoff/`
/// (E6, `router::READ_ONLY_TOOLS`) — mirrors `tests/trace_impact_e2e.rs`'s
/// `trace_impact_never_writes_to_handoff`.
#[test]
fn trace_propose_never_writes_to_handoff() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");
    let mut server = Server::spawn();
    build_minimal_project(&mut server, &dir);

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

    // No warm-up call, deliberately (same rationale as
    // `trace_impact_never_writes_to_handoff`): `handle_trace_propose` calls
    // no write path at all (no `runs::sync`, no layer resync), so the very
    // first call must already be byte-invariant.
    let before = snapshot(&handoff);
    server.call(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1" }),
    );
    server.call(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "title": "Ad-hoc title with no task" }),
    );
    let after = snapshot(&handoff);
    assert_eq!(
        before, after,
        "handoff_trace_propose must never change any byte under .handoff/"
    );
}

/// t360.20.30 (M2-S6 reviewer/developer B findings), real-binary coverage for
/// all three behavior changes together: (1) a `standard`-profile project
/// whose `basic_spec` target document overrides `trace_profile` to `minimal`
/// gets a single inline-受入基準 item (not a `basic_spec`/`system_test`
/// pair) — the document override wins over the project default; (2) that
/// same `basic_spec` layer (not top-most left layer in `standard`) gets a
/// `refines:` suggestion naming the existing, scope-unrelated `REQ-` item
/// that best matches the query title; (3) an implicit acceptance-
/// verification sub-item materialized by a real `doc_save` (`REQ-900#AC1`)
/// never appears as its own candidate, and only the parent `REQ-900` does.
#[test]
fn doc_trace_profile_override_and_refines_suggestion_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-propose-e2e-2" }),
    );
    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("standard".to_string());
    write_config(&config_path, &config).expect("write config");

    // A requirement document whose own layer sync (real `doc_save`, implicit
    // acceptance disabled project-wide under `standard`) materializes no
    // implicit sub-item on its own — `implicit_acceptance` for *this*
    // document's own sync is driven by the project default (`standard`,
    // false), independent of the `minimal`-overridden `auth-spec` document
    // below; REQ-900's own AC1 bullet is exercised in a *second* document
    // that explicitly sets `trace_profile: minimal` so its layer sync
    // materializes the implicit `REQ-900#AC1` sub-item this test checks is
    // excluded from candidates.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "legacy-requirements",
            "title": "Legacy requirements",
            "doc_type": "spec",
            "layer": "requirement",
            "trace_profile": "minimal",
            "body": "# Requirements\n\n\
                ### REQ-900 Account lockout after failed logins\n\n\
                5 consecutive failed logins locks the account for 15 minutes.\n\n\
                受入基準:\n\
                - AC1: Given 4 prior failures When a 5th fails Then the account is locked\n",
        }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "auth-spec",
            "title": "Auth spec",
            "doc_type": "spec",
            "layer": "basic_spec",
            "scope_paths": ["src/auth/"],
            "trace_profile": "minimal",
            "body": "# Spec\n",
        }),
    );

    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": {
                "id": "t1",
                "title": "Account lockout after failed logins",
                "scope_paths": ["src/auth/login.rs"],
            },
        }),
    );

    let out = server.call(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1" }),
    );

    // (3) the implicit AC sub-item never appears as its own candidate.
    let candidates = out["candidates"].as_array().expect("candidates array");
    assert!(
        candidates.iter().all(|c| c["id"] != "REQ-900#AC1"),
        "implicit item must never appear as a candidate: {out}"
    );
    assert!(
        candidates.iter().any(|c| c["id"] == "REQ-900"),
        "expected REQ-900 among candidates: {out}"
    );

    // (1) the auth-spec document's own trace_profile=minimal override wins
    // over the project default (standard) for implicit_acceptance.
    assert_eq!(out["proposal"]["doc"], "auth-spec", "{out}");
    assert_eq!(out["proposal"]["profile"], "standard", "{out}");
    let next_ids = out["proposal"]["next_ids"].as_array().unwrap();
    assert_eq!(next_ids.len(), 1, "{out}");
    assert!(next_ids[0].as_str().unwrap().starts_with("SPEC-"), "{out}");
    let markdown = out["proposal"]["markdown"].as_str().unwrap();
    assert!(markdown.contains("受入基準"), "{markdown}");

    // (2) basic_spec is not standard's top-most left layer (requirement is)
    // -> a refines: suggestion naming the top candidate (REQ-900).
    assert!(markdown.contains("- refines: REQ-900"), "{markdown}");
}

/// Review-rework round 2 (t360.20.30, MAJOR finding), real-binary repro of
/// the reviewer's own scenario: a `standard` project with an unrelated
/// REQ-001 (`requirement`) and a lexically closer ST-001 (`system_test`,
/// right-side). Before the fix, `trace_propose` suggested `- refines:
/// ST-001` — the plain top of `candidates` by title similarity alone — which
/// `handoff_doc_save` then flagged as `invalid_link` once actually saved
/// (`refines target is not a strictly upper left-side item`,
/// `src/trace/engine.rs`). This test drives the full real round-trip: propose
/// -> save the proposed markdown verbatim via a real `doc_save` -> confirm no
/// `invalid_link` gap exists for the new item via a real `trace_report`.
#[test]
fn refines_suggestion_targets_a_valid_upper_layer_item_not_the_top_scoring_one() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-propose-e2e-3" }),
    );
    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("standard".to_string());
    write_config(&config_path, &config).expect("write config");

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements",
            "title": "Requirements",
            "doc_type": "spec",
            "layer": "requirement",
            "body": "# Requirements\n\n\
                ### REQ-001 Something unrelated\n\n\
                Not related to the new item's topic at all.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "system-tests",
            "title": "System tests",
            "doc_type": "spec",
            "layer": "system_test",
            "body": "# System tests\n\n\
                ### ST-001 Audit log retention check\n\n\
                Verifies the audit log retains entries for the required period.\n",
        }),
    );

    let out = server.call(
        "handoff_trace_propose",
        json!({ "project_dir": dir.to_string_lossy(), "title": "Audit log retention" }),
    );

    // Sanity check on the repro itself: ST-001 does outrank REQ-001 by title
    // similarity (otherwise this test would not exercise the bug at all).
    assert_eq!(out["candidates"][0]["id"], "ST-001", "{out}");

    let markdown = out["proposal"]["markdown"].as_str().unwrap();
    assert!(markdown.contains("- refines: REQ-001"), "{markdown}");
    assert!(!markdown.contains("- refines: ST-001"), "{markdown}");

    // Save the proposed markdown verbatim into a new basic_spec document, the
    // same way a real caller would act on this proposal.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "new-spec",
            "title": "New spec",
            "doc_type": "spec",
            "layer": "basic_spec",
            "body": format!("# Spec\n\n{markdown}\n"),
        }),
    );

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let gaps = report["gaps"].as_array().unwrap();
    assert!(
        gaps.iter().all(|g| g["kind"] != "invalid_link"),
        "the saved refines: suggestion must not be flagged invalid_link: {gaps:?}"
    );
}

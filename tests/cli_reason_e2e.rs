//! CLI E2E for `--reason` argv parsing (t-cli-reason): the real binary is
//! spawned with `Command::new` (no shell). A reason that itself starts with
//! `--` must reach the tool as text (not be swallowed as the next flag), and
//! `--reason=<text>` must be accepted. Observed through the audit record that
//! `trace suspect --action clear` writes under `.handoff/trace/clears/`.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc;
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

/// Runs the setup calls through the real binary's stdio JSON-RPC transport.
fn setup_calls(calls: &[(&str, Value)]) -> Vec<Value> {
    let mut child = Command::new(binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("failed to spawn handoff-mcp server");
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });

    let mut results = Vec::new();
    for (i, (name, arguments)) in calls.iter().enumerate() {
        let id = i as u64 + 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        writeln!(stdin, "{req}").expect("write request");
        stdin.flush().expect("flush");
        let line = rx
            .recv_timeout(Duration::from_secs(10))
            .unwrap_or_else(|_| panic!("no response for {name} within 10s"));
        let resp: Value = serde_json::from_str(&line).expect("JSON-RPC response");
        assert_eq!(resp["id"], id);
        let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "{name} failed: {text}"
        );
        results.push(serde_json::from_str(text).unwrap_or(Value::Null));
    }
    let _ = child.kill();
    let _ = child.wait();
    results
}

const REQ_V1: &str = "# Requirements\n\n### REQ-100 Lockout\n\n- priority: P1\n\nOriginal text.\n";
const REQ_V2: &str =
    "# Requirements\n\n### REQ-100 Lockout\n\n- priority: P1\n\nChanged text (round 2).\n";

/// Builds a project where task `t1` (linked to REQ-100) is a task suspect.
fn project_with_task_suspect(dir: &Path) {
    let pd = dir.to_string_lossy().to_string();
    let first = setup_calls(&[
        (
            "handoff_init",
            json!({ "project_dir": pd, "project_name": "cli-reason-e2e" }),
        ),
        (
            "handoff_doc_save",
            json!({ "project_dir": pd, "slug": "reqs", "title": "reqs",
                    "layer": "requirement", "body": REQ_V1 }),
        ),
    ]);
    let doc_id = first[1]["doc_id"].as_str().expect("doc_id").to_string();
    setup_calls(&[
        (
            "handoff_update_task",
            json!({ "project_dir": pd,
                    "task": { "id": "t1", "title": "Implement lockout",
                              "requirement_ids": ["REQ-100"] } }),
        ),
        (
            "handoff_doc_save",
            json!({ "project_dir": pd, "slug": "reqs", "title": "reqs",
                    "doc_id": doc_id, "body": REQ_V2 }),
        ),
    ]);
}

/// The `reason` recorded in the single clear audit file, if any.
fn recorded_reasons(dir: &Path) -> Vec<String> {
    let clears = dir.join(".handoff/trace/clears");
    let Ok(entries) = std::fs::read_dir(&clears) else {
        return Vec::new();
    };
    entries
        .map(|e| {
            let v: Value =
                serde_json::from_str(&std::fs::read_to_string(e.unwrap().path()).unwrap()).unwrap();
            v["reason"].as_str().expect("reason string").to_string()
        })
        .collect()
}

fn clear_with(reason_args: &[&str]) -> (Vec<String>, String, i32) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    project_with_task_suspect(&dir);
    let dir_str = dir.to_str().unwrap();

    let mut args = vec![
        "trace",
        "suspect",
        "--action",
        "clear",
        "--targets",
        r#"[{"task_id":"t1"}]"#,
        "--project-dir",
        dir_str,
    ];
    args.extend_from_slice(reason_args);
    let (stdout, stderr, code) = run_cli(&args);
    (recorded_reasons(&dir), format!("{stdout}{stderr}"), code)
}

#[test]
fn cli_reason_value_starting_with_double_dash_is_recorded_as_text() {
    let (reasons, out, code) = clear_with(&["--reason", "--force is not working"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(reasons, vec!["--force is not working".to_string()], "{out}");
}

#[test]
fn cli_reason_equals_form_is_accepted() {
    let (reasons, out, code) = clear_with(&["--reason=reviewed, text-only change"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(
        reasons,
        vec!["reviewed, text-only change".to_string()],
        "{out}"
    );
}

#[test]
fn cli_reason_equals_form_with_leading_double_dash_value() {
    let (reasons, out, code) = clear_with(&["--reason=--force is not working"]);
    assert_eq!(code, 0, "{out}");
    assert_eq!(reasons, vec!["--force is not working".to_string()], "{out}");
}

#[test]
fn cli_reason_without_a_value_fails_with_a_clear_message() {
    let (reasons, out, code) = clear_with(&["--reason"]);
    assert_ne!(code, 0, "{out}");
    assert!(out.contains("--reason"), "{out}");
    assert!(reasons.is_empty(), "nothing may be cleared: {reasons:?}");
}

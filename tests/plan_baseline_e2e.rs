//! Real-binary E2E for t391.2 + t391.4 (implementation-plan management and
//! baseline schedule): spawns the actual `handoff-mcp` binary over stdio
//! JSON-RPC and round-trips `doc_type: plan`, `handoff_get_metrics(plan_id)`,
//! milestone `requirement_coverage`, and the `baseline_start`/`baseline_due`
//! lifecycle (auto-record, never-overwrite, `""` reset).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
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

    /// Returns the tool's raw response text, unparsed — some tools
    /// (`handoff_update_task`'s create path) return a plain confirmation
    /// string, not JSON.
    fn call_raw(&mut self, name: &str, arguments: Value) -> String {
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
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "{name} failed: {resp}"
        );
        resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string()
    }

    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let text = self.call_raw(name, arguments);
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn project() -> (tempfile::TempDir, PathBuf, Server) {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "plan-baseline" }),
    );
    (tmp, dir, server)
}

fn create_task(server: &mut Server, dir: &Path, title: &str, milestone: &str) -> String {
    let created = server.call_raw(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "title": title, "schedule": { "estimate_hours": 2.0, "milestone": milestone } },
        }),
    );
    let id = created
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap_or_default()
        .to_string();
    assert!(!id.is_empty(), "could not extract task id from: {created}");
    id
}

fn get_task(server: &mut Server, dir: &Path, id: &str) -> Value {
    server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": id }),
    )
}

fn milestone<'a>(metrics: &'a Value, name: &str) -> &'a Value {
    metrics["milestones"]
        .as_array()
        .expect("milestones array")
        .iter()
        .find(|m| m["name"] == name)
        .unwrap_or_else(|| panic!("milestone {name} missing in {metrics}"))
}

#[test]
fn plan_doc_scopes_metrics_and_reports_milestone_requirement_coverage() {
    let (_tmp, dir, mut server) = project();
    let pd = dir.to_string_lossy().to_string();
    // auto_layer must NOT put a plan on a V-model layer.
    server.call(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "trace.auto_layer": true } }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "plan-reqs",
            "title": "Plan requirements",
            "body": "# Spec\n\n### REQ-301 First\n\nA.\n\n### REQ-302 Second\n\nB.\n",
            "layer": "basic_spec",
        }),
    );
    let t_done = create_task(&mut server, &dir, "Done in plan", "m1");
    let t_todo = create_task(&mut server, &dir, "Todo in plan", "m1");
    create_task(&mut server, &dir, "Outside the plan", "m1");

    server.call_raw(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": {
            "id": t_done, "requirement_ids": ["REQ-301"], "status": "done" } }),
    );
    server.call_raw(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": t_todo, "requirement_ids": ["REQ-302"] } }),
    );

    let plan = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "impl-plan",
            "title": "Implementation plan",
            "doc_type": "plan",
            "body": "# Implementation plan\n\nScope.\n",
            "task_ids": [t_done, t_todo],
        }),
    );
    let plan_id = plan["doc_id"].as_str().expect("doc_id").to_string();
    assert_eq!(plan["doc_type"], "plan");

    // doc_list(doc_type=plan) returns only the plan.
    let listed = server.call(
        "handoff_doc_list",
        json!({ "project_dir": pd, "doc_type": "plan" }),
    );
    let docs = listed["documents"].as_array().expect("documents");
    assert_eq!(docs.len(), 1, "{listed}");
    assert_eq!(docs[0]["slug"], "impl-plan");

    // auto_layer=true must leave the plan layer-less.
    let meta = server.call(
        "handoff_doc_get",
        json!({ "project_dir": pd, "doc_id": plan_id, "format": "meta" }),
    );
    assert!(
        meta["layer"].is_null(),
        "plan must not get an auto-inferred layer: {meta}"
    );

    // Whole project vs plan-scoped metrics.
    let all = server.call("handoff_get_metrics", json!({ "project_dir": pd }));
    assert_eq!(all["total"], 3);
    assert_eq!(milestone(&all, "m1")["total"], 3);

    for key in [plan_id.as_str(), "impl-plan"] {
        let scoped = server.call(
            "handoff_get_metrics",
            json!({ "project_dir": pd, "plan_id": key }),
        );
        assert_eq!(scoped["total"], 2, "plan_id={key}: {scoped}");
        let ms = milestone(&scoped, "m1");
        assert_eq!(ms["total"], 2);
        let cov = &ms["requirement_coverage"];
        assert_eq!(cov["total"], 2, "{ms}");
        assert_eq!(cov["implemented"], 1, "{ms}");
        assert_eq!(cov["not_started"], 1, "{ms}");
        assert_eq!(cov["coverage_percent"], 50.0, "{ms}");
    }

    // An unknown plan is an empty result, not an error.
    let none = server.call(
        "handoff_get_metrics",
        json!({ "project_dir": pd, "plan_id": "no-such-plan" }),
    );
    assert_eq!(none["total"], 0);
}

#[test]
fn baseline_is_auto_recorded_once_and_resettable_with_empty_string() {
    let (_tmp, dir, mut server) = project();
    let pd = dir.to_string_lossy().to_string();
    let t_set = create_task(&mut server, &dir, "Has baseline", "m1");
    let t_unset = create_task(&mut server, &dir, "No baseline", "m1");

    // A fresh task serializes without baseline values.
    let fresh = get_task(&mut server, &dir, &t_set);
    assert!(fresh["schedule"]["baseline_start"].is_null(), "{fresh}");

    server.call_raw(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": t_set, "schedule": {
            "baseline_start": "2020-01-01", "baseline_due": "2020-01-02" } } }),
    );
    let set = get_task(&mut server, &dir, &t_set);
    assert_eq!(set["schedule"]["baseline_start"], "2020-01-01");
    assert_eq!(set["schedule"]["baseline_due"], "2020-01-02");
    assert_eq!(
        set["schedule"]["estimate_hours"], 2.0,
        "merge keeps other fields"
    );

    // dry_run records nothing.
    server.call(
        "handoff_auto_schedule",
        json!({ "project_dir": pd, "dry_run": true, "start_date": "2026-03-02" }),
    );
    let dry = get_task(&mut server, &dir, &t_unset);
    assert!(dry["schedule"]["baseline_start"].is_null(), "{dry}");

    // Apply: unset task gets a baseline equal to its first schedule; the
    // explicitly set one is left alone.
    server.call(
        "handoff_auto_schedule",
        json!({ "project_dir": pd, "dry_run": false, "start_date": "2026-03-02" }),
    );
    let unset = get_task(&mut server, &dir, &t_unset);
    assert_eq!(
        unset["schedule"]["baseline_start"],
        unset["schedule"]["start_date"]
    );
    assert_eq!(
        unset["schedule"]["baseline_due"],
        unset["schedule"]["due_date"]
    );
    assert!(unset["schedule"]["baseline_start"].is_string(), "{unset}");
    let set = get_task(&mut server, &dir, &t_set);
    assert_eq!(set["schedule"]["baseline_start"], "2020-01-01");
    assert_eq!(set["schedule"]["baseline_due"], "2020-01-02");
    assert_ne!(set["schedule"]["start_date"], "2020-01-01");

    // Rescheduling from a later anchor must not move the recorded baseline.
    let first_baseline = unset["schedule"]["baseline_start"].clone();
    server.call(
        "handoff_auto_schedule",
        json!({ "project_dir": pd, "dry_run": false, "start_date": "2026-04-06" }),
    );
    let rescheduled = get_task(&mut server, &dir, &t_unset);
    assert_eq!(rescheduled["schedule"]["baseline_start"], first_baseline);
    assert_ne!(rescheduled["schedule"]["start_date"], first_baseline);

    // "" resets; the next apply records a fresh baseline.
    server.call_raw(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": t_set, "schedule": {
            "baseline_start": "", "baseline_due": "" } } }),
    );
    let reset = get_task(&mut server, &dir, &t_set);
    assert!(reset["schedule"]["baseline_start"].is_null(), "{reset}");
    assert!(reset["schedule"]["baseline_due"].is_null(), "{reset}");
    server.call(
        "handoff_auto_schedule",
        json!({ "project_dir": pd, "dry_run": false, "start_date": "2026-05-04" }),
    );
    let again = get_task(&mut server, &dir, &t_set);
    assert_eq!(
        again["schedule"]["baseline_start"],
        again["schedule"]["start_date"]
    );
    assert_ne!(again["schedule"]["baseline_start"], "2020-01-01");
}

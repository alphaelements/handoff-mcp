//! Real-binary E2E for the compact `task_tree` of `handoff_load_context`
//! (FR-V0473-002/003, SPEC-V0473-002..702): the default output stays small on
//! a project with hundreds of todos, says why each task is in it, and
//! `task_tree:"full"` still returns exactly what v0.47.2 returned.
//!
//! The projects are built through the running server (the same tool calls the
//! golden fixtures were built with, `scripts/gen-load-context-golden-v0472.sh`);
//! every due date is a fixed day so `overdue_count` does not depend on today.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};
use tempfile::TempDir;

const OLD: &str = "2020-01-10";
const FAR: &str = "2099-12-31";
const SIZE_CAP_BYTES: usize = 20_000;

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
    /// Both identity variables are removed so the result never depends on the
    /// developer's own Claude session.
    fn spawn() -> Self {
        let mut child = Command::new(binary())
            .env_remove("CLAUDE_CODE_SESSION_ID")
            .env_remove("CLAUDE_SESSION_ID")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = String::new();
                match reader.read_line(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            break;
                        }
                    }
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
            .recv_timeout(Duration::from_secs(30))
            .unwrap_or_else(|_| panic!("no response for {name} within 30s"));
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

struct Project {
    _tmp: TempDir,
    dir: PathBuf,
    pd: String,
    server: Server,
}

impl Project {
    fn new(name: &str) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let pd = dir.to_string_lossy().to_string();
        let mut server = Server::spawn();
        server.call(
            "handoff_init",
            json!({ "project_dir": &pd, "project_name": name }),
        );
        server.call(
            "handoff_update_config",
            json!({
                "project_dir": &pd,
                "updates": { "settings.require_estimate_hours": false },
            }),
        );
        Project {
            _tmp: tmp,
            dir,
            pd,
            server,
        }
    }

    fn task(&mut self, task: Value) {
        self.server.call(
            "handoff_update_task",
            json!({ "project_dir": &self.pd, "task": task }),
        );
    }

    /// Close a session whose `related_task_ids` the next load inherits.
    fn close_previous_session(&mut self, related: Vec<String>) {
        self.server.call(
            "handoff_save_context",
            json!({
                "project_dir": &self.pd,
                "summary": "golden previous session",
                "session_status": "closed",
                "related_task_ids": related,
            }),
        );
    }

    fn load(&mut self, extra: Value) -> Value {
        let mut args = json!({ "project_dir": &self.pd });
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            args[k] = v;
        }
        self.server.call("handoff_load_context", args)
    }

    fn set_config(&mut self, key: &str, value: Value) {
        self.server.call(
            "handoff_update_config",
            json!({ "project_dir": &self.pd, "updates": { key: value } }),
        );
    }
}

fn priority_of(i: u32) -> &'static str {
    match i % 3 {
        0 => "high",
        1 => "medium",
        _ => "low",
    }
}

/// 29 tasks: parent/child/grandchild, every status, dependencies and one
/// closed previous session (`small.json`).
fn small_project() -> Project {
    let mut p = Project::new("golden-small");
    p.task(json!({"id":"t1","title":"Epic A","status":"in_progress","priority":"high","labels":["epic"],
        "schedule":{"due_date":FAR,"milestone":"M1","estimate_hours":20}}));
    p.task(
        json!({"id":"t1.1","title":"A child done","status":"done","priority":"medium",
        "schedule":{"due_date":OLD,"milestone":"M1"}}),
    );
    p.task(
        json!({"id":"t1.2","title":"A child in progress","status":"in_progress","priority":"high",
        "assignee":"alice","schedule":{"due_date":OLD,"milestone":"M1","estimate_hours":4}}),
    );
    p.task(
        json!({"id":"t1.2.1","title":"A grandchild todo","status":"todo","priority":"low",
        "schedule":{"due_date":FAR}}),
    );
    p.task(
        json!({"id":"t1.2.2","title":"A grandchild review","status":"review","priority":"medium",
        "schedule":{"due_date":FAR}}),
    );
    p.task(
        json!({"id":"t1.3","title":"A child blocked","status":"blocked","priority":"high",
        "dependencies":["t1.1"],"schedule":{"due_date":OLD}}),
    );
    p.task(
        json!({"id":"t2","title":"Epic B","status":"todo","priority":"medium","labels":["feature"],
        "schedule":{"due_date":FAR,"milestone":"M2"}}),
    );
    for i in 1..=6 {
        let status = match i {
            1 | 2 | 5 => "todo",
            3 => "done",
            4 => "skipped",
            _ => "in_progress",
        };
        p.task(
            json!({"id":format!("t2.{i}"),"title":format!("B child {i}"),"status":status,
            "priority":"medium","schedule":{"due_date":FAR,"milestone":"M2"}}),
        );
    }
    p.task(json!({"id":"t3","title":"Epic C all done","status":"done","priority":"low"}));
    for i in 1..=3 {
        p.task(json!({"id":format!("t3.{i}"),"title":format!("C child {i}"),"status":"done","priority":"low"}));
    }
    for i in 4..=15 {
        let status = if i % 3 == 2 { "done" } else { "todo" };
        let priority = if i % 2 == 0 { "high" } else { "low" };
        let due = if i % 4 == 0 { OLD } else { FAR };
        p.task(
            json!({"id":format!("t{i}"),"title":format!("Standalone {i}"),"status":status,
            "priority":priority,"schedule":{"due_date":due}}),
        );
    }
    p.close_previous_session(vec!["t1.2".into(), "t2.1".into(), "t2.2".into()]);
    p
}

/// 300 tasks: 299 todo and one `in_progress` parent with 240 children; the
/// previous session lists 299 of them (`todo300.json`).
fn todo300_project() -> Project {
    let mut p = Project::new("golden-todo300");
    p.task(
        json!({"id":"t1","title":"Big parent","status":"in_progress","priority":"high",
        "schedule":{"due_date":FAR}}),
    );
    for i in 1..=240u32 {
        let mut schedule = json!({"due_date": if i % 7 == 0 { OLD } else { FAR }});
        if i % 10 == 0 {
            schedule["milestone"] = json!("M1");
        }
        p.task(
            json!({"id":format!("t1.{i}"),"title":format!("Big child {i}"),"status":"todo",
            "priority":priority_of(i),"schedule":schedule}),
        );
    }
    for i in 2..=60u32 {
        p.task(
            json!({"id":format!("t{i}"),"title":format!("Flat todo {i}"),"status":"todo",
            "priority":priority_of(i),"schedule":{"due_date":FAR}}),
        );
    }
    let related: Vec<String> = (1..=240)
        .map(|i| format!("t1.{i}"))
        .chain((2..=60).map(|i| format!("t{i}")))
        .collect();
    p.close_previous_session(related);
    p
}

fn golden(name: &str) -> Value {
    let text = match name {
        "small" => include_str!("fixtures/load_context_golden_v0472/small.json"),
        "todo300" => include_str!("fixtures/load_context_golden_v0472/todo300.json"),
        other => panic!("unknown golden {other}"),
    };
    serde_json::from_str(text).expect("golden JSON")
}

fn node_ids(nodes: &Value) -> Vec<String> {
    fn walk(nodes: &Value, out: &mut Vec<String>) {
        for n in nodes.as_array().into_iter().flatten() {
            out.push(n["id"].as_str().unwrap().to_string());
            walk(&n["children"], out);
        }
    }
    let mut out = Vec::new();
    walk(nodes, &mut out);
    out
}

fn reason_of(tree: &Value, id: &str) -> Option<String> {
    fn find(nodes: &Value, id: &str) -> Option<Value> {
        for n in nodes.as_array().into_iter().flatten() {
            if n["id"] == id {
                return Some(n.clone());
            }
            if let Some(found) = find(&n["children"], id) {
                return Some(found);
            }
        }
        None
    }
    find(tree, id).and_then(|n| n["scope_reason"].as_str().map(str::to_string))
}

fn count_reasons(tree: &Value) -> std::collections::BTreeMap<String, usize> {
    fn walk(nodes: &Value, out: &mut std::collections::BTreeMap<String, usize>) {
        for n in nodes.as_array().into_iter().flatten() {
            if let Some(r) = n["scope_reason"].as_str() {
                *out.entry(r.to_string()).or_insert(0) += 1;
            }
            walk(&n["children"], out);
        }
    }
    let mut out = std::collections::BTreeMap::new();
    walk(tree, &mut out);
    out
}

// --- full == v0.47.2 --------------------------------------------------------

#[test]
fn full_mode_matches_the_v0472_golden_small() {
    let mut p = small_project();
    let got = p.load(json!({ "task_tree": "full" }));
    let want = golden("small");
    assert_eq!(got["task_tree"], want["task_tree"], "task_tree");
    assert_eq!(got["task_summary"], want["task_summary"], "task_summary");
}

#[test]
fn full_mode_matches_the_v0472_golden_todo300() {
    let mut p = todo300_project();
    let got = p.load(json!({ "task_tree": "full" }));
    let want = golden("todo300");
    assert_eq!(got["task_tree"], want["task_tree"], "task_tree");
    assert_eq!(got["task_summary"], want["task_summary"], "task_summary");
    assert!(
        reason_of(&got["task_tree"], "t1").is_none(),
        "full nodes carry no scope_reason"
    );
    assert_eq!(count_reasons(&got["task_tree"]).len(), 0);
}

#[test]
fn full_mode_meta_reports_nothing_omitted() {
    let mut p = small_project();
    let got = p.load(json!({ "task_tree": "full" }));
    let meta = &got["task_tree_meta"];
    assert_eq!(meta["mode"], "full");
    assert_eq!(meta["omitted_total"], 0);
    assert_eq!(meta["truncated"], false);
}

#[test]
fn by_status_keys_are_in_the_same_ascending_order_on_every_load() {
    let mut p = small_project();
    for _ in 0..5 {
        let got = p.load(json!({}));
        let keys: Vec<&String> = got["task_summary"]["by_status"]
            .as_object()
            .unwrap()
            .keys()
            .collect();
        let mut sorted = keys.clone();
        sorted.sort();
        assert_eq!(keys, sorted, "by_status keys must be ascending");
    }
}

// --- compact ----------------------------------------------------------------

#[test]
fn compact_is_the_default_and_matches_the_explicit_compact_mode() {
    let mut p = small_project();
    let default = p.load(json!({}));
    let explicit = p.load(json!({ "task_tree": "compact" }));
    assert_eq!(default["task_tree_meta"]["mode"], "compact");
    assert_eq!(default["task_tree"], explicit["task_tree"]);
    assert_eq!(default["task_tree_meta"], explicit["task_tree_meta"]);
    assert_eq!(
        p.load(json!({ "task_tree": null }))["task_tree"],
        default["task_tree"]
    );
}

#[test]
fn compact_output_of_a_300_todo_project_stays_within_the_size_cap() {
    let mut p = todo300_project();
    let got = p.load(json!({}));
    for key in ["task_tree", "task_tree_meta", "task_summary"] {
        assert!(got.get(key).is_some(), "{key} missing");
    }
    let tree_bytes = serde_json::to_string(&got["task_tree"]).unwrap().len();
    assert!(
        tree_bytes <= SIZE_CAP_BYTES,
        "task_tree is {tree_bytes} bytes (cap {SIZE_CAP_BYTES})"
    );
    let meta = &got["task_tree_meta"];
    assert_eq!(meta["truncated"], true);
    assert!(meta["omitted_total"].as_u64().unwrap() >= 1);
    assert_eq!(
        got["task_summary"]["total"], 300,
        "the summary counts every task"
    );

    // The polluted previous session (299 related ids) does not flood the guidance.
    let related = got["session_guidance"]["suggested_fields"]["related_task_ids"]
        .as_array()
        .expect("related_task_ids");
    assert!(related.len() <= 20, "{} related ids", related.len());
    assert_eq!(
        got["session_guidance"]["suggested_fields"]["related_task_ids_truncated"],
        true
    );
}

#[test]
fn compact_meta_adds_up_to_the_summary() {
    let mut p = todo300_project();
    let got = p.load(json!({}));
    let shown = node_ids(&got["task_tree"]).len() as u64;
    let meta = &got["task_tree_meta"];
    assert_eq!(meta["omitted_total"].as_u64().unwrap() + shown, 300);
    let by_status_sum: u64 = meta["omitted_by_status"]
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(by_status_sum, meta["omitted_total"].as_u64().unwrap());
    let hint = meta["hint"].as_str().unwrap();
    for word in ["handoff_list_tasks", "ids", "task_tree", "full"] {
        assert!(hint.contains(word), "{word} missing from hint: {hint}");
    }
}

#[test]
fn compact_selects_active_and_previous_session_tasks_and_labels_them() {
    let mut p = small_project();
    let got = p.load(json!({}));
    let tree = &got["task_tree"];
    for id in ["t1", "t1.2", "t1.2.2", "t1.3", "t2.6"] {
        assert_eq!(reason_of(tree, id).as_deref(), Some("active"), "{id}");
    }
    assert_eq!(reason_of(tree, "t2.1").as_deref(), Some("previous_session"));
    assert_eq!(reason_of(tree, "t2.2").as_deref(), Some("previous_session"));
    // A done task is only ever an ancestor.
    assert!(reason_of(tree, "t3").is_none());
    assert!(!node_ids(tree).contains(&"t3.1".to_string()));
}

#[test]
fn compact_task_limit_caps_everything_but_the_active_tasks() {
    let mut p = todo300_project();
    p.set_config("settings.compact_task_limit", json!(5));
    let got = p.load(json!({}));
    let reasons = count_reasons(&got["task_tree"]);
    assert_eq!(reasons.get("active"), Some(&1), "{reasons:?}");
    let others: usize = reasons
        .iter()
        .filter(|(r, _)| r.as_str() != "active" && r.as_str() != "scope")
        .map(|(_, n)| n)
        .sum();
    assert_eq!(others, 5, "{reasons:?}");
}

impl Project {
    /// Create a task under `parent_id` (a real subtask: it lives in its
    /// parent's directory and the ids are assigned by the server).
    fn subtask(&mut self, parent_id: &str, title: &str, status: &str) {
        self.server.call(
            "handoff_update_task",
            json!({
                "project_dir": &self.pd,
                "parent_id": parent_id,
                "task": { "title": title, "status": status, "priority": "low",
                          "schedule": { "due_date": FAR } },
            }),
        );
    }

    /// `t1` (todo) with `t1.1`, `t1.2` (which has `t1.2.1` and a done
    /// `t1.2.2`) and 28 more children; `t2` is an unrelated top-level todo.
    fn nested(compact_task_limit: u32) -> Project {
        let mut p = Project::new("nested");
        p.set_config("settings.compact_task_limit", json!(compact_task_limit));
        p.task(
            json!({"title":"Epic","status":"todo","priority":"low","schedule":{"due_date":FAR}}),
        );
        p.task(
            json!({"title":"Other","status":"todo","priority":"low","schedule":{"due_date":FAR}}),
        );
        p.subtask("t1", "first", "todo");
        p.subtask("t1", "second", "todo");
        p.subtask("t1.2", "grandchild", "todo");
        p.subtask("t1.2", "finished grandchild", "done");
        for i in 3..=30 {
            p.subtask("t1", &format!("child {i}"), "todo");
        }
        p
    }
}

#[test]
fn task_scope_includes_the_named_subtree_beyond_the_limit() {
    let mut p = Project::nested(0);
    let got = p.load(json!({ "task_scope": ["t1"] }));
    let reasons = count_reasons(&got["task_tree"]);
    // t1 and its 30 children and the grandchild; the finished one is not.
    assert_eq!(reasons.get("scope"), Some(&32), "{reasons:?}");
    assert_eq!(reasons.len(), 1, "only scope: {reasons:?}");
    assert!(!node_ids(&got["task_tree"]).contains(&"t1.2.2".to_string()));
    assert!(!node_ids(&got["task_tree"]).contains(&"t2".to_string()));
}

#[test]
fn task_scope_naming_a_subtask_keeps_its_ancestors_as_plain_nodes() {
    let mut p = Project::nested(0);
    let got = p.load(json!({ "task_scope": ["t1.2"] }));
    let tree = &got["task_tree"];
    assert_eq!(reason_of(tree, "t1.2").as_deref(), Some("scope"));
    assert_eq!(reason_of(tree, "t1.2.1").as_deref(), Some("scope"));
    assert_eq!(
        node_ids(tree),
        ["t1", "t1.2", "t1.2.1"].map(String::from).to_vec()
    );
    assert!(reason_of(tree, "t1").is_none(), "t1 is only an ancestor");
    assert_eq!(tree[0]["children"][0]["id"], "t1.2");
}

#[test]
fn an_unknown_task_scope_id_is_a_warning_not_an_error() {
    let mut p = Project::nested(0);
    let got = p.load(json!({ "task_scope": ["t1.2", "t99999"] }));
    assert_eq!(
        reason_of(&got["task_tree"], "t1.2.1").as_deref(),
        Some("scope")
    );
    let warnings = serde_json::to_string(&got["warnings"]).unwrap();
    assert!(warnings.contains("t99999"), "{warnings}");
}

#[test]
fn sibling_cap_applies_per_parent_in_a_real_hierarchy() {
    let mut p = Project::nested(100);
    p.close_previous_session(vec!["t1.2.1".into()]);
    let got = p.load(json!({}));
    let tree = &got["task_tree"];
    assert_eq!(
        reason_of(tree, "t1.2.1").as_deref(),
        Some("previous_session")
    );
    // t1.2 has no other unfinished child, so the sibling rule adds nothing.
    assert_eq!(count_reasons(tree).get("sibling"), None);
    // Its ancestors are present, in the original order.
    let ids = node_ids(tree);
    assert_eq!(ids[0], "t1");
    assert!(ids.contains(&"t1.2".to_string()), "{ids:?}");
}

#[test]
fn full_mode_lists_the_whole_hierarchy_with_children_nested() {
    let mut p = Project::nested(0);
    let got = p.load(json!({ "task_tree": "full" }));
    assert_eq!(node_ids(&got["task_tree"]).len(), 34);
    let children = got["task_tree"][0]["children"].as_array().unwrap();
    assert_eq!(children.len(), 30);
    let second = children.iter().find(|c| c["id"] == "t1.2").unwrap();
    assert_eq!(node_ids(&second["children"]), ["t1.2.1", "t1.2.2"]);
    assert_eq!(count_reasons(&got["task_tree"]).len(), 0);
}

#[test]
fn an_unknown_task_tree_value_is_rejected_with_the_allowed_values() {
    let mut p = small_project();
    let (is_error, text) = p.server.call_raw(
        "handoff_load_context",
        json!({ "project_dir": &p.pd, "task_tree": "everything" }),
    );
    assert!(is_error, "{text}");
    assert!(text.contains("compact") && text.contains("full"), "{text}");
}

#[test]
fn related_task_ids_do_not_list_plain_todos_in_either_mode() {
    let mut p = small_project();
    for mode in ["compact", "full"] {
        let got = p.load(json!({ "task_tree": mode }));
        let related: Vec<String> = got["session_guidance"]["suggested_fields"]["related_task_ids"]
            .as_array()
            .expect("related_task_ids")
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        // In flight (tree order), then the unfinished previous-session tasks.
        assert!(related.contains(&"t1".to_string()), "{mode}: {related:?}");
        assert!(related.contains(&"t2.1".to_string()), "{mode}: {related:?}");
        assert!(
            !related.contains(&"t4".to_string()),
            "{mode}: plain todo listed"
        );
        assert!(
            got["session_guidance"]["suggested_fields"]
                .get("related_task_ids_truncated")
                .is_none(),
            "{mode}: nothing was cut"
        );
    }
}

// --- git --------------------------------------------------------------------

fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .args([
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

#[test]
fn the_branch_name_and_recent_commits_select_tasks() {
    let mut p = Project::new("branch");
    git(&p.dir, &["init", "-q", "-b", "fix/t2-something"]);
    git(
        &p.dir,
        &["commit", "-q", "--allow-empty", "-m", "feat(t3.1): start"],
    );
    for (id, title) in [
        ("t1", "Other"),
        ("t2", "Named by the branch"),
        ("t3", "Parent"),
    ] {
        p.task(json!({"id":id,"title":title,"status":"todo","priority":"low","schedule":{"due_date":FAR}}));
    }
    p.task(
        json!({"id":"t3.1","title":"Named by a commit","status":"todo","priority":"low",
        "schedule":{"due_date":FAR}}),
    );
    let got = p.load(json!({}));
    assert_eq!(
        reason_of(&got["task_tree"], "t2").as_deref(),
        Some("branch")
    );
    assert_eq!(
        reason_of(&got["task_tree"], "t3.1").as_deref(),
        Some("branch")
    );
    assert_eq!(
        reason_of(&got["task_tree"], "t3").as_deref(),
        Some("priority"),
        "t3 is not named; it only fits the remaining budget"
    );
}

#[test]
fn outside_a_git_repository_the_rule_selects_nothing_and_does_not_fail() {
    let mut p = Project::new("nogit");
    p.task(
        json!({"id":"t2","title":"A","status":"todo","priority":"low","schedule":{"due_date":FAR}}),
    );
    let got = p.load(json!({}));
    assert_eq!(
        reason_of(&got["task_tree"], "t2").as_deref(),
        Some("priority")
    );
}

#[test]
fn compact_task_limit_is_set_through_update_config_and_rejects_bad_values() {
    let mut p = Project::new("limit");
    let (is_error, reply) = p.server.call_raw(
        "handoff_update_config",
        json!({ "project_dir": &p.pd, "updates": { "settings.compact_task_limit": 12 } }),
    );
    assert!(!is_error, "{reply}");
    assert!(reply.contains("compact_task_limit"), "{reply}");
    let config = std::fs::read_to_string(p.dir.join(".handoff/config.toml")).unwrap();
    assert!(config.contains("compact_task_limit = 12"), "{config}");

    let (is_error, text) = p.server.call_raw(
        "handoff_update_config",
        json!({ "project_dir": &p.pd, "updates": { "settings.compact_task_limit": "many" } }),
    );
    assert!(is_error, "{text}");
    assert!(text.contains("compact_task_limit"), "{text}");
}

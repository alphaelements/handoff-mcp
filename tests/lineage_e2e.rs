//! Real-binary E2E for automatic continuation of a handoff (t439.6,
//! FR-V0473-009): `load_context` picks the previous session without being
//! told, says so (`inferred`, `evidence`) or reports `ambiguous` with
//! candidates, and `save_context` records the choice in `continues`.
//!
//! Drives the actual `handoff-mcp` server over stdio against real git
//! repositories, with sessions seeded as files the way older and newer builds
//! write them.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};

use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use tempfile::TempDir;

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
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
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
            .expect("spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Server {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn call_raw(&mut self, name: &str, arguments: Value) -> (String, bool) {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        });
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush request");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        (text, resp["result"]["isError"].as_bool().unwrap_or(false))
    }

    fn call(&mut self, name: &str, arguments: Value) -> String {
        let (text, is_error) = self.call_raw(name, arguments);
        assert!(!is_error, "{name} failed: {text}");
        text
    }

    fn load(&mut self, pd: &str, extra: Value) -> Value {
        let mut args = json!({ "project_dir": pd });
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            args[k] = v;
        }
        let text = self.call("handoff_load_context", args);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("load_context JSON: {e}\n{text}"))
    }

    fn save(&mut self, pd: &str, extra: Value) -> String {
        // A session file is named after the save's second and summary, so
        // same-second saves need distinct summaries or they share one file.
        static SAVES: AtomicU64 = AtomicU64::new(0);
        let n = SAVES.fetch_add(1, Ordering::Relaxed);
        let mut args = json!({ "project_dir": pd, "summary": format!("e2e save {n}") });
        for (k, v) in extra.as_object().cloned().unwrap_or_default() {
            args[k] = v;
        }
        self.call("handoff_save_context", args)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn git(dir: &Path, args: &[&str]) -> String {
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
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// An empty commit; returns its full hash.
fn commit(dir: &Path, message: &str) -> String {
    git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
    git(dir, &["rev-parse", "HEAD"])
}

struct Project {
    _tmp: TempDir,
    dir: PathBuf,
    pd: String,
}

/// A git repository (branch `main`, one commit) with an initialised `.handoff`.
fn project() -> Project {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    commit(&dir, "init");
    let pd = dir.to_string_lossy().to_string();
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": &pd, "project_name": "lineage-e2e" }),
    );
    Project { _tmp: tmp, dir, pd }
}

fn sessions_dir(p: &Project) -> PathBuf {
    p.dir.join(".handoff/sessions")
}

/// What to seed as a session file.
struct Seed<'a> {
    id: &'a str,
    status: &'a str,
    ended_at: DateTime<Utc>,
    branch: &'a str,
    commit: &'a str,
    /// `None` writes the old format (no `lineage_v` / `continues`).
    continues: Option<&'a [&'a str]>,
    note: &'a str,
    related: &'a [&'a str],
    owner: Option<&'a str>,
    /// The checkout the session was saved from; `None` leaves the field out.
    worktree: Option<&'a str>,
}

impl<'a> Seed<'a> {
    fn new(id: &'a str, ended_at: DateTime<Utc>) -> Self {
        Seed {
            id,
            status: "closed",
            ended_at,
            branch: "main",
            commit: "",
            continues: Some(&[]),
            note: "next step",
            related: &[],
            owner: None,
            worktree: None,
        }
    }
}

fn seed(p: &Project, s: Seed<'_>) {
    let mut body = json!({
        "version": 2,
        "id": s.id,
        "ended_at": s.ended_at.to_rfc3339(),
        "summary": format!("summary of {}", s.id),
        "branch": s.branch,
        "commit": s.commit,
        "handoff_notes": [{ "category": "suggestion", "note": format!("{} {}", s.note, s.id) }],
        "related_task_ids": s.related,
    });
    if let Some(continues) = s.continues {
        body["lineage_v"] = json!(1);
        body["continues"] = json!(continues);
    }
    if let Some(owner) = s.owner {
        body["agent_id"] = json!(owner);
        body["owner_v"] = json!(1);
    }
    if let Some(worktree) = s.worktree {
        body["worktree"] = json!(worktree);
    }
    let name = format!(
        "{}-{}.{}.json",
        s.ended_at.format("%Y%m%d-%H%M%S"),
        s.id,
        s.status
    );
    std::fs::write(
        sessions_dir(p).join(name),
        serde_json::to_string_pretty(&body).unwrap(),
    )
    .unwrap();
}

/// All session files as `(status, json)`.
fn session_files(p: &Project) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(sessions_dir(p)).unwrap() {
        let path = entry.unwrap().path();
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let Some(stem) = name.strip_suffix(".json") else {
            continue;
        };
        let status = stem.rsplit('.').next().unwrap().to_string();
        let v: Value = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        out.push((status, v));
    }
    out
}

fn find_session(p: &Project, id: &str) -> Value {
    session_files(p)
        .into_iter()
        .map(|(_, v)| v)
        .find(|v| v["id"] == id)
        .unwrap_or_else(|| panic!("session {id} not found"))
}

fn sessions_with_status(p: &Project, status: &str) -> Vec<Value> {
    session_files(p)
        .into_iter()
        .filter(|(s, _)| s == status)
        .map(|(_, v)| v)
        .collect()
}

fn ago(minutes: i64) -> DateTime<Utc> {
    Utc::now() - Duration::minutes(minutes)
}

fn previous_id(v: &Value) -> Option<String> {
    v["previous_session"]["id"].as_str().map(String::from)
}

fn suggestions(v: &Value) -> Vec<String> {
    v["next_actions"]
        .as_array()
        .map(|a| a.iter().map(|s| s.as_str().unwrap().to_string()).collect())
        .unwrap_or_default()
}

// --------------------------------------------------------------- AC1 ----

/// A fresh project has nothing to continue: neither a previous session nor an
/// ambiguity.
#[test]
fn fresh_project_reports_neither_previous_nor_ambiguous() {
    let p = project();
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert!(v.get("previous_session").is_none(), "{v}");
    assert!(v.get("ambiguous").is_none(), "{v}");
    assert!(v.get("other_heads").is_none(), "{v}");
}

/// Real-data shape: 20 closed sessions, all ancestors of HEAD, one lineage.
/// Every one of the 21 consecutive (previous, new) pairs is confirmed.
#[test]
fn one_lineage_of_twenty_sessions_confirms_every_pair() {
    let p = project();
    // 20 commits; the old-format sessions alternate between a feature branch
    // (since deleted) and main, as real histories do.
    let hashes: Vec<String> = (0..20).map(|i| commit(&p.dir, &format!("c{i}"))).collect();
    for (i, h) in hashes.iter().enumerate() {
        let id = format!("s-old-{i:02}");
        let mut s = Seed::new(&id, ago(2000 - i as i64 * 10));
        s.continues = None;
        s.branch = if i % 2 == 0 { "feature-x" } else { "main" };
        s.commit = &h[..7];
        seed(&p, s);
    }

    let mut server = Server::spawn();
    let mut expected = "s-old-19".to_string();
    for round in 0..21 {
        let v = server.load(&p.pd, json!({}));
        assert!(v.get("ambiguous").is_none(), "round {round}: {v}");
        assert_eq!(
            previous_id(&v),
            Some(expected.clone()),
            "round {round}: {v}"
        );
        assert_eq!(v["previous_session"]["inferred"], true, "round {round}");
        assert!(v["previous_session"]["evidence"].is_object());
        assert!(v["lineage_token"].is_string());
        // The next closed session continues exactly what was reported.
        let msg = server.save(&p.pd, json!({ "summary": format!("round {round}") }));
        let id = msg
            .lines()
            .find_map(|l| l.strip_prefix("Session ID: "))
            .unwrap()
            .to_string();
        let saved = find_session(&p, &id);
        assert_eq!(
            saved["continues"],
            json!([expected.clone()]),
            "round {round}"
        );
        assert_eq!(saved["lineage_v"], 1);
        assert_eq!(saved["commit"].as_str().unwrap().len(), 40);
        assert_eq!(saved["worktree_key"], "main");
        expected = id;
    }
}

// --------------------------------------------------------------- AC2 ----

/// Two lineages on different branches: the one on the current branch wins and
/// the other lineage's suggestions never reach `next_actions`.
#[test]
fn lineages_on_different_branches_confirm_the_current_branch() {
    let p = project();
    git(&p.dir, &["checkout", "-q", "-b", "feature-a"]);
    let fa = commit(&p.dir, "work on a");
    git(&p.dir, &["checkout", "-q", "main"]);
    git(&p.dir, &["checkout", "-q", "-b", "feature-b"]);
    let fb = commit(&p.dir, "work on b");

    let mut a = Seed::new("s-a", ago(30));
    a.branch = "feature-a";
    a.commit = &fa;
    a.note = "lineage A:";
    seed(&p, a);
    let mut b = Seed::new("s-b", ago(20));
    b.branch = "feature-b";
    b.commit = &fb;
    b.note = "lineage B:";
    seed(&p, b);

    let mut server = Server::spawn();
    // B is the more recent, A is on the checked-out branch.
    git(&p.dir, &["checkout", "-q", "feature-a"]);
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-a".into()), "{v}");
    assert_eq!(suggestions(&v), vec!["lineage A: s-a".to_string()], "{v}");
    assert_eq!(v["previous_session"]["inferred"], true);
    assert_eq!(v["previous_session"]["evidence"]["candidates"], 2);

    git(&p.dir, &["checkout", "-q", "feature-b"]);
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-b".into()), "{v}");
    assert_eq!(suggestions(&v), vec!["lineage B: s-b".to_string()], "{v}");
}

// --------------------------------------------------------------- AC3 ----

fn two_lineages_on_main(p: &Project) {
    let c1 = commit(&p.dir, "one");
    let c2 = commit(&p.dir, "two");
    let mut a = Seed::new("s-a", ago(30));
    a.commit = &c2; // at HEAD
    seed(p, a);
    let mut b = Seed::new("s-b", ago(20));
    b.commit = &c1; // one commit behind
    seed(p, b);
}

/// Same branch, same checkout, nothing else to tell them apart: the server
/// says so and lists candidates without bodies instead of guessing.
#[test]
fn lineages_on_one_branch_are_ambiguous_with_candidates() {
    let p = project();
    two_lineages_on_main(&p);

    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
    assert!(v.get("previous_session").is_none(), "{v}");
    assert!(v.get("next_actions").is_none(), "{v}");
    assert!(v.get("lineage_token").is_none(), "{v}");
    assert!(
        v["message"].as_str().unwrap().contains("continue_from"),
        "{v}"
    );
    let heads = v["other_heads"].as_array().unwrap();
    assert_eq!(heads.len(), 2);
    let allowed = [
        "id",
        "summary",
        "label",
        "branch",
        "related_task_ids",
        "ended_at",
        "in_flight_by",
        "stale",
    ];
    for h in heads {
        for key in h.as_object().unwrap().keys() {
            assert!(allowed.contains(&key.as_str()), "unexpected key {key}");
        }
        assert!(h["id"].is_string() && h["summary"].is_string());
    }
    let guidance = &v["session_guidance"];
    assert!(guidance.get("suggested_fields").is_none(), "{guidance}");
    assert!(v
        .get("warnings")
        .is_none_or(|w| !w.to_string().contains("INCOMPLETE_TASKS")));
}

/// `continue_from` settles it: no scoring, `explicit`, and the next creation
/// records that choice.
#[test]
fn continue_from_overrides_and_is_recorded_by_the_next_save() {
    let p = project();
    two_lineages_on_main(&p);
    let mut server = Server::spawn();

    let v = server.load(&p.pd, json!({ "continue_from": "s-b" }));
    assert_eq!(previous_id(&v), Some("s-b".into()), "{v}");
    assert_eq!(v["previous_session"]["inferred"], false);
    assert_eq!(v["previous_session"]["explicit"], true);
    assert!(v.get("ambiguous").is_none());
    assert!(v["lineage_token"].is_string());

    let msg = server.save(&p.pd, json!({ "session_status": "active" }));
    let id = msg
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap();
    assert_eq!(find_session(&p, id)["continues"], json!(["s-b"]), "{msg}");
    assert!(!msg.contains("continues_uncertain"), "{msg}");
}

/// An ambiguous load followed by a save with no explicit choice records the
/// best candidate and says it was a guess.
#[test]
fn ambiguous_creation_records_best_candidate_as_uncertain() {
    let p = project();
    two_lineages_on_main(&p);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true);

    let msg = server.save(&p.pd, json!({ "session_status": "active" }));
    assert!(msg.contains("continues_uncertain: true"), "{msg}");
    let id = msg
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap();
    // `s-a` is at HEAD (6 points) and outranks `s-b` (5).
    assert_eq!(find_session(&p, id)["continues"], json!(["s-a"]), "{msg}");
}

/// An explicit `continues` wins over the retained choice, an empty one starts
/// a new lineage, and an unknown id is kept with a warning.
#[test]
fn explicit_continues_overrides_everything() {
    let p = project();
    two_lineages_on_main(&p);
    let mut server = Server::spawn();
    server.load(&p.pd, json!({ "continue_from": "s-a" }));

    let msg = server.save(&p.pd, json!({ "continues": ["s-b"] }));
    let id = msg
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap()
        .to_string();
    assert_eq!(find_session(&p, &id)["continues"], json!(["s-b"]));

    // The explicit choice also used up the retained one.
    let msg = server.save(&p.pd, json!({ "continues": [] }));
    let id = msg
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap();
    assert_eq!(find_session(&p, id)["continues"], json!([]));

    let msg = server.save(&p.pd, json!({ "continues": ["s-nope"] }));
    assert!(msg.contains("s-nope"), "{msg}");
    assert!(msg.contains("does not match"), "{msg}");
}

// --------------------------------------------------------------- AC4 ----

/// The retained previous session is used once: a second creation decides for
/// itself and continues the first.
#[test]
fn retained_previous_session_is_used_once() {
    let p = project();
    let mut s = Seed::new("s-prev", ago(10));
    s.continues = None;
    seed(&p, s);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-prev".into()));

    let first = server.save(&p.pd, json!({}));
    let first_id = first
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap()
        .to_string();
    assert_eq!(find_session(&p, &first_id)["continues"], json!(["s-prev"]));

    let second = server.save(&p.pd, json!({}));
    let second_id = second
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap();
    assert_eq!(
        find_session(&p, second_id)["continues"],
        json!([first_id]),
        "the second creation continues the first, not the retained value"
    );
}

/// A second `load_context` replaces the retained value.
#[test]
fn a_later_load_replaces_the_retained_value() {
    let p = project();
    let mut a = Seed::new("s-a", ago(30));
    a.continues = None;
    seed(&p, a);
    let mut server = Server::spawn();
    server.load(&p.pd, json!({}));
    let mut b = Seed::new("s-b", ago(5));
    b.continues = Some(&["s-a"]);
    seed(&p, b);
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-b".into()));

    let msg = server.save(&p.pd, json!({}));
    let id = msg
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap();
    assert_eq!(find_session(&p, id)["continues"], json!(["s-b"]));
}

/// SPEC-V0473-509: every `load_context` replaces the retained value, including
/// one that ends ambiguous. A choice confirmed by an earlier load must not be
/// silently recorded after a later load could no longer tell the lineages apart.
#[test]
fn an_ambiguous_load_drops_the_earlier_retained_value() {
    let p = project();
    let c1 = commit(&p.dir, "one");
    let c2 = commit(&p.dir, "two");
    let mut b = Seed::new("s-b", ago(20));
    b.commit = &c1;
    seed(&p, b);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-b".into()), "{v}");

    // A second lineage appears; `s-a` at HEAD outranks `s-b`, but not by 2.
    let mut a = Seed::new("s-a", ago(30));
    a.commit = &c2;
    seed(&p, a);
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");

    let msg = server.save(&p.pd, json!({ "session_status": "active" }));
    assert!(msg.contains("continues_uncertain: true"), "{msg}");
    let id = msg
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap();
    assert_eq!(
        find_session(&p, id)["continues"],
        json!(["s-a"]),
        "the stale retained s-b must not be used: {msg}"
    );
}

/// A `lineage_token` selects its own load result; an unknown one is ignored
/// with a warning.
#[test]
fn lineage_token_is_honoured_and_unknown_ones_warn() {
    let p = project();
    let mut a = Seed::new("s-a", ago(30));
    a.continues = None;
    seed(&p, a);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    let token = v["lineage_token"].as_str().unwrap().to_string();

    let bad = server.save(&p.pd, json!({ "lineage_token": "lt-bogus" }));
    assert!(bad.contains("lt-bogus"), "{bad}");
    assert!(bad.contains("unknown or already used"), "{bad}");

    // The bogus token did not consume the real one.
    let again = server.load(&p.pd, json!({}));
    let token = again["lineage_token"]
        .as_str()
        .unwrap_or(&token)
        .to_string();
    let ok = server.save(&p.pd, json!({ "lineage_token": token }));
    assert!(!ok.contains("unknown or already used"), "{ok}");
}

/// `keep_active` updates never touch `continues` and never use the retained value.
#[test]
fn updating_an_active_session_keeps_continues_and_the_retained_value() {
    let p = project();
    let mut a = Seed::new("s-a", ago(30));
    a.continues = None;
    seed(&p, a);
    let mut server = Server::spawn();
    server.load(&p.pd, json!({}));
    let created = server.save(&p.pd, json!({ "session_status": "active" }));
    let id = created
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap()
        .to_string();
    assert_eq!(find_session(&p, &id)["continues"], json!(["s-a"]));
    let before = std::fs::read_dir(sessions_dir(&p)).unwrap().count();

    // Update in place.
    server.save(
        &p.pd,
        json!({ "session_status": "active", "summary": "again" }),
    );
    assert_eq!(find_session(&p, &id)["continues"], json!(["s-a"]));
    assert_eq!(std::fs::read_dir(sessions_dir(&p)).unwrap().count(), before);
}

// --------------------------------------------------------------- AC5 ----

/// Old data is one lineage: ten old closed sessions leave one head.
#[test]
fn old_format_sessions_form_a_single_lineage() {
    let p = project();
    for i in 0..10 {
        let id = format!("s-old-{i}");
        let mut s = Seed::new(&id, ago(500 - i * 10));
        s.continues = None;
        s.branch = if i % 3 == 0 { "feature-q" } else { "main" };
        seed(&p, s);
    }
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-old-9".into()), "{v}");
    assert!(v.get("ambiguous").is_none());
}

/// A head that another agent's active session already continues is shown with
/// `in_flight_by` and never confirmed, even when it is the only candidate.
#[test]
fn head_continued_by_another_agents_active_is_ambiguous_with_in_flight_by() {
    let p = project();
    let mut a = Seed::new("s-a", ago(30));
    a.continues = Some(&[]);
    seed(&p, a);
    let mut other = Seed::new("s-other-active", ago(5));
    other.status = "active";
    other.continues = Some(&["s-a"]);
    other.owner = Some("agent-someone-else");
    seed(&p, other);

    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
    assert!(v.get("previous_session").is_none());
    assert_eq!(
        v["other_heads"][0]["in_flight_by"],
        json!(["s-other-active"])
    );
    let message = v["message"].as_str().unwrap();
    assert!(message.contains("adopt"), "{v}");
    // One candidate: the text must not talk about "more than one".
    assert!(!message.contains("More than one"), "{v}");
}

/// The `adopt` hint belongs to a session of the caller's own checkout (restart,
/// `--continue`, reconnect). One that lives in another checkout is not the
/// caller's to adopt, so the text does not suggest it.
#[test]
fn in_flight_session_of_another_checkout_gets_no_adopt_hint() {
    let p = project();
    seed(&p, Seed::new("s-a", ago(30)));
    let mut other = Seed::new("s-other-active", ago(5));
    other.status = "active";
    other.continues = Some(&["s-a"]);
    other.owner = Some("agent-someone-else");
    other.worktree = Some("/somewhere/else");
    seed(&p, other);

    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
    assert_eq!(
        v["other_heads"][0]["in_flight_by"],
        json!(["s-other-active"])
    );
    let message = v["message"].as_str().unwrap();
    assert!(!message.contains("adopt"), "{v}");
    assert!(!message.contains("More than one"), "{v}");
}

/// AC2 under real concurrency: another agent is active on lineage A, this
/// caller works on feature-b. Lineage B wins on branch and commit, so it is
/// confirmed even though A's head is in flight.
#[test]
fn clear_winner_is_confirmed_while_the_other_lineage_is_in_flight() {
    let p = project();
    git(&p.dir, &["checkout", "-q", "-b", "feature-a"]);
    let fa = commit(&p.dir, "work on a");
    git(&p.dir, &["checkout", "-q", "main"]);
    git(&p.dir, &["checkout", "-q", "-b", "feature-b"]);
    let fb = commit(&p.dir, "work on b");

    let mut a = Seed::new("s-a", ago(30));
    a.branch = "feature-a";
    a.commit = &fa;
    seed(&p, a);
    let mut b = Seed::new("s-b", ago(60));
    b.branch = "feature-b";
    b.commit = &fb;
    b.note = "lineage B:";
    seed(&p, b);
    let mut act = Seed::new("s-act-a", ago(5));
    act.status = "active";
    act.continues = Some(&["s-a"]);
    act.owner = Some("agent-a");
    act.branch = "feature-a";
    seed(&p, act);

    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert!(v.get("ambiguous").is_none(), "{v}");
    assert_eq!(previous_id(&v), Some("s-b".into()), "{v}");
    assert_eq!(v["previous_session"]["inferred"], true);
    assert_eq!(suggestions(&v), vec!["lineage B: s-b".to_string()], "{v}");
}

/// The caution stays where it matters: when the in-flight head is the best
/// match for this checkout, it is listed with `in_flight_by` and not confirmed.
#[test]
fn in_flight_head_that_matches_the_checkout_stays_ambiguous() {
    let p = project();
    git(&p.dir, &["checkout", "-q", "-b", "feature-a"]);
    let fa = commit(&p.dir, "work on a");
    git(&p.dir, &["checkout", "-q", "main"]);
    git(&p.dir, &["checkout", "-q", "-b", "feature-b"]);
    let fb = commit(&p.dir, "work on b");
    git(&p.dir, &["checkout", "-q", "feature-a"]);

    let mut a = Seed::new("s-a", ago(30));
    a.branch = "feature-a";
    a.commit = &fa;
    seed(&p, a);
    let mut b = Seed::new("s-b", ago(20));
    b.branch = "feature-b";
    b.commit = &fb;
    seed(&p, b);
    let mut act = Seed::new("s-act-a", ago(5));
    act.status = "active";
    act.continues = Some(&["s-a"]);
    act.owner = Some("agent-a");
    seed(&p, act);

    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
    assert!(v.get("previous_session").is_none(), "{v}");
    let heads = v["other_heads"].as_array().unwrap();
    assert_eq!(heads[0]["id"], "s-a", "{v}");
    assert_eq!(heads[0]["in_flight_by"], json!(["s-act-a"]));
}

/// A load in a linked worktree while the primary checkout has an active
/// session: the worktree session will continue that session, so the report is
/// the primary's own previous session. It is not ambiguous, and no in-flight
/// block applies to the orchestrator's own claim.
#[test]
fn load_in_a_worktree_while_the_primary_is_active_reports_its_previous_session() {
    let p = project();
    seed(&p, Seed::new("s-head", ago(30)));

    let mut orchestrator = Server::spawn();
    let v = orchestrator.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-head".into()), "{v}");
    let created = orchestrator.save(
        &p.pd,
        json!({ "session_status": "active", "summary": "primary work" }),
    );
    let parent_id = created
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap()
        .to_string();
    assert_eq!(
        find_session(&p, &parent_id)["continues"],
        json!(["s-head"]),
        "{created}"
    );

    let wt = p.dir.parent().unwrap().join("wt-loop");
    git(
        &p.dir,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-loop",
            wt.to_str().unwrap(),
        ],
    );
    let wt_pd = std::fs::canonicalize(&wt)
        .unwrap()
        .to_string_lossy()
        .to_string();

    // The same process (same agent) and a different one both start cleanly.
    let mut other_agent = Server::spawn();
    for server in [&mut orchestrator, &mut other_agent] {
        let v = server.load(&wt_pd, json!({}));
        assert!(v.get("ambiguous").is_none(), "{v}");
        assert!(v.get("other_heads").is_none(), "{v}");
        assert_eq!(previous_id(&v), Some("s-head".into()), "{v}");
        assert!(v.get("lineage_token").is_none(), "{v}");
    }

    // The worktree session still records its parent.
    let child = other_agent.save(
        &wt_pd,
        json!({ "session_status": "active", "summary": "worktree work" }),
    );
    let child_id = child
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap();
    assert_eq!(
        find_session(&p, child_id)["continues"],
        json!([parent_id]),
        "{child}"
    );
}

/// Only old heads: ambiguous and flagged stale, one candidate.
#[test]
fn only_stale_heads_are_reported_as_stale_ambiguity() {
    let p = project();
    let mut a = Seed::new("s-old", Utc::now() - Duration::days(45));
    a.continues = Some(&[]);
    seed(&p, a);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
    assert!(v.get("previous_session").is_none());
    assert_eq!(v["other_heads"][0]["stale"], true);
    assert_eq!(v["other_heads"].as_array().unwrap().len(), 1);
}

/// Old-format active sessions are not rewritten with lineage fields.
#[test]
fn updating_an_old_format_active_adds_no_lineage_fields() {
    let p = project();
    let mut act = Seed::new("s-legacy-active", ago(5));
    act.status = "active";
    act.continues = None;
    seed(&p, act);
    let mut server = Server::spawn();
    server.save(
        &p.pd,
        json!({ "session_status": "active", "summary": "touch" }),
    );
    let v = find_session(&p, "s-legacy-active");
    assert!(v.get("lineage_v").is_none(), "{v}");
    assert!(v.get("continues").is_none(), "{v}");
}

// --------------------------------------------------------------- AC6 ----

/// `/clear` keeps the same server process and the same active session: the
/// next `load_context` reports the same previous session, never an ambiguity.
#[test]
fn clear_equivalent_keeps_the_previous_session() {
    let p = project();
    let mut a = Seed::new("s-a", ago(30));
    a.continues = None;
    seed(&p, a);
    let mut server = Server::spawn();
    let first = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&first), Some("s-a".into()));
    server.save(&p.pd, json!({ "session_status": "active" }));

    // A competing head appears meanwhile; the active session pins the answer.
    let mut other = Seed::new("s-other", ago(1));
    other.continues = Some(&[]);
    seed(&p, other);

    let second = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&second), Some("s-a".into()), "{second}");
    assert!(second.get("ambiguous").is_none(), "{second}");
}

// ----------------------------------------------- git state variations ----

/// After a rebase the saved hashes are gone from history: with nothing else to
/// tell two lineages apart the server must not pick one.
#[test]
fn rebase_leaves_two_lineages_ambiguous() {
    let p = project();
    commit(&p.dir, "rewritten");
    let gone1 = "1".repeat(40);
    let gone2 = "2".repeat(40);
    let mut a = Seed::new("s-a", ago(30));
    a.commit = &gone1;
    seed(&p, a);
    let mut b = Seed::new("s-b", ago(20));
    b.commit = &gone2;
    seed(&p, b);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
}

/// Detached HEAD: a branch name proves nothing, the commit does.
#[test]
fn detached_head_uses_the_commit_alone() {
    let p = project();
    let at_head = commit(&p.dir, "work");
    git(&p.dir, &["checkout", "-q", "--detach"]);
    let gone = "3".repeat(40);

    let mut a = Seed::new("s-a", ago(30));
    a.commit = &at_head;
    seed(&p, a);
    let mut b = Seed::new("s-b", ago(20));
    b.commit = &gone;
    seed(&p, b);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-a".into()), "{v}");
    assert_eq!(v["previous_session"]["evidence"]["signals"]["branch"], 0);
}

/// Equal commits and a detached HEAD: nothing separates the lineages.
#[test]
fn detached_head_with_equal_commits_is_ambiguous() {
    let p = project();
    let at_head = commit(&p.dir, "work");
    git(&p.dir, &["checkout", "-q", "--detach"]);
    for (id, mins) in [("s-a", 30), ("s-b", 20)] {
        let mut s = Seed::new(id, ago(mins));
        s.commit = &at_head;
        s.branch = if id == "s-a" { "main" } else { "feature-z" };
        seed(&p, s);
    }
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
}

/// After a squash merge the feature lineage's commits are not in main's
/// history; the main lineage wins and the feature one is not reported.
#[test]
fn squash_merge_confirms_the_main_lineage() {
    let p = project();
    let on_main = commit(&p.dir, "main work");
    git(&p.dir, &["checkout", "-q", "-b", "feature"]);
    let on_feature = commit(&p.dir, "feature work");
    git(&p.dir, &["checkout", "-q", "main"]);
    // Squash: main gets a *different* commit with the feature's content.
    commit(&p.dir, "squashed feature");

    let mut m = Seed::new("s-main", ago(60));
    m.commit = &on_main;
    m.note = "main:";
    seed(&p, m);
    let mut f = Seed::new("s-feature", ago(10));
    f.branch = "feature";
    f.commit = &on_feature;
    f.note = "feature:";
    seed(&p, f);

    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-main".into()), "{v}");
    assert_eq!(suggestions(&v), vec!["main: s-main".to_string()]);
}

/// Task ids named by the branch tell lineages apart.
#[test]
fn branch_named_task_breaks_a_tie() {
    let p = project();
    git(&p.dir, &["checkout", "-q", "-b", "work/t77-thing"]);
    let c = commit(&p.dir, "work");
    let mut a = Seed::new("s-a", ago(30));
    a.branch = "other-a";
    a.commit = &c;
    a.related = &["t77"];
    seed(&p, a);
    let mut b = Seed::new("s-b", ago(20));
    b.branch = "other-b";
    b.commit = &c;
    b.related = &["t12"];
    seed(&p, b);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some("s-a".into()), "{v}");
    assert_eq!(
        v["previous_session"]["evidence"]["signals"]["task_overlap"],
        2
    );
}

// ----------------------------------------------- other creation paths ----

/// `import_context` continues the active session it replaces.
#[test]
fn import_continues_the_active_session_it_closes() {
    let p = project();
    let mut server = Server::spawn();
    server.load(&p.pd, json!({}));
    let created = server.save(&p.pd, json!({ "session_status": "active" }));
    let active_id = created
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap()
        .to_string();

    server.call(
        "handoff_import_context",
        json!({
            "project_dir": &p.pd,
            "source": { "description": "notes", "format": "other" },
            "session": { "summary": "imported" },
        }),
    );
    let opens = sessions_with_status(&p, "open");
    assert_eq!(opens.len(), 1);
    assert_eq!(opens[0]["continues"], json!([active_id]));
    assert_eq!(opens[0]["lineage_v"], 1);
}

/// `fork_session` continues its source.
#[test]
fn fork_continues_its_source() {
    let p = project();
    let mut a = Seed::new("s-src", ago(30));
    a.continues = None;
    seed(&p, a);
    let mut server = Server::spawn();
    let out = server.call(
        "handoff_fork_session",
        json!({ "project_dir": &p.pd, "source_session_id": "s-src", "summary": "forked" }),
    );
    let id = out
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap();
    let forked = find_session(&p, id);
    assert_eq!(forked["continues"], json!(["s-src"]));
    assert_eq!(forked["lineage_v"], 1);
}

// ------------------------------------------------------------ pruning ----

fn pruned_events(p: &Project) -> Vec<Value> {
    std::fs::read_to_string(p.dir.join(".handoff/events.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|e| e["event"] == "session.pruned")
        .collect()
}

/// SPEC-V0473-210: pruning keeps `continues`-based heads and reports the
/// overshoot, which the single-newest interim rule could never produce.
#[test]
fn pruning_keeps_every_head_and_warns_about_the_overshoot() {
    let p = project();
    let t = |m| ago(m);
    let mut a1 = Seed::new("s-a1", t(600));
    a1.continues = Some(&[]);
    seed(&p, a1);
    let mut b1 = Seed::new("s-b1", t(590));
    b1.continues = Some(&[]);
    seed(&p, b1);
    let mut a2 = Seed::new("s-a2", t(580));
    a2.continues = Some(&["s-a1"]);
    seed(&p, a2);
    let mut b2 = Seed::new("s-b2", t(570));
    b2.continues = Some(&["s-b1"]);
    seed(&p, b2);
    let mut c1 = Seed::new("s-c1", t(560));
    c1.continues = Some(&[]);
    seed(&p, c1);
    // An unowned active session, so the save below updates it in place
    // instead of creating another closed one.
    let mut act = Seed::new("s-act", t(5));
    act.status = "active";
    act.continues = Some(&[]);
    seed(&p, act);

    let mut server = Server::spawn();
    server.call(
        "handoff_update_config",
        json!({ "project_dir": &p.pd, "updates": { "settings.session_prune_limit": 2 } }),
    );
    let msg = server.save(&p.pd, json!({ "session_status": "active" }));

    let mut left: Vec<String> = sessions_with_status(&p, "closed")
        .iter()
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    left.sort();
    assert_eq!(left, vec!["s-a2", "s-b2", "s-c1"], "{msg}");
    assert!(
        msg.contains("kept 1 session(s) above session_prune_limit (2) because they are heads"),
        "{msg}"
    );
    assert_eq!(pruned_events(&p).len(), 2);
}

// -------------------------------------------------------- more cases ----

/// A task the previous session was working on warns only when that session is
/// the one reported; an ambiguous load names no previous session and warns about
/// nothing.
#[test]
fn incomplete_tasks_warn_only_for_the_reported_previous_session() {
    let p = project();
    let mut server = Server::spawn();
    let created = server.call(
        "handoff_update_task",
        json!({
            "project_dir": &p.pd,
            "task": { "title": "Open work", "schedule": { "estimate_hours": 1.0 } },
        }),
    );
    let task_id = created
        .trim_start_matches("Created task ")
        .split(':')
        .next()
        .unwrap()
        .to_string();
    server.call(
        "handoff_update_task",
        json!({ "project_dir": &p.pd, "task": { "id": &task_id, "status": "in_progress" } }),
    );

    let mut a = Seed::new("s-a", ago(30));
    let related = [task_id.as_str()];
    a.related = &related;
    seed(&p, a);
    let v = server.load(&p.pd, json!({}));
    assert!(v.to_string().contains("INCOMPLETE_TASKS"), "{v}");

    // A second lineage makes the choice ambiguous: nothing is reported, so no
    // task warning either.
    let mut b = Seed::new("s-b", ago(20));
    b.branch = "main";
    seed(&p, b);
    let v = server.load(&p.pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
    assert!(!v.to_string().contains("INCOMPLETE_TASKS"), "{v}");
}

/// `continue_from` naming no closed session is not an error: it warns and the
/// automatic choice stands.
#[test]
fn unknown_continue_from_warns_and_falls_back_to_automatic_choice() {
    let p = project();
    let mut a = Seed::new("s-a", ago(30));
    a.continues = None;
    seed(&p, a);
    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({ "continue_from": "s-missing" }));
    assert_eq!(previous_id(&v), Some("s-a".into()), "{v}");
    assert_eq!(v["previous_session"]["inferred"], true);
    assert!(v["warnings"].to_string().contains("s-missing"), "{v}");
}

/// Outside git, branch/commit/path signals are all 0 and nothing errors; two
/// equal lineages are simply ambiguous.
#[test]
fn outside_git_two_lineages_are_ambiguous_without_error() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("nogit");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": &pd, "project_name": "nogit" }),
    );
    let p = Project {
        _tmp: tmp,
        dir,
        pd: pd.clone(),
    };
    seed(&p, Seed::new("s-a", ago(30)));
    seed(&p, Seed::new("s-b", ago(20)));
    let v = server.load(&pd, json!({}));
    assert_eq!(v["ambiguous"], true, "{v}");
}

/// A session created in a linked worktree continues its parent (the primary's
/// active session), records the worktree's name, and never competes as a head
/// — nor does it displace its parent.
#[test]
fn worktree_session_continues_its_parent_and_is_not_a_head() {
    let p = project();
    let mut server = Server::spawn();
    server.load(&p.pd, json!({}));
    // Distinct summaries: a session file is named after the save's second and
    // summary, so two same-second saves with one summary would share a file.
    let created = server.save(
        &p.pd,
        json!({ "session_status": "active", "summary": "primary work" }),
    );
    let parent_id = created
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap()
        .to_string();
    assert_eq!(find_session(&p, &parent_id)["worktree_key"], "main");

    let wt = p.dir.parent().unwrap().join("wt-one");
    git(
        &p.dir,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-branch",
            wt.to_str().unwrap(),
        ],
    );
    let wt_pd = std::fs::canonicalize(&wt)
        .unwrap()
        .to_string_lossy()
        .to_string();

    let child = server.save(
        &wt_pd,
        json!({ "session_status": "active", "summary": "worktree work" }),
    );
    let child_id = child
        .lines()
        .find_map(|l| l.strip_prefix("Session ID: "))
        .unwrap()
        .to_string();
    let child_file = find_session(&p, &child_id);
    assert_eq!(
        child_file["continues"],
        json!([parent_id.clone()]),
        "{child}"
    );
    assert_eq!(child_file["parent_session_id"], parent_id.as_str());
    assert_eq!(child_file["worktree_key"], "wt-one");

    // Close both; the parent stays the only head.
    server.call(
        "handoff_save_context",
        json!({ "project_dir": &wt_pd, "summary": "wt done", "close_session_id": &child_id }),
    );
    server.call(
        "handoff_save_context",
        json!({ "project_dir": &p.pd, "summary": "primary done", "close_session_id": &parent_id }),
    );

    let mut other = Server::spawn();
    let v = other.load(&p.pd, json!({}));
    assert_eq!(previous_id(&v), Some(parent_id), "{v}");
}

/// SPEC-V0473-909 AC2: when the caller's own active session cannot name its
/// previous session (its first `continues` entry is gone), the choice is made
/// as if there were no active session, and that active session does not count
/// as "another agent's" in-flight claim on a head.
#[test]
fn own_active_with_a_vanished_continues_is_scored_as_if_absent() {
    let p = project();
    let mut a = Seed::new("s-a", ago(30));
    a.continues = Some(&[]);
    seed(&p, a);
    // Unowned, so this caller treats it as its own. It names a pruned session
    // first and the head second: were it counted as an in-flight claim, the
    // head could never be confirmed.
    let mut act = Seed::new("s-act", ago(5));
    act.status = "active";
    act.continues = Some(&["s-pruned", "s-a"]);
    seed(&p, act);

    let mut server = Server::spawn();
    let v = server.load(&p.pd, json!({}));
    assert!(v.get("ambiguous").is_none(), "{v}");
    assert_eq!(previous_id(&v), Some("s-a".into()), "{v}");
}

/// SPEC-V0473-509 AC6: two checkouts sharing one `.handoff` keep separate
/// retained values — each creation uses the previous session its own
/// checkout's `load_context` settled on, not the other's.
#[test]
fn each_checkout_uses_its_own_retained_previous_session() {
    let p = project();
    let mut a = Seed::new("s-a", ago(30));
    a.continues = None;
    seed(&p, a);
    let wt = p.dir.parent().unwrap().join("wt-two");
    git(
        &p.dir,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "wt-two",
            wt.to_str().unwrap(),
        ],
    );
    let wt_pd = std::fs::canonicalize(&wt)
        .unwrap()
        .to_string_lossy()
        .to_string();

    let mut server = Server::spawn();
    let main_load = server.load(&p.pd, json!({}));
    let wt_load = server.load(&wt_pd, json!({}));
    assert_eq!(previous_id(&main_load), Some("s-a".into()));
    assert_eq!(previous_id(&wt_load), Some("s-a".into()));

    // Each checkout's closed save continues what its own load reported, even
    // though the first save changes which session is the head.
    for pd in [&wt_pd, &p.pd] {
        let msg = server.save(pd, json!({}));
        let id = msg
            .lines()
            .find_map(|l| l.strip_prefix("Session ID: "))
            .unwrap();
        assert_eq!(find_session(&p, id)["continues"], json!(["s-a"]), "{msg}");
    }
}

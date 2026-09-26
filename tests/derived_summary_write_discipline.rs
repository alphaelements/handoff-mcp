//! PR-8 (wiki/240-performance-design.md §6) derived-file write discipline —
//! t370.14, decision 2026-09-27 案(a): derived files
//! (`.handoff/docs/_requirements_summary.json` today) are excluded from
//! `tests/perf_budget.rs`'s PR-8 byte budget and instead held to a separate
//! discipline: (1) unchanged content is never rewritten, (2) at most one
//! write happens per request. This file checks that discipline directly
//! against the real binary (real transport, real filesystem), for the three
//! ops named in the task: `handoff_update_task` (status change on a
//! requirement-linked task), `handoff_doc_verify` (`set_dev_stage`), and
//! `handoff_doc_update_section`.
//!
//! Measurement: `src/mcp/handlers/docs.rs`'s `record_derived_write_for_test`
//! appends one `"{path}\t{bytes}"` line to the file named by
//! `HANDOFF_MCP_DERIVED_WRITE_LOG` every time [it does an actual
//! (non-skipped) derived-file write. Bracketing a single request's slice of
//! that log (line count before/after) gives an exact write count for that
//! request — unlike a `stat` before/after, which can't distinguish "written
//! once" from "written twice back to the same final content" when nothing
//! else changed the file's on-disk stamp in between.
//!
//! Not scaled by `HANDOFF_PERF_SLACK` and not `#[ignore]`d (unlike
//! `tests/perf_budget.rs`): this checks a deterministic write-count
//! invariant, not a timing budget, so it runs as part of the default `cargo
//! test` suite.

#[path = "support/perf_fixture.rs"]
mod perf_fixture;

use perf_fixture::{generate, FixtureOpts};

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    fn spawn(derived_log: &std::path::Path) -> Self {
        let bin = env!("CARGO_BIN_EXE_handoff-mcp");
        let mut child = Command::new(bin)
            .env("HANDOFF_MCP_REQUEST_TIMEOUT_SECS", "900")
            .env("HANDOFF_MCP_DERIVED_WRITE_LOG", derived_log)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn handoff-mcp binary (release build required)");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut client = Client {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        client.rpc(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {}, "clientInfo": {"name": "derived_write_discipline", "version": "0"}}),
        );
        client.notify("notifications/initialized");
        client
    }

    fn notify(&mut self, method: &str) {
        let line = json!({"jsonrpc": "2.0", "method": method}).to_string();
        writeln!(self.stdin, "{line}").expect("write notification");
        self.stdin.flush().expect("flush");
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let id = self.next_id;
        let line =
            json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}).to_string();
        writeln!(self.stdin, "{line}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).expect("read response line");
        serde_json::from_str(&buf)
            .unwrap_or_else(|e| panic!("invalid JSON-RPC response: {e}: {buf}"))
    }

    /// Calls `name`, asserting no error, and returns the response text.
    fn call(&mut self, name: &str, args: Value) -> String {
        let resp = self.rpc("tools/call", json!({"name": name, "arguments": args}));
        let result = resp.get("result").cloned().unwrap_or(Value::Null);
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.get("text").and_then(|t| t.as_str()))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .unwrap_or_default();
        let is_error = resp.get("error").is_some()
            || result
                .get("isError")
                .and_then(|b| b.as_bool())
                .unwrap_or(false);
        assert!(
            !is_error,
            "{name} failed: error={:?} text={}",
            resp.get("error"),
            &text[..text.len().min(400)]
        );
        text
    }

    fn close(mut self) {
        drop(self.stdin);
        let _ = self.child.wait();
    }
}

/// Every derived-file write logged so far, in order.
fn log_entries(path: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|text| text.lines().map(str::to_string).collect())
        .unwrap_or_default()
}

/// Number of derived-file writes the single call `f` caused, by bracketing
/// `log_path`'s line count immediately before/after `f` runs.
fn writes_during(log_path: &std::path::Path, f: impl FnOnce()) -> usize {
    let before = log_entries(log_path).len();
    f();
    log_entries(log_path).len() - before
}

/// `handoff_update_task` (status change on a requirement-linked task, PR-8's
/// `update_task_status_with_links`): transitioning `hot_req_task` to a
/// status that actually changes its derived `dev_stage` (fixture design:
/// `FixtureMeta::hot_req_task`'s doc comment) must write the summary exactly
/// once; repeating the identical status again afterward is not tested here
/// (a same-process `handoff_update_task` call always bumps the task file's
/// own `updated_at`/mtime regardless of whether `status` actually changed,
/// which by design (wiki/240 §4 P-M4: "関係ない文書の保存でも `inputs` が
/// 変わるので書き直す") makes the *next* summary refresh see a changed
/// `inputs` fingerprint and rewrite again — that is not a violation of "at
/// most 1 write per request", since each write belongs to its own request).
#[test]
fn update_task_status_with_links_writes_derived_summary_at_most_once_per_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    let meta = generate(&proj, &FixtureOpts::s()).expect("generate fixture");
    let log_path = tmp.path().join("derived_writes.log");
    let mut client = Client::spawn(&log_path);

    let p = proj.to_string_lossy().to_string();
    let writes = writes_during(&log_path, || {
        client.call(
            "handoff_update_task",
            json!({"project_dir": p, "task": {"id": meta.hot_req_task, "status": "in_progress"}}),
        );
    });
    client.close();

    assert_eq!(
        writes, 1,
        "a single handoff_update_task call that changes hot_req_task's status (and therefore \
         its derived dev_stage) must write _requirements_summary.json exactly once, not {writes}"
    );
}

/// `handoff_doc_verify` `set_dev_stage`: setting a SubItem's dev_stage to a
/// new value must write the summary exactly once per request.
#[test]
fn doc_verify_set_dev_stage_writes_derived_summary_at_most_once_per_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    let meta = generate(&proj, &FixtureOpts::s()).expect("generate fixture");
    let log_path = tmp.path().join("derived_writes.log");
    let mut client = Client::spawn(&log_path);

    let p = proj.to_string_lossy().to_string();
    let writes = writes_during(&log_path, || {
        client.call(
            "handoff_doc_verify",
            json!({
                "project_dir": p, "doc_id": meta.doc_slug, "action": "set_dev_stage",
                "fragment_seq": meta.verify_seq, "sub_item_index": meta.verify_idx_a,
                "dev_stage": "in_progress",
            }),
        );
    });
    client.close();

    assert_eq!(
        writes, 1,
        "a single handoff_doc_verify set_dev_stage call must write \
         _requirements_summary.json exactly once, not {writes}"
    );
}

/// `handoff_doc_update_section` on a *non-layer* document never touches the
/// requirements summary at all (it only edits a section's body text, and
/// `sync_layer_items_if_needed` is a no-op when `doc.layer` is unset —
/// `handle_doc_update_section`, `src/mcp/handlers/docs.rs`). Regression
/// guard: if a future change makes plain section edits refresh the summary
/// too, this catches it exceeding the "at most 1" discipline the moment it
/// exceeds "at most 0". Renamed from `doc_update_section_never_writes_derived_summary`
/// (rework round 2, MAJOR fix): a *layer* document's `doc_update_section`
/// legitimately writes the summary once — see
/// `doc_update_section_on_layer_doc_writes_derived_summary_at_most_once_per_request`
/// below — so this guard needed scoping to the non-layer case specifically.
#[test]
fn doc_update_section_on_non_layer_doc_never_writes_derived_summary() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    let meta = generate(&proj, &FixtureOpts::s()).expect("generate fixture");
    let log_path = tmp.path().join("derived_writes.log");
    let mut client = Client::spawn(&log_path);

    let p = proj.to_string_lossy().to_string();
    let content = format!("## {0}. Section {0}\n\nedited body\n\n", meta.section_seq);
    let writes = writes_during(&log_path, || {
        client.call(
            "handoff_doc_update_section",
            json!({
                "project_dir": p, "doc_id": meta.doc_slug, "seq": meta.section_seq,
                "new_content": content,
            }),
        );
    });
    client.close();

    assert_eq!(
        writes, 0,
        "handoff_doc_update_section on a non-layer document must never write \
         _requirements_summary.json, wrote {writes} times"
    );
}

/// wiki/220 §2.4 step 7 (rework round 2, MAJOR fix): a layer document's
/// `doc_save` that actually re-syncs the matrix (here: the very first
/// `doc_save` the fixture's `layer_doc_slug` document has ever gone
/// through — `generate()` writes it directly via `write_doc`/`write_doc_body`,
/// so `source.body_raw_hash` starts unset and the first real `doc_save` is
/// always treated as changed, per `sync_layer_items_if_needed`'s own doc
/// comment) must write `_requirements_summary.json` exactly once per
/// request, same discipline as every other summary-refreshing op above.
#[test]
fn doc_save_on_layer_doc_writes_derived_summary_at_most_once_per_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    let meta = generate(&proj, &FixtureOpts::s()).expect("generate fixture");
    let log_path = tmp.path().join("derived_writes.log");
    let mut client = Client::spawn(&log_path);

    let p = proj.to_string_lossy().to_string();
    let writes = writes_during(&log_path, || {
        client.call(
            "handoff_doc_save",
            json!({
                "project_dir": p, "doc_id": meta.layer_doc_id, "tags": ["bench-layer"],
            }),
        );
    });
    client.close();

    assert_eq!(
        writes, 1,
        "a layer document's first doc_save (never synced before) must write \
         _requirements_summary.json exactly once, not {writes}"
    );
}

/// Same discipline, via `doc_update_section` on the layer document — its
/// body actually changes (a fresh SPEC item set, mirroring
/// `tests/perf_budget.rs`'s `doc_update_section_layer` op), so this is not
/// relying on the "never synced before" first-call case above to force the
/// resync.
#[test]
fn doc_update_section_on_layer_doc_writes_derived_summary_at_most_once_per_request() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    let meta = generate(&proj, &FixtureOpts::s()).expect("generate fixture");
    let log_path = tmp.path().join("derived_writes.log");
    let mut client = Client::spawn(&log_path);

    let p = proj.to_string_lossy().to_string();
    let content =
        perf_fixture::layer_document_body(meta.layer_lang, perf_fixture::LAYER_BODY_SEED, 1);
    let writes = writes_during(&log_path, || {
        client.call(
            "handoff_doc_update_section",
            json!({
                "project_dir": p, "doc_id": meta.layer_doc_slug, "seq": meta.layer_section_seq,
                "new_content": content,
            }),
        );
    });
    client.close();

    assert_eq!(
        writes, 1,
        "a layer document's doc_update_section (body actually changed) must write \
         _requirements_summary.json exactly once, not {writes}"
    );
}

/// Discipline (1) (wiki/240 §4 P-M4): "内容が変わらないファイルは書かない".
/// `handoff_doc_req_status` is read-only and its own call never mutates any
/// document/task file, so two back-to-back calls with nothing else touching
/// the project in between have byte-identical `DerivedInputs` (P-M4: the
/// fingerprint, not mtime, decides staleness) — the first call computes and
/// writes the summary (it doesn't exist yet), the second must see it as
/// already up to date and skip the write entirely.
#[test]
fn doc_req_status_skips_second_write_when_nothing_changed_in_between() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    generate(&proj, &FixtureOpts::s()).expect("generate fixture");
    let log_path = tmp.path().join("derived_writes.log");
    let mut client = Client::spawn(&log_path);

    let p = proj.to_string_lossy().to_string();
    let first_writes = writes_during(&log_path, || {
        client.call("handoff_doc_req_status", json!({"project_dir": p}));
    });
    let second_writes = writes_during(&log_path, || {
        client.call("handoff_doc_req_status", json!({"project_dir": p}));
    });
    client.close();

    assert_eq!(
        first_writes, 1,
        "the first handoff_doc_req_status call (summary file doesn't exist yet) must write it \
         exactly once, not {first_writes}"
    );
    assert_eq!(
        second_writes, 0,
        "a second handoff_doc_req_status call, with nothing else touching the project in \
         between, must skip the write entirely (unchanged inputs fingerprint) — wrote \
         {second_writes} times"
    );
}

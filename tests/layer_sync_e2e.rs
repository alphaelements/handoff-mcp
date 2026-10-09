//! Real-binary E2E test for M1 layer sync (wiki/220-vmodel-integration-design.md
//! §2.4, t360.6): spawns the actual `handoff-mcp` binary and drives it over
//! real stdio JSON-RPC (same harness style as `tests/stdio_server.rs`), not
//! `process_line` in-process — proving `sync_layer_items` is reachable
//! through the real MCP transport, not just through handler unit tests.
//!
//! Covers the task's required E2E path: a layer document `doc_save` ->
//! `req_list` shows the parsed item -> a direct `.md` hand-edit (removing one
//! item, changing another's title) followed by a metadata-only `doc_save`
//! picks the edit up and reflects it (updated title, removed item + warning)
//! -> the §2.3 write guard refuses a body-owned mutation through
//! `doc_verify`.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
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

    /// Calls an MCP tool over real stdio and returns the parsed JSON payload
    /// (the tool's own JSON response, unwrapped from the JSON-RPC envelope's
    /// `result.content[0].text`).
    fn call(&mut self, name: &str, arguments: Value) -> Value {
        let (is_error, text) = self.call_raw(name, arguments);
        if is_error {
            // Tool errors surface as plain "Error: <message>" text (not
            // JSON) inside `result.content[0].text` with `isError: true`
            // (`src/mcp/handlers/mod.rs`) — normalize to `{"error":
            // {"message": ...}}` so callers can assert on it the same way
            // regardless of transport-level vs. handler-level errors.
            return json!({ "error": { "message": text } });
        }
        serde_json::from_str(&text).unwrap_or(Value::Null)
    }

    /// Like [`Server::call`] but returns the tool's raw `content[0].text`
    /// (for tools such as `handoff_update_task` whose reply is plain text,
    /// not JSON).
    fn call_text(&mut self, name: &str, arguments: Value) -> String {
        self.call_raw(name, arguments).1
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
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn unique_slug(label: &str) -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{label}-{n}")
}

#[test]
fn layer_doc_save_req_list_hand_edit_resync_and_write_guard_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();

    let init = server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "layer-e2e" }),
    );
    assert!(
        init.get("error").is_none() || init["error"].is_null(),
        "init failed: {init}"
    );

    let slug = unique_slug("basic-spec-e2e");
    let body_v1 = "# Basic spec\n\n### SPEC-001 Lockout\n\n- priority: P1\n\nLock after 5 failures.\n\n### SPEC-002 Session timeout\n\nExpire after 30 minutes.\n";

    // 1. doc_save(layer=basic_spec) over real stdio.
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Basic spec (E2E)",
            "body": body_v1,
            "layer": "basic_spec",
        }),
    );
    assert!(saved.get("warnings").is_some(), "unexpected shape: {saved}");
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    // 2. req_list must show both parsed items, addressable by stable_id.
    let list = server.call(
        "handoff_doc_req_list",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let items = list["items"].as_array().expect("items array");
    let ids: Vec<&str> = items
        .iter()
        .filter_map(|i| i["stable_id"].as_str())
        .collect();
    assert!(ids.contains(&"SPEC-001"), "req_list items: {items:?}");
    assert!(ids.contains(&"SPEC-002"), "req_list items: {items:?}");

    // 2b. wiki/220 §2.4 step 7 (rework round 2, MAJOR fix): a layer doc_save
    // that actually syncs the matrix must refresh
    // `_requirements_summary.json` too (not just leave it for the VSCode
    // extension's own fallback aggregation), with SPEC-001 carrying its
    // `category`/`layer` (§2.3).
    let summary_path = dir.join(".handoff/docs").join("_requirements_summary.json");
    let summary: Value =
        serde_json::from_str(&std::fs::read_to_string(&summary_path).unwrap_or_else(|e| {
            panic!("_requirements_summary.json must exist after a layer doc_save: {e}")
        }))
        .expect("summary is valid JSON");
    let summary_items = summary["items"].as_array().expect("summary items array");
    let summary_spec_001 = summary_items
        .iter()
        .find(|i| i["stable_id"] == "SPEC-001")
        .unwrap_or_else(|| panic!("SPEC-001 missing from summary items: {summary_items:?}"));
    assert_eq!(summary_spec_001["category"], "requirement");
    assert_eq!(summary_spec_001["layer"], "basic_spec");

    // 3. Direct .md hand-edit: rename SPEC-001's title, delete SPEC-002
    // entirely (simulating a human editing the body out-of-band).
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = on_disk
        .replace(
            "### SPEC-001 Lockout",
            "### SPEC-001 Lockout after failed logins",
        )
        .replace(
            "\n### SPEC-002 Session timeout\n\nExpire after 30 minutes.\n",
            "\n",
        );
    assert!(
        !edited.contains("### SPEC-002"),
        "hand-edit must have actually removed the SPEC-002 heading from the body \
         (the frontmatter's already-synced `verification` block legitimately still \
         mentions \"SPEC-002\" until the next resync, so this checks the heading \
         specifically rather than the whole file)"
    );
    std::fs::write(&md_path, &edited).unwrap();

    // 4. A metadata-only doc_save (no body/append_body) must pick the
    // hand-edit up (body_raw_hash differs from what was last synced) and
    // re-run sync_layer_items: SPEC-001's title updates, SPEC-002 is dropped
    // with a `removed` warning.
    let resynced = server.call(
        "handoff_doc_save",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": doc_id, "tags": ["e2e"] }),
    );
    let warnings = resynced["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("SPEC-002")),
        "metadata-only save must report SPEC-002 as removed: {warnings:?}"
    );

    let list_after = server.call(
        "handoff_doc_req_list",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let items_after = list_after["items"].as_array().expect("items array");
    let spec_001 = items_after
        .iter()
        .find(|i| i["stable_id"] == "SPEC-001")
        .expect("SPEC-001 must still be present");
    assert_eq!(spec_001["title"], "Lockout after failed logins");
    assert!(
        !items_after.iter().any(|i| i["stable_id"] == "SPEC-002"),
        "SPEC-002 must be gone from req_list after hand-edit + resync: {items_after:?}"
    );

    // 5. §2.3 write guard: `add_item` on this (layer) document is refused
    // through the real transport, directing the caller to edit the body.
    let guard_resp = server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": 0,
            "description": "hand-added via doc_verify",
        }),
    );
    let message = guard_resp
        .get("error")
        .and_then(|e| e.get("message"))
        .and_then(|m| m.as_str())
        .unwrap_or("");
    assert!(
        message.contains("本文を編集"),
        "add_item on a layer doc must be refused with the body-edit guard message: {guard_resp}"
    );
}

/// Reads a task's `task_links` array from `handoff_get_task`'s response,
/// whether the tool returns them at the top level or nested under `task`
/// (both shapes are used across the codebase's handlers/tests).
fn task_links(task_resp: &Value) -> Vec<Value> {
    task_resp["task_links"]
        .as_array()
        .or_else(|| task_resp["task"]["task_links"].as_array())
        .unwrap_or_else(|| panic!("task_links present: {task_resp}"))
        .clone()
}

fn has_requirement_link(links: &[Value], stable_id: &str) -> bool {
    links
        .iter()
        .any(|l| l["link_type"] == "requirement" && l["label"] == stable_id)
}

/// Finds a `SubItem` by `stable_id` in a `handoff_doc_verify_status(include_items:
/// true)` response, across every `items[].sub_items`.
fn find_sub_item_by_stable_id<'a>(status: &'a Value, stable_id: &str) -> &'a Value {
    status["items"]
        .as_array()
        .expect("items array")
        .iter()
        .flat_map(|i| i["sub_items"].as_array().expect("sub_items array"))
        .find(|s| s["stable_id"] == stable_id)
        .unwrap_or_else(|| panic!("stable_id {stable_id} not found in {status}"))
}

/// Rework round 2 (MAJOR fix from the M1 adversarial review): reproduces the
/// reviewer's first repro on the real binary. wiki/220 §2.4 step 6 / §2.5
/// (D3) make the task side the authority for a `requirement`-type link —
/// keyed by `label`/stable_id, with `doc_id` only a hint ("項目が別文書へ移っ
/// ても label で解決"). The pre-fix `sync_layer_items_if_needed` used to call
/// `remove_stale_reverse_links` the instant a body item disappeared from
/// *this* document, which silently deleted t1's link the moment REQ-005 was
/// hand-edited out of the body — even though REQ-005 still exists (just
/// moved to a different document, per the second test below) or comes right
/// back (delete-then-undo). This test drives the removal-in-place case: no
/// move, just REQ-005 disappearing from its own document.
#[test]
fn layer_sync_removing_a_body_item_leaves_the_tasks_reverse_link_intact() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "layer-unlink-e2e" }),
    );

    let slug = unique_slug("reqs-inplace-e2e");
    let body_v1 = "# Requirements\n\n### REQ-001 Keep\n\nStays put.\n\n### REQ-005 Will move away\n\nGets removed from this doc's body.\n";
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Requirements (in-place removal E2E)",
            "layer": "requirement",
            "body": body_v1,
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t1", "title": "Implement REQ-005", "requirement_ids": ["REQ-005"] },
        }),
    );

    let before = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1" }),
    );
    assert!(
        has_requirement_link(&task_links(&before), "REQ-005"),
        "t1 must be linked to REQ-005 right after update_task: {before}"
    );

    // Hand-edit the body to drop REQ-005 entirely, then a metadata-only
    // doc_save picks the edit up and re-syncs (same pattern as the test
    // above).
    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let on_disk = std::fs::read_to_string(&md_path).unwrap();
    let edited = on_disk.replace(
        "\n### REQ-005 Will move away\n\nGets removed from this doc's body.\n",
        "\n",
    );
    assert!(
        !edited.contains("### REQ-005"),
        "hand-edit must have actually removed the REQ-005 heading from the body (the \
         frontmatter's already-synced `verification` block legitimately still mentions \
         \"REQ-005\" until the next resync, so this checks the heading specifically rather \
         than the whole file)"
    );
    std::fs::write(&md_path, &edited).unwrap();

    let resynced = server.call(
        "handoff_doc_save",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": doc_id, "tags": ["e2e"] }),
    );
    let warnings = resynced["warnings"].as_array().expect("warnings array");
    assert!(
        warnings
            .iter()
            .any(|w| w.as_str().unwrap_or("").contains("removed")
                && w.as_str().unwrap_or("").contains("REQ-005")),
        "resync must report REQ-005 as removed: {warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| {
            let w = w.as_str().unwrap_or("");
            w.contains("REQ-005") && w.contains("t1")
        }),
        "resync must inform (not silently drop) that t1 is still linked to the removed \
         REQ-005: {warnings:?}"
    );

    let after = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1" }),
    );
    assert!(
        has_requirement_link(&task_links(&after), "REQ-005"),
        "layer sync must NOT delete t1's reverse link when REQ-005 disappears from the body \
         (the task side is the authority, wiki/220 §2.5 D3): {after}"
    );
}

/// Rework round 2 (MAJOR fix): reproduces the reviewer's second repro on the
/// real binary — moving a requirement from one document to another must not
/// drop the task's link to it. Saves doc B with REQ-005 first, then re-saves
/// doc A without it (the exact sequence from the review finding).
#[test]
fn layer_sync_moving_a_requirement_between_documents_keeps_the_tasks_link() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "layer-move-e2e" }),
    );

    let slug_a = unique_slug("reqs-a-e2e");
    let saved_a = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug_a,
            "title": "Requirements A",
            "layer": "requirement",
            "body": "# Requirements A\n\n### REQ-005 Movable requirement\n\nLives here for now.\n",
        }),
    );
    let doc_a_id = saved_a["doc_id"].as_str().expect("doc_id").to_string();

    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t1", "title": "Implement REQ-005", "requirement_ids": ["REQ-005"] },
        }),
    );
    let before = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1" }),
    );
    assert!(
        has_requirement_link(&task_links(&before), "REQ-005"),
        "t1 must be linked to REQ-005 right after update_task: {before}"
    );

    // Save doc B with REQ-005 (the item's new home) before removing it from
    // doc A, per the review finding's exact repro sequence.
    let slug_b = unique_slug("reqs-b-e2e");
    let saved_b = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug_b,
            "title": "Requirements B",
            "layer": "requirement",
            "body": "# Requirements B\n\n### REQ-005 Movable requirement\n\nNow lives here.\n",
        }),
    );
    let doc_b_id = saved_b["doc_id"].as_str().expect("doc_id").to_string();

    // t360.41 (M-S12 reviewer follow-up): the instant REQ-005 reappears in
    // doc B (a brand-new document, saved while t1's task-side link to
    // REQ-005 already exists via doc A), its SubItem.task_ids must already
    // reflect that link — not stay empty until some later, unrelated task
    // mutation happens to touch it.
    let status_b_immediately_after_move = server.call(
        "handoff_doc_verify_status",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": &doc_b_id, "include_items": true }),
    );
    let req_005_in_b = find_sub_item_by_stable_id(&status_b_immediately_after_move, "REQ-005");
    assert_eq!(
        req_005_in_b["task_ids"].as_array().unwrap(),
        &vec![Value::String("t1".to_string())],
        "REQ-005's task_ids must be restored from the task side immediately upon \
         reappearing in doc B, before doc A's removal or any other task mutation: \
         {status_b_immediately_after_move}"
    );

    // Now remove REQ-005 from doc A's body (metadata+body doc_save, not a
    // hand-edit, to also cover the ordinary `doc_save(body=...)` path rather
    // than only the hand-edit + metadata-only resync path).
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": doc_a_id,
            "body": "# Requirements A\n\nAll requirements have moved elsewhere.\n",
        }),
    );

    let after = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t1" }),
    );
    assert!(
        has_requirement_link(&task_links(&after), "REQ-005"),
        "moving REQ-005 from doc A to doc B must not drop t1's reverse link to it \
         (label/stable_id resolves it regardless of which document currently owns it): {after}"
    );

    // REQ-005 is still resolvable — now via doc B.
    let list = server.call(
        "handoff_doc_req_list",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let items = list["items"].as_array().expect("items array");
    let req_005 = items
        .iter()
        .find(|i| i["stable_id"] == "REQ-005")
        .unwrap_or_else(|| panic!("REQ-005 must still resolve (now via doc B): {items:?}"));
    assert_eq!(
        req_005["doc_id"], doc_b_id,
        "REQ-005 must resolve to its new owning document B, not the deleted-from doc A"
    );
}

/// t392: an unregistered-prefix multi-segment heading (`RQ-VGAP-001`) must
/// surface an "ID-like heading ignored" warning from `doc_save`, and linking a
/// task to that unresolvable id must carry a prefix hint in `update_task`'s
/// "Could not resolve" warning — over the real binary's stdio transport.
#[test]
fn unregistered_multi_segment_prefix_warns_on_doc_save_and_hints_on_update_task() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "prefix-hint-e2e" }),
    );

    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": unique_slug("unregistered-prefix-e2e"),
            "title": "Unregistered prefix (E2E)",
            "layer": "requirement",
            "body": "# Reqs\n\n### RQ-VGAP-001 Unregistered\n\nBody.\n\n### HTTP-2 Support\n\nText.\n\n### REQ-001 Registered\n\nBody.\n",
        }),
    );
    let warnings: Vec<&str> = saved["warnings"]
        .as_array()
        .expect("warnings array")
        .iter()
        .filter_map(|w| w.as_str())
        .collect();
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("ID-like heading ignored") && w.contains("RQ-VGAP-001")),
        "doc_save must warn about RQ-VGAP-001: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|w| w.contains("ID-like heading ignored") && w.contains("HTTP-2")),
        "pre-existing HTTP-2 warning must remain: {warnings:?}"
    );
    assert!(
        !warnings.iter().any(|w| w.contains("\"REQ-001")),
        "registered REQ-001 must not be warned: {warnings:?}"
    );

    let unresolved = server.call_text(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t1", "title": "Link bad id", "requirement_ids": ["RQ-VGAP-001"] },
        }),
    );
    assert!(
        unresolved.contains("Could not resolve requirement stable_id(s): RQ-VGAP-001"),
        "unresolved warning missing: {unresolved}"
    );
    assert!(
        unresolved.contains("the prefix 'RQ' is not in the allowed list")
            && unresolved.contains("requirement=[REQ,FR,NFR]")
            && unresolved.contains("[trace.id_prefixes]"),
        "unresolved warning must carry the prefix hint: {unresolved}"
    );
}

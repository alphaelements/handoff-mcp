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
            .unwrap_or_default();
        if is_error {
            // Tool errors surface as plain "Error: <message>" text (not
            // JSON) inside `result.content[0].text` with `isError: true`
            // (`src/mcp/handlers/mod.rs`) — normalize to `{"error":
            // {"message": ...}}` so callers can assert on it the same way
            // regardless of transport-level vs. handler-level errors.
            return json!({ "error": { "message": text } });
        }
        serde_json::from_str(text).unwrap_or(Value::Null)
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

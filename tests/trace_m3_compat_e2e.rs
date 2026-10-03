//! M2 -> M3 互換性 E2E（t360.40.14, wiki/270-vmodel-m3-design.md §7/§10）:
//! `tests/trace_compat_e2e.rs`（M1->M2）とは別に、M3 固有の2シナリオに限定
//! する:
//!
//! 1. M2 のフィクスチャ（`approval` フィールドなし、`needs`/`assignee` なし）
//!    を M3 バイナリで開き、E12 互換の読み替え（`approval` 欠落 ->
//!    `"draft"`、`needs` 欠落 -> プロファイルの `default_needs`、`assignee`
//!    欠落 -> `None`）が正しいこと。
//! 2. 削除した3機能（`doc_verify(action="link_task")` /
//!    `task_checklist(action="generate")` / `handoff_doc_req_test_sync`）の
//!    ツール名を呼ぶと JSON-RPC のエラーが返ること（個々の「catch-all に合流
//!    する」細部は既存の `tests/doc_verify.rs` / `tests/task_checklist.rs` /
//!    `tests/mcp_protocol.rs` が担うため、ここでは「エラーになる」ことだけを
//!    一本化して確認する）。

use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

use handoff_mcp::storage::docs::model::DocMetadata;
use handoff_mcp::storage::docs::{write_doc, write_doc_body};

fn binary() -> std::path::PathBuf {
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
            use std::io::{BufRead, BufReader};
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
        use std::io::Write;
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

const TS: &str = "2026-09-01T00:00:00.000000000+00:00";

/// Writes one layer document directly via the storage layer's own writer
/// functions (never through `handoff_doc_save`) — starts with no
/// `verification` block at all, exactly the M2 "never synced under M3" state
/// (same technique `tests/trace_compat_e2e.rs` uses for M1 -> M2).
fn write_layer_doc(
    handoff_dir: &Path,
    doc_id: &str,
    slug: &str,
    layer: &str,
    title: &str,
    body: &str,
) {
    let mut doc = DocMetadata::new(
        doc_id.to_string(),
        slug.to_string(),
        title.to_string(),
        "spec".to_string(),
        TS.to_string(),
    );
    doc.layer = Some(layer.to_string());
    write_doc_body(handoff_dir, slug, body).expect("write_doc_body");
    write_doc(handoff_dir, &doc).expect("write_doc");
}

/// シナリオ1: `approval`/`needs`/`assignee` を一切書かない M2 時代の層文書を
/// M3 バイナリの最初の集計呼び出し（`trace_report`）に通すと、E12 互換の
/// 読み替えが効く — `approval` 欠落は `"draft"`、`assignee` 欠落は `null`、
/// `needs` 欠落はプロファイルの `default_needs`（`minimal` なら
/// `acceptance`）による暗黙カバレッジ要求として扱われる。
#[test]
fn m2_fixture_without_approval_needs_assignee_reads_back_with_e12_defaults() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m3-compat-fixture" }),
    );

    // An M2-era requirement document: no `approval`/`needs`/`assignee` lines
    // at all, and never synced by any binary (`verification: None`).
    write_layer_doc(
        &handoff,
        "doc-20260901-000000-0001",
        "req-m3-compat",
        "requirement",
        "Requirements",
        "# Requirements\n\n### REQ-700 Session timeout\n\n\
         - priority: P1\n\n\
         Original requirement statement text.\n",
    );
    write_layer_doc(
        &handoff,
        "doc-20260901-000000-0002",
        "at-m3-compat",
        "acceptance",
        "Acceptance",
        "# Acceptance\n\n### AT-700 Confirms timeout\n\n\
         - verifies: REQ-700\n- method: manual\n\n\
         Original acceptance statement text.\n",
    );

    // The first aggregation call resyncs both never-synced documents in one
    // batch (same migration step `trace_compat_e2e.rs` exercises for M1 ->
    // M2) and is where the M3 binary must apply E12's read-mapping.
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let items = report["items"].as_array().expect("items");
    let req700 = items
        .iter()
        .find(|i| i["id"] == "REQ-700")
        .expect("REQ-700 present after resync");
    assert_eq!(
        req700["approval"], "draft",
        "a never-approved M2 item must read back as draft (E12): {req700}"
    );
    assert!(
        req700["assignee"].is_null(),
        "an M2 item with no assignee line must read back as null: {req700}"
    );
    // `minimal`'s/the default profile's `default_needs` still gates this
    // item's horizontal coverage even though it never wrote a `needs:` line
    // itself — AT-700 (an `acceptance`-layer verifier) satisfies it.
    assert_eq!(
        req700["coverage"]["horizontal"], "covered",
        "an M2 item with no `needs:` line must fall back to the profile's \
         default_needs, which AT-700 (acceptance) already satisfies: {req700}"
    );

    // The on-disk document itself must have been resynced with M3 fields
    // left absent (never invented out of thin air) — only `def_hash`/
    // `layer_sync_stamp`/implicit M2 fields are (re)written by the sync.
    let req_doc = handoff_mcp::storage::docs::read_doc(&handoff, "req-m3-compat")
        .expect("read_doc")
        .expect("doc exists");
    let verification = req_doc
        .verification
        .expect("resync must populate verification");
    let sub_item = verification
        .items
        .iter()
        .flat_map(|item| item.sub_items.iter())
        .find(|s| s.stable_id.as_deref() == Some("REQ-700"))
        .expect("REQ-700 sub_item present after resync");
    assert!(
        sub_item.approval.is_none(),
        "resync must not invent an approval value for an M2 item that never had one: {sub_item:?}"
    );
    assert!(
        sub_item.assignee.is_none(),
        "resync must not invent an assignee for an M2 item that never had one: {sub_item:?}"
    );
    assert!(
        sub_item.needs.is_none(),
        "resync must not invent a needs list for an M2 item that never had one \
         (the profile's default_needs is applied at read-time, not persisted): {sub_item:?}"
    );
}

/// シナリオ2: 削除した3機能（§4.8）のツール名を呼ぶと、個々の詳細は別テスト
/// が担うにせよ、どの呼び出し方でも必ずエラー（`isError: true` または MCP
/// プロトコルの unknown-tool エラー）になる。
#[test]
fn removed_m3_tools_always_error_when_called() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "m3-compat-removed-tools" }),
    );
    server.call(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": "t-m3-compat-removed", "title": "Implement" } }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-m3-compat-removed",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-800 Something\n\nBody.\n",
        }),
    );

    // `doc_verify(action="link_task")` — removed; must error (wiki §4.8,
    // migration target: `handoff_update_task(requirement_ids=...)`).
    let (is_error, text) = server.call_raw(
        "handoff_doc_verify",
        json!({
            "project_dir": pd,
            "doc_id": "req-m3-compat-removed",
            "action": "link_task",
            "task_id": "t-m3-compat-removed",
        }),
    );
    assert!(
        is_error,
        "doc_verify(link_task) must error post-removal: {text}"
    );

    // `task_checklist(action="generate")` — removed; must error (migration
    // target: `handoff_trace_scaffold`).
    let (is_error, text) = server.call_raw(
        "handoff_task_checklist",
        json!({ "project_dir": pd, "task_id": "t-m3-compat-removed", "action": "generate" }),
    );
    assert!(
        is_error,
        "task_checklist(generate) must error post-removal: {text}"
    );

    // `handoff_doc_req_test_sync` — the whole tool was removed; must error
    // via the "tool not implemented" catch-all (migration target:
    // `handoff_trace_ingest(format="cargo_json")`).
    let (is_error, text) =
        server.call_raw("handoff_doc_req_test_sync", json!({ "project_dir": pd }));
    assert!(
        is_error,
        "handoff_doc_req_test_sync must error post-removal: {text}"
    );
}

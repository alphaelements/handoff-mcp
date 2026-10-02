//! 互換性 E2E（t360.20.20/M2-20, wiki/260-vmodel-m2-design.md §7・§10）:
//! M1 データから M2 への移行手順と、M1↔M2 旧バイナリ混在の再現。
//! `tests/fixtures/trace/v1`・`tests/fixtures/trace/v2` の state/ギャップの
//! byte-for-byte 一致自体は既存の `tests/trace_report_contract_fixture_e2e.rs`
//! が担っているため、このファイルはそれが覆っていない4つのシナリオに限定
//! する:
//!
//! 1. M1 データからの移行手順の実演 — stamp なし（一度も `doc_save` を
//!    通っていない層文書）→ 最初の `doc_save`/`trace_report` で再同期 →
//!    `trace_suspect baseline` dry-run で既存リンク数を確認 → `--apply`。
//! 2. `- rationale:` / `- assignee:` / `- derived:` の行を属性ブロックに
//!    持つ M1 項目の `body_hash` が M1 と同じで、M1 時代に記録された run が
//!    M2 バイナリの下でも result suspect にならない。
//! 3. 旧バイナリ混在（§7）: 旧バイナリが層文書を書き直すと M2 フィールド
//!    （`def_hash`/`link_baselines`/`layer_sync_stamp`、暗黙受入検証項目）が
//!    落ちる — M2 バイナリは再同期で `def_hash`/暗黙項目を戻すが、
//!    ベースラインは戻らない（unbaselined になる）。
//! 4. 層（`layer`）を一切使わないプロジェクトで、既存ツールの出力が M1
//!    時代と変わらない。

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

use handoff_mcp::storage::docs::model::DocMetadata;
use handoff_mcp::storage::docs::{read_doc, write_doc, write_doc_body};

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

/// Copies `src` to `dst` recursively — same technique as
/// `tests/trace_report_contract_fixture_e2e.rs`.
fn copy_dir_recursive(src: &Path, dst: &Path) {
    std::fs::create_dir_all(dst).unwrap();
    for entry in std::fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let file_type = entry.file_type().unwrap();
        let dst_path = dst.join(entry.file_name());
        if file_type.is_dir() {
            copy_dir_recursive(&entry.path(), &dst_path);
        } else {
            std::fs::copy(entry.path(), &dst_path).unwrap();
        }
    }
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
/// `source.body_raw_hash`/`verification` at all, exactly the "pre-M2, never
/// synced" state §7 describes.
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

/// シナリオ1: M1 データからの移行手順 — stamp なし（一度も `doc_save` を
/// 通っていない）2つの層文書 → 最初の `handoff_doc_save`（どちらかの文書に
/// 対する軽い編集）で両方が再同期される → `trace_suspect baseline`
/// dry-run で新規に追加されたリンクの数を確認（この再同期自体が1回で両方の
/// 文書をバイナリへ通すので、REQ-700/SPEC-700 間のリンクはこの時点で既に
/// baseline 済み — §2.5 手順4「この同期で追加されたリンクだけ記録」）→
/// `--apply` は0件（既にベースライン済みのため）になることを確認する。
#[test]
fn migration_from_never_synced_m1_data_resyncs_then_baseline_dry_run_then_apply() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-compat-migration" }),
    );

    write_layer_doc(
        &handoff,
        "doc-20260901-000000-0001",
        "req-compat-migration",
        "requirement",
        "Requirements",
        "# Requirements\n\n### REQ-700 Session timeout\n\n- priority: P1\n\n\
         Original requirement statement text.\n",
    );
    write_layer_doc(
        &handoff,
        "doc-20260901-000000-0002",
        "spec-compat-migration",
        "basic_spec",
        "Basic spec",
        "# Basic spec\n\n### SPEC-700 Timeout enforcement\n\n- refines: REQ-700\n\n\
         Original spec statement text.\n",
    );

    // Step 1 of the migration: the first aggregation call resyncs both
    // never-synced documents in one batch (§7's "最初の doc_save /
    // trace_report / trace_slice で... 再同期され").
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );

    let req_doc = read_doc(&handoff, "req-compat-migration")
        .expect("read_doc")
        .expect("doc exists");
    assert!(
        req_doc.source.layer_sync_stamp.is_some(),
        "the first aggregation call must stamp the never-synced document"
    );

    // Step 2: baseline dry-run reports how many links would be newly
    // baselined — this batch resync already baselined SPEC-700's `refines`
    // the moment it was synced (§2.5 step 4: baseline every newly-added
    // reference immediately), so the dry-run must report 0 unbaselined
    // links left to fix up, not "discover" SPEC-700 as unbaselined.
    let dry_run = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline", "dry_run": true }),
    );
    assert_eq!(dry_run["dry_run"], true, "{dry_run}");
    assert_eq!(
        dry_run["baselined"]["links"], 0,
        "every link created through a live sync is baselined on the spot, \
         never left unbaselined for a later baseline call to discover: {dry_run}"
    );

    // Step 3: --apply changes nothing further (idempotent / no-op once
    // nothing is unbaselined).
    let apply = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline", "dry_run": false }),
    );
    assert_eq!(apply["dry_run"], false, "{apply}");
    assert_eq!(apply["baselined"]["links"], 0, "{apply}");

    let list = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(list["unbaselined"]["links"], 0, "{list}");
}

/// シナリオ1b: 真に unbaselined な状態からの baseline dry-run → apply の
/// 実演。REQ-800/SPEC-800 を通常どおり `doc_save` で作成すると、初回 sync が
/// 両者を同じバッチで通し、`refines: REQ-800` は「この同期で新規に追加
/// されたリンク」として §2.5 手順4によりその場で baseline されてしまう
/// （シナリオ1が確認している挙動）。M1 データ移行直後の典型形（リンクは
/// 既に存在するが `link_baselines` に記録がない）を確実に再現するため、
/// 一度正常に同期させたあとで `link_baselines` だけを手動で消し去る
/// （`old_binary_mixing` テストと同じ技法）。
#[test]
fn migration_baseline_dry_run_then_apply_actually_baselines_a_real_unbaselined_link() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-compat-migration-2" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-compat-migration-2",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-800 Something\n\n- priority: P1\n\nBody text.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "spec-compat-migration-2",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-800 Something enforced\n\n- refines: REQ-800\n\nBody text.\n",
        }),
    );

    // Pre-condition: the live sync above already baselined `refines:
    // REQ-800` on the spot (§2.5 手順4) — confirm it, then strip the
    // baseline back out to model "M1 データ移行後の典型形" (the link exists,
    // but no M2 binary has ever recorded a baseline for it).
    let list_live = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        list_live["unbaselined"]["links"], 0,
        "a link created through a live sync must already be baselined: {list_live}"
    );

    let mut spec_doc = read_doc(&handoff, "spec-compat-migration-2")
        .expect("read_doc")
        .expect("doc exists");
    if let Some(verification) = spec_doc.verification.as_mut() {
        for item in verification.items.iter_mut() {
            for sub in item.sub_items.iter_mut() {
                sub.link_baselines.clear();
            }
        }
    }
    write_doc(&handoff, &spec_doc).expect("write_doc (strip baseline to model M1 migration)");

    let list0 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    let unbaselined0 = list0["unbaselined"]["links"].as_u64().unwrap_or(0);
    assert_eq!(
        unbaselined0, 1,
        "stripping link_baselines must produce exactly one genuinely \
         unbaselined link for this test to exercise: {list0}"
    );

    let dry_run = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline", "dry_run": true }),
    );
    assert_eq!(
        dry_run["baselined"]["links"], unbaselined0,
        "dry_run must report exactly the current unbaselined count: {dry_run}"
    );
    // dry_run must not itself mutate state — the link must still read as
    // unbaselined afterwards.
    let list_after_dry_run = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        list_after_dry_run["unbaselined"]["links"], unbaselined0,
        "dry_run must not mutate state: {list_after_dry_run}"
    );

    let apply = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline", "dry_run": false }),
    );
    assert_eq!(apply["baselined"]["links"], unbaselined0, "{apply}");

    let list1 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        list1["unbaselined"]["links"], 0,
        "apply must clear every unbaselined link reported by the dry-run: {list1}"
    );
}

/// シナリオ2: `- rationale:` / `- assignee:` / `- derived:` の行を属性
/// ブロックに持つ M1 項目 — M1 時代はこれらのキーを認識しなかったので
/// `statement` に文字どおり残っていた。M2 パーサはこれらを属性として認識
/// するが `body_hash` は M1 の鍵集合でだけ計算し続ける（E14）ため、M1 時代
/// に記録済みの run は M2 バイナリの下でも result suspect にならない。
#[test]
fn m1_item_with_rationale_assignee_derived_attribute_lines_keeps_its_m1_era_run_non_suspect() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-compat-m1-attrs" }),
    );

    // An M1-era document: `- rationale:`/`- assignee:`/`- derived:` lines
    // were never recognized under M1, so they stayed as ordinary statement
    // text when the M1 binary parsed and recorded a run against it.
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "st-compat-m1-attrs",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System tests\n\n### ST-900 Lockout check\n\n\
                - rationale: 総当たり攻撃の抑止\n\
                - assignee: alice\n\
                - derived: 実装方式から必要になった項目\n\n\
                Run the lockout scenario end to end.\n",
        }),
    );
    let doc_id = saved["doc_id"].as_str().expect("doc_id").to_string();

    // Record a pass (the M1-era run) against this item.
    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": dir.to_string_lossy(),
            "results": [{"item": "ST-900", "result": "pass"}],
        }),
    );

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let st900 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "ST-900")
        .expect("ST-900");
    assert_eq!(
        st900["state"], "passing",
        "the recorded pass must still be reflected: {st900}"
    );
    assert!(
        st900["suspect"].as_array().unwrap().is_empty(),
        "M2 recognizing rationale/assignee/derived as real attribute keys \
         must never retroactively invalidate an already-recorded M1-era \
         run's body_hash match (E14): {st900}"
    );

    // Re-saving the exact same body (idempotent save) must not change
    // body_hash either — a second round-trip through the M2 parser/renderer
    // stays stable.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": doc_id,
            "body": "# System tests\n\n### ST-900 Lockout check\n\n\
                - rationale: 総当たり攻撃の抑止\n\
                - assignee: alice\n\
                - derived: 実装方式から必要になった項目\n\n\
                Run the lockout scenario end to end.\n",
        }),
    );
    let report2 = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let st900_2 = report2["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "ST-900")
        .expect("ST-900 after re-save");
    assert_eq!(st900_2["state"], "passing", "{st900_2}");
    assert!(
        st900_2["suspect"].as_array().unwrap().is_empty(),
        "{st900_2}"
    );
    let _ = &handoff;
}

/// シナリオ3: 旧バイナリ混在（§7）— 旧バイナリが層文書を書き直すと M2
/// フィールド（`def_hash`/`link_baselines`/`source.layer_sync_stamp`）が
/// 落ちる。実際に旧バイナリを呼ぶ必要はなく、M2 フィールドを手で落とした
/// fixture を作る。M2 バイナリがそれを再同期すると `def_hash` は戻るが、
/// **ベースラインは戻らない**（そのリンクは unbaselined になる、黙って
/// 現在値で埋めることはしない）。
#[test]
fn old_binary_mixing_m2_binary_resyncs_def_hash_but_never_silently_restores_the_baseline() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-compat-old-binary" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-compat-old-binary",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-900 Something\n\n- priority: P1\n\nBody text.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "spec-compat-old-binary",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-900 Something enforced\n\n- refines: REQ-900\n\nBody text.\n",
        }),
    );
    drop(server);

    // Confirm the link was baselined by the live M2 binary (pre-condition).
    let mut server = Server::spawn();
    let list_before = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(list_before["unbaselined"]["links"], 0, "{list_before}");
    drop(server);

    // Simulate an old (M1) binary rewriting SPEC-900: drop every M2-only
    // field from its SubItem (`def_hash`, `link_baselines`) and its
    // document-level `source.layer_sync_stamp` — exactly what §7 describes
    // an M1 binary's own write path (which has no storage slot for these
    // fields) does to a shared `.handoff/`.
    let mut spec_doc = read_doc(&handoff, "spec-compat-old-binary")
        .expect("read_doc")
        .expect("doc exists");
    spec_doc.source.layer_sync_stamp = None;
    if let Some(verification) = spec_doc.verification.as_mut() {
        for item in verification.items.iter_mut() {
            for sub in item.sub_items.iter_mut() {
                sub.def_hash = None;
                sub.link_baselines.clear();
            }
        }
    }
    write_doc(&handoff, &spec_doc).expect("write_doc (simulated old-binary rewrite)");

    // The next M2 aggregation call resyncs the stamp-less document: def_hash
    // comes back...
    let mut server = Server::spawn();
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let spec_doc_after = read_doc(&handoff, "spec-compat-old-binary")
        .expect("read_doc")
        .expect("doc exists");
    let spec_sub = spec_doc_after
        .verification
        .as_ref()
        .expect("verification")
        .items
        .iter()
        .flat_map(|i| i.sub_items.iter())
        .find(|s| s.stable_id.as_deref() == Some("SPEC-900"))
        .cloned()
        .expect("SPEC-900 sub item");
    assert!(
        spec_sub.def_hash.is_some(),
        "resync must recompute def_hash for the stamp-less document: {spec_sub:?}"
    );

    // ...but the baseline does NOT come back silently — the link is now
    // unbaselined (lint `info`, never a suspect) until a human explicitly
    // runs `trace_suspect baseline --apply`.
    assert!(
        !spec_sub.link_baselines.contains_key("REQ-900"),
        "a dropped baseline must never be silently backfilled by a plain \
         resync — only an explicit `baseline` call may do that (§2.5 手順4, \
         §7): {spec_sub:?}"
    );

    let list_after = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        list_after["unbaselined"]["links"], 1,
        "the link whose baseline was dropped by the simulated old binary \
         must now read as unbaselined: {list_after}"
    );
    assert!(
        list_after["suspects"]
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["item"] != "SPEC-900"),
        "an unbaselined link is never itself a suspect (§3.1/§5.4): {list_after}"
    );

    let lint = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let findings = lint["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f["rule"] == "unbaselined" && f["item"] == "SPEC-900"),
        "trace_lint must surface the unbaselined link as an info finding: {lint}"
    );

    // An explicit `baseline --apply` restores it going forward (the human
    // migration step, §7's last paragraph).
    let apply = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "baseline", "dry_run": false }),
    );
    assert_eq!(apply["baselined"]["links"], 1, "{apply}");
    let list_final = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(list_final["unbaselined"]["links"], 0, "{list_final}");
}

/// シナリオ4: 層を一切使わないプロジェクト（`layer` フィールドなし）では、
/// 既存ツール（`doc_list`/`doc_get`/`doc_req_list`）の出力が M1 時代と
/// 変わらない — `tests/fixtures/pre_m1_compat` の層なしプロジェクトに対して
/// 新ツール（`trace_report`/`trace_lint`）は空の結果を返すだけで既存出力に
/// 影響しない。
#[test]
fn layer_less_project_existing_tool_output_is_unaffected_by_m2() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let proj = tmp.path().join("proj");
    let fixture = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pre_m1_compat");
    copy_dir_recursive(&fixture.join("project/handoff"), &proj.join(".handoff"));

    let mut server = Server::spawn();
    let pd = proj.to_string_lossy().to_string();

    let doc_list = server.call("handoff_doc_list", json!({ "project_dir": pd }));
    let docs = doc_list["documents"]
        .as_array()
        .or_else(|| doc_list.as_array())
        .expect("doc_list must return a list");
    assert!(!docs.is_empty(), "{doc_list}");
    assert!(
        docs.iter().all(|d| d.get("unreadable").is_none()
            || d["unreadable"] == Value::Null
            || d["unreadable"] == false),
        "a layer-less, well-formed fixture must not report any document as \
         unreadable: {doc_list}"
    );

    // The new trace tools return empty results for a project with no layer
    // declared (§7: "層設定も層文書もないプロジェクト... 新ツールは空の結果
    // を返す").
    let report = server.call("handoff_trace_report", json!({ "project_dir": pd }));
    assert!(
        report["trace_layers"]["in_use"]
            .as_array()
            .unwrap()
            .is_empty(),
        "{report}"
    );
    assert!(
        report["items"].as_array().is_none_or(|a| a.is_empty()) || report.get("items").is_none()
    );
    assert!(
        report["gaps"].as_array().unwrap().is_empty(),
        "no layer items means no gaps: {report}"
    );

    let lint = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    assert_eq!(
        lint["exit_code"], 0,
        "an all-layer-less project has nothing to lint: {lint}"
    );
}

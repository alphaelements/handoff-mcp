//! M2 全体の実バイナリ E2E（t360.20.20/M2-20, wiki/260-vmodel-m2-design.md
//! §10 の E2E 段落）: 個別ツールごとの契約は既存の `tests/trace_*_e2e.rs`
//! 群がそれぞれ担うため、このファイルはそれらを縫い合わせた「M2 全体を
//! 通した1本のシナリオ」に限定する — 重複した網羅は避ける。
//!
//! 1. `minimal` プロファイルの要件文書だけで V が閉じる（暗黙 AC に
//!    record → passing）。
//! 2. `standard` で要件 → 仕様 → システムテスト → タスクを作り、要件の
//!    本文変更で1ホップだけ suspect、仕様変更でさらに孫が suspect になり、
//!    `trace_suspect clear` で解除・監査ファイルが残る。
//! 3. `handoff_trace_update` 1回で項目追加・リンク・dev_stage 変更・結果
//!    記録を行う。
//! 4. `handoff_trace_ingest` に JUnit XML を渡す。
//! 5. `handoff_trace_lint` の終了コードが 0 / 1 / 2 になる（CLI 経由）。
//! 6. 読み取り専用ツール群（`trace_report`/`trace_slice`/`trace_lint`/
//!    `trace_matrix`/`trace_next`/`trace_impact`/`trace_suspect list`/
//!    `get_task`/`list_tasks`）を呼んだ前後で `.handoff/` のバイト列が
//!    1バイトも変わらない。
//! 7. `handoff_trace_next` が優先度順にアクションを返す。

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

fn snapshot(handoff: &Path) -> Vec<(PathBuf, Vec<u8>)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_dir() {
                    walk(&path, out);
                } else {
                    out.push(path);
                }
            }
        }
    }
    let mut paths = Vec::new();
    walk(handoff, &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let bytes = std::fs::read(&p).unwrap();
            (p, bytes)
        })
        .collect()
}

fn suspects_of_kind<'a>(list: &'a Value, kind: &str) -> Vec<&'a Value> {
    list["suspects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] == kind)
        .collect()
}

fn suspect_items(list: &Value, kind: &str) -> Vec<String> {
    let mut ids: Vec<String> = suspects_of_kind(list, kind)
        .into_iter()
        .map(|s| s["item"].as_str().unwrap().to_string())
        .collect();
    ids.sort();
    ids
}

/// §10 シナリオ1: `minimal` プロファイル（`requirement`/`acceptance` の2層、
/// `implicit_acceptance: true`）の要件文書だけで V が閉じる — 受入基準の
/// `AC1` が暗黙の検証項目 `REQ-001#AC1` として実体化し、`trace_update` の
/// `record` でそのまま `pass` を記録でき、`trace_report` の `state` が
/// `passing` になる。
#[test]
fn minimal_profile_closes_the_v_with_only_a_requirement_document() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-e2e-minimal" }),
    );

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-m2-e2e-minimal",
            "title": "Requirements",
            "layer": "requirement",
            "trace_profile": "minimal",
            "body": "# Requirements\n\n### REQ-001 Account lockout\n\n\
                - priority: P1\n\n\
                After 5 consecutive failed logins, the account locks for 15 minutes.\n\n\
                受入基準:\n\
                - AC1: Given 4回失敗済み When 5回目に失敗する Then アカウントがロックされる\n",
        }),
    );

    let report0 = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let items0 = report0["items"].as_array().expect("items");
    assert!(
        items0.iter().any(|i| i["id"] == "REQ-001#AC1"),
        "minimal profile must materialize the implicit acceptance item: {report0}"
    );
    let ac_item0 = items0
        .iter()
        .find(|i| i["id"] == "REQ-001#AC1")
        .expect("REQ-001#AC1");
    assert_eq!(
        ac_item0["state"], "not_run",
        "no run recorded yet: {ac_item0}"
    );

    let update_out = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": dir.to_string_lossy(),
            "ops": [{"op": "record", "item": "REQ-001#AC1", "result": "pass"}],
        }),
    );
    assert!(update_out.get("failed").is_none(), "{update_out}");

    let report1 = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let ac_item1 = report1["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-001#AC1")
        .expect("REQ-001#AC1 after record");
    assert_eq!(
        ac_item1["state"], "passing",
        "recording pass on the implicit AC item must close the V: {ac_item1}"
    );
    assert_eq!(
        report1["coverage"]["acceptance"]["state"]["passing"], 1,
        "the implicit AC item's own pass must register as passing: {report1}"
    );
}

/// §10 シナリオ2〜3: `standard` で要件 → 仕様 → システムテスト → タスクを
/// 作り、要件の本文変更で1ホップだけ suspect（仕様・タスク）、システムテスト
/// は変わらない → 仕様の本文変更でシステムテスト（孫）も suspect になる →
/// `handoff_trace_suspect` の `clear` で一括解除し監査ファイルが残る。
#[test]
fn standard_profile_suspect_spreads_one_hop_then_deeper_and_clear_leaves_an_audit_file() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-e2e-standard" }),
    );
    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-m2-e2e-standard",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-200 Session timeout\n\n\
                - priority: P1\n\n\
                Original requirement statement text.\n",
        }),
    );
    let req_doc_id = req["doc_id"].as_str().expect("doc_id").to_string();

    let spec = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "spec-m2-e2e-standard",
            "title": "Basic spec",
            "layer": "basic_spec",
            "body": "# Basic spec\n\n### SPEC-200 Timeout enforcement\n\n\
                - refines: REQ-200\n\n\
                Original spec statement text.\n",
        }),
    );
    let spec_doc_id = spec["doc_id"].as_str().expect("doc_id").to_string();

    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "st-m2-e2e-standard",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System tests\n\n### ST-200 Session expiry check\n\n\
                - verifies: SPEC-200\n\
                - method: auto\n\n\
                Assert the idle session is torn down.\n",
        }),
    );

    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t-m2-e2e-standard", "title": "Implement timeout", "requirement_ids": ["REQ-200"] },
        }),
    );

    // Baseline: no suspects right after creation (every link was baselined
    // as it was created, §2.5).
    let list0 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert!(
        list0["suspects"].as_array().unwrap().is_empty(),
        "no suspects right after creation: {list0}"
    );

    // Edit the root REQ-200: SPEC-200 (refines) and the task (implements)
    // must become suspect (1 hop) — ST-200 (2 hops away) must stay clean.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-200 Session timeout\n\n\
                - priority: P1\n\n\
                Updated requirement statement text.\n",
        }),
    );
    let list1 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        suspect_items(&list1, "link"),
        vec!["SPEC-200".to_string()],
        "only the 1-hop child must be suspect right after editing the root: {list1}"
    );
    let task_suspects = suspects_of_kind(&list1, "task");
    assert_eq!(task_suspects.len(), 1, "{list1}");
    assert_eq!(task_suspects[0]["item"], "REQ-200", "{list1}");

    // Edit the middle item SPEC-200: ST-200 (the grandchild) now also
    // becomes suspect.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": spec_doc_id,
            "body": "# Basic spec\n\n### SPEC-200 Timeout enforcement\n\n\
                - refines: REQ-200\n\n\
                Updated spec statement text.\n",
        }),
    );
    let list2 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert_eq!(
        suspect_items(&list2, "link"),
        vec!["SPEC-200".to_string(), "ST-200".to_string()],
        "editing the middle item must additionally surface the grandchild: {list2}"
    );

    // Clear everything in one bulk call and verify the audit file.
    let clear = server.call(
        "handoff_trace_suspect",
        json!({
            "project_dir": dir.to_string_lossy(),
            "action": "clear",
            "targets": [{"upstream": "REQ-200"}, {"upstream": "SPEC-200"}, {"task_id": "t-m2-e2e-standard"}],
            "reason": "reviewed all diffs end to end (M2-20 E2E)",
            "executor_kind": "human",
            "executor_id": "m2-e2e",
        }),
    );
    assert!(clear["cleared"]["links"].as_u64().unwrap() >= 2, "{clear}");
    assert_eq!(clear["cleared"]["tasks"], 1, "{clear}");
    let clear_id = clear["clear_id"].as_str().expect("clear_id");
    let clear_path = dir
        .join(".handoff/trace/clears")
        .join(format!("{clear_id}.json"));
    assert!(
        clear_path.exists(),
        "clear must write an audit file at {clear_path:?}"
    );

    let list3 = server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": dir.to_string_lossy(), "action": "list" }),
    );
    assert!(
        list3["suspects"].as_array().unwrap().is_empty(),
        "bulk clear must resolve every suspect: {list3}"
    );
}

/// §10 シナリオ3〜4: `handoff_trace_update` 1回で項目追加・リンク・
/// dev_stage 変更・結果記録を行い、続けて `handoff_trace_ingest` に JUnit
/// XML を渡す。
#[test]
fn trace_update_combines_four_operations_and_trace_ingest_accepts_junit_xml() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-e2e-update" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-m2-e2e-update",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\nPlaceholder.\n",
        }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "st-m2-e2e-update",
            "title": "System tests",
            "layer": "system_test",
            "body": "# System tests\n\nPlaceholder.\n",
        }),
    );
    server.call(
        "handoff_update_task",
        json!({ "project_dir": dir.to_string_lossy(), "task": { "id": "t-m2-e2e-update", "title": "Implement" } }),
    );

    // One call: add REQ-300, add ST-300 (verifies REQ-300), link the task,
    // set dev_stage, and record a pass — all in one `trace_update`.
    let out = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": dir.to_string_lossy(),
            "ops": [
                {"op": "upsert_item", "doc": "req-m2-e2e-update", "id": "REQ-300",
                 "title": "Export throttling", "statement": "Exports are rate-limited.",
                 "attrs": {"priority": "P1"}},
                {"op": "upsert_item", "doc": "st-m2-e2e-update", "id": "ST-300",
                 "title": "Throttle check", "statement": "Assert the rate limiter rejects burst traffic.",
                 "attrs": {"verifies": ["REQ-300"], "method": "auto", "test": "mod1::throttle_check"}},
                {"op": "link", "item": "REQ-300", "task": "t-m2-e2e-update"},
                {"op": "set", "item": "REQ-300", "dev_stage": "implemented"},
                {"op": "record", "item": "ST-300", "result": "pass"},
            ],
        }),
    );
    assert!(out.get("failed").is_none(), "{out}");
    let applied = out["applied"].as_array().expect("applied");
    assert_eq!(applied.len(), 5, "{out}");

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy(), "include_items": true }),
    );
    let items = report["items"].as_array().unwrap();
    let req300 = items
        .iter()
        .find(|i| i["id"] == "REQ-300")
        .expect("REQ-300");
    assert_eq!(req300["dev_stage"], "implemented", "{req300}");
    let st300 = items.iter().find(|i| i["id"] == "ST-300").expect("ST-300");
    assert_eq!(st300["state"], "passing", "{st300}");

    let task = server.call(
        "handoff_get_task",
        json!({ "project_dir": dir.to_string_lossy(), "task_id": "t-m2-e2e-update" }),
    );
    assert!(
        task["trace"]["layers"]
            .as_array()
            .is_some_and(|l| l.iter().any(|layer| layer["layer"] == "requirement")),
        "task must show the new requirement link: {task}"
    );

    // JUnit XML ingestion for ST-300's declared `- test:` attribute
    // (independent of the record op above — re-records the same item via
    // the ingest path).
    let junit = r#"<testsuite><testcase classname="mod1" name="throttle_check"><failure message="boom"/></testcase></testsuite>"#;
    let ingest = server.call(
        "handoff_trace_ingest",
        json!({
            "project_dir": dir.to_string_lossy(),
            "format": "junit_xml",
            "output": junit,
        }),
    );
    assert_eq!(ingest["recorded"], 1, "{ingest}");
    assert_eq!(ingest["matched"][0]["item"], "ST-300", "{ingest}");
    assert_eq!(ingest["matched"][0]["result"], "fail", "{ingest}");
}

/// §10 シナリオ5: `handoff_trace_lint` の終了コードが 0（クリーン）/ 1
/// （findings あり）/ 2（設定・入力エラー）になる — CLI 経由で確認する。
#[test]
fn trace_lint_cli_exit_codes_are_0_1_and_2() {
    // exit 0: clean project.
    {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Server::spawn();
        server.call(
            "handoff_init",
            json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-e2e-lint-clean" }),
        );
        server.call(
            "handoff_doc_save",
            json!({
                "project_dir": dir.to_string_lossy(),
                "slug": "req-m2-e2e-lint-clean",
                "title": "Requirements",
                "layer": "requirement",
                "body": "# Requirements\n\n### REQ-400 Something\n\n- priority: P2\n\nBody text.\n",
            }),
        );
        drop(server);

        let (stdout, stderr, code) =
            run_cli(&["trace", "lint", "--project-dir", dir.to_str().unwrap()]);
        assert_eq!(code, 0, "stdout={stdout} stderr={stderr}");
        let parsed: Value = serde_json::from_str(&stdout).expect("valid JSON stdout");
        assert_eq!(parsed["exit_code"], 0, "{parsed}");
    }

    // exit 1: a dangling reference (findings present, no `require` rule
    // failure).
    {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Server::spawn();
        server.call(
            "handoff_init",
            json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-e2e-lint-dangling" }),
        );
        server.call(
            "handoff_doc_save",
            json!({
                "project_dir": dir.to_string_lossy(),
                "slug": "spec-m2-e2e-lint-dangling",
                "title": "Basic spec",
                "layer": "basic_spec",
                "body": "# Basic spec\n\n### SPEC-401 Orphaned\n\n- refines: REQ-NOPE\n\nBody text.\n",
            }),
        );
        drop(server);

        let (stdout, stderr, code) =
            run_cli(&["trace", "lint", "--project-dir", dir.to_str().unwrap()]);
        assert_eq!(code, 1, "stdout={stdout} stderr={stderr}");
        let parsed: Value = serde_json::from_str(&stdout).expect("valid JSON stdout");
        assert_eq!(parsed["exit_code"], 1, "{parsed}");
    }

    // exit 2: an invalid `--fail-on` value (configuration/input error).
    {
        let tmp = tempfile::tempdir().expect("temp dir");
        let dir = tmp.path().join("proj");
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Server::spawn();
        server.call(
            "handoff_init",
            json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-e2e-lint-badconfig" }),
        );
        drop(server);

        let (_stdout, _stderr, code) = run_cli(&[
            "trace",
            "lint",
            "--project-dir",
            dir.to_str().unwrap(),
            "--fail-on",
            "bogus",
        ]);
        assert_eq!(
            code, 2,
            "exit code must be 2 for an invalid --fail-on value"
        );
    }
}

/// §10 シナリオ6: 読み取り専用ツール（`trace_report`/`trace_slice`/
/// `trace_lint`/`trace_matrix`/`trace_next`/`trace_impact`/
/// `trace_suspect list`/`get_task`/`list_tasks`）を呼ぶ前後で `.handoff/`
/// のバイト列が一切変わらない。直接編集・未同期の層文書（`layer_sync_stamp`
/// なし）を含んだ状態で検査する（E6 の「メモリ上でだけ再同期する」契約）。
#[test]
fn readonly_tools_leave_handoff_bytes_byte_for_byte_unchanged() {
    use handoff_mcp::storage::docs::model::DocMetadata;
    use handoff_mcp::storage::docs::{write_doc, write_doc_body};

    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let handoff = dir.join(".handoff");
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-e2e-readonly" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-m2-e2e-readonly",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-500 Something\n\n- priority: P2\n\nBody text.\n",
        }),
    );
    server.call(
        "handoff_update_task",
        json!({
            "project_dir": dir.to_string_lossy(),
            "task": { "id": "t-m2-e2e-readonly", "title": "Implement", "requirement_ids": ["REQ-500"] },
        }),
    );

    // A second layer document written directly via the storage layer (never
    // through `handoff_doc_save`) — never synced, no `layer_sync_stamp` —
    // exactly the "direct edit" case E6 says must only resync in memory for
    // read-only tools.
    let mut doc = DocMetadata::new(
        "doc-20260901-000000-9999".to_string(),
        "spec-m2-e2e-readonly".to_string(),
        "Basic spec".to_string(),
        "spec".to_string(),
        "2026-09-01T00:00:00.000000000+00:00".to_string(),
    );
    doc.layer = Some("basic_spec".to_string());
    write_doc_body(
        &handoff,
        "spec-m2-e2e-readonly",
        "# Basic spec\n\n### SPEC-500 Direct edit\n\n- refines: REQ-500\n\nBody text.\n",
    )
    .expect("write_doc_body");
    write_doc(&handoff, &doc).expect("write_doc");

    drop(server);

    // `trace_report`/`trace_slice` are *not* part of E6's read-only set
    // (wiki/260 §6: "M1 と同じ（再同期と自己修復を書き込む）") — calling
    // `trace_report` first resyncs the never-synced SPEC-500 document to
    // disk (§7's "最初の doc_save / trace_report / trace_slice で... 再同期
    // され"), establishing the stable on-disk state the actually-read-only
    // tools below must then leave untouched.
    let mut server = Server::spawn();
    let pd = dir.to_string_lossy().to_string();
    server.call("handoff_trace_report", json!({ "project_dir": pd }));
    server.call(
        "handoff_trace_slice",
        json!({ "project_dir": pd, "item": "REQ-500" }),
    );
    drop(server);

    let before = snapshot(&handoff);

    // E6's actual read-only set (`trace_lint`/`trace_matrix`/`trace_next`/
    // `trace_impact`/`trace_suspect list`), plus `get_task`/`list_tasks`
    // (same in-memory-only contract, §3.4/§4.11) — none of these may write
    // a single byte under `.handoff/`.
    let mut server = Server::spawn();
    server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    server.call(
        "handoff_trace_matrix",
        json!({ "project_dir": pd, "format": "csv" }),
    );
    server.call("handoff_trace_next", json!({ "project_dir": pd }));
    server.call(
        "handoff_trace_impact",
        json!({ "project_dir": pd, "item": "REQ-500" }),
    );
    server.call(
        "handoff_trace_suspect",
        json!({ "project_dir": pd, "action": "list" }),
    );
    server.call(
        "handoff_get_task",
        json!({ "project_dir": pd, "task_id": "t-m2-e2e-readonly" }),
    );
    server.call("handoff_list_tasks", json!({ "project_dir": pd }));
    drop(server);

    let after = snapshot(&handoff);
    if before != after {
        let before_map: std::collections::BTreeMap<_, _> = before.iter().cloned().collect();
        let after_map: std::collections::BTreeMap<_, _> = after.iter().cloned().collect();
        for (path, before_bytes) in &before_map {
            match after_map.get(path) {
                None => panic!("file disappeared after read-only calls: {path:?}"),
                Some(after_bytes) if after_bytes != before_bytes => panic!(
                    "file changed after read-only calls: {path:?}\nbefore: {}\nafter: {}",
                    String::from_utf8_lossy(before_bytes),
                    String::from_utf8_lossy(after_bytes)
                ),
                _ => {}
            }
        }
        for path in after_map.keys() {
            if !before_map.contains_key(path) {
                panic!("new file appeared after read-only calls: {path:?}");
            }
        }
    }
}

/// §10 シナリオ7: `handoff_trace_next` が優先度の高い種別を先頭に返す —
/// ここでは `review_suspect`（既に suspect が存在する）が `write_verification`
/// （まだ記録のない検証項目）より先に来ることだけを確認する（個々の種別・
/// フィルタの網羅は `tests/trace_next_e2e.rs` の既存10ケースが担う）。
#[test]
fn trace_next_ranks_review_suspect_above_write_verification() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-e2e-next" }),
    );
    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "req-m2-e2e-next",
            "title": "Requirements",
            "layer": "requirement",
            "body": "# Requirements\n\n### REQ-600 Something\n\n- priority: P0\n\nOriginal text.\n",
        }),
    );
    let req_doc_id = req["doc_id"].as_str().expect("doc_id").to_string();
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "at-m2-e2e-next",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-600 Check\n\n- verifies: REQ-600\n- method: manual\n\nCheck it.\n",
        }),
    );
    // Introduce a link suspect on AT-600 by editing REQ-600 after creation.
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-600 Something\n\n- priority: P0\n\nUpdated text.\n",
        }),
    );

    let next = server.call(
        "handoff_trace_next",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    let actions = next["actions"].as_array().expect("actions");
    let review_suspect_rank = actions
        .iter()
        .find(|a| a["kind"] == "review_suspect")
        .map(|a| a["rank"].as_u64().unwrap())
        .expect("review_suspect action must be present");
    let write_verification_rank = actions
        .iter()
        .find(|a| a["kind"] == "write_verification")
        .map(|a| a["rank"].as_u64().unwrap());
    if let Some(wv_rank) = write_verification_rank {
        assert!(
            review_suspect_rank < wv_rank,
            "review_suspect must outrank write_verification: {next}"
        );
    }
}

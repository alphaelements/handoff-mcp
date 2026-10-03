//! M3 全体の実バイナリ E2E（t360.40.14, wiki/270-vmodel-m3-design.md §10）:
//! 個々のツールの契約は S1-S5 で追加済みの `tests/trace_*_e2e.rs` 群がそれ
//! ぞれ担うため、このファイルはそれらを縫い合わせた「M3 全体を通した1本の
//! シナリオ」に限定する — 重複した網羅は避ける。
//!
//! 1. minimal プロファイルの要件文書を作成し、`needs: acceptance` を設定
//! 2. 要件項目に `assignee: ryoma` を設定
//! 3. approval を draft -> review -> approved に遷移させ、監査ファイルを確認
//! 4. 本文を変更して自動差し戻し（approved -> draft）を確認
//! 5. `trace_baseline create` でベースラインを作成
//! 6. 項目を追加して `trace_baseline create` -> `trace_baseline diff` で
//!    added と state_changes を確認
//! 7. `trace_update(propose=true)` で delta を作成 -> `trace_delta list` で
//!    pending -> `trace_delta apply`
//! 8. `trace_test_run create` -> `trace_record(test_run_id=...)` ->
//!    `trace_test_run progress`
//! 9. `trace_lint` で品質ルール（ambiguous_word, missing_acceptance）を確認
//! 10. `trace_lint(action="quality_prompt")` でプロンプトテンプレートを取得
//! 11. `trace_next(assignee="ryoma")` でフィルタ
//! 12. done ガードで `approval_blocker` が報告されること

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
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

/// §10 の全シナリオを一本に繋いだ M3 全体の E2E。
#[test]
fn m3_full_scenario_approval_baseline_delta_test_run_lint_next_done_guard() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();
    let pd = dir.to_string_lossy().to_string();
    let mut server = Server::spawn();

    server.call(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "m3-e2e-full" }),
    );
    server.call(
        "handoff_update_config",
        json!({ "project_dir": pd, "updates": { "settings.require_estimate_hours": false } }),
    );

    // 1. minimal プロファイルの要件文書 + needs: acceptance
    let req = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-m3-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "trace_profile": "minimal",
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\
                - needs: acceptance\n\n\
                After 5 consecutive failed logins, the account locks for 15 minutes.\n\n\
                受入基準:\n\
                - AC1: Given 4回失敗済み When 5回目に失敗する Then アカウントがロックされる\n",
        }),
    );
    let req_doc_id = req["doc_id"].as_str().expect("doc_id").to_string();

    // 2. assignee: ryoma を要件項目に設定（ロスター登録の上で属性ブロックに
    //    `- assignee: ryoma` を追記 — `trace_update` の `upsert_item.attrs`
    //    には `assignee`/`needs` キーが存在しない（reserved attrs は
    //    Markdown の属性行経由でのみ設定できる、§2.2）ので `doc_save` で
    //    本文を直接書き換える。
    server.call(
        "handoff_add_assignee",
        json!({ "project_dir": pd, "key": "ryoma" }),
    );
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\
                - needs: acceptance\n\
                - assignee: ryoma\n\n\
                After 5 consecutive failed logins, the account locks for 15 minutes.\n\n\
                受入基準:\n\
                - AC1: Given 4回失敗済み When 5回目に失敗する Then アカウントがロックされる\n",
        }),
    );

    // `assignee` is not itself surfaced on `trace_report` items (it only
    // feeds `trace_next`'s `manual_pending` kind/filter, §4.5) — step 11
    // below verifies it end to end via `trace_next(assignee="ryoma")`.
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let req100 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-100")
        .expect("REQ-100");
    assert_eq!(
        req100["approval"], "draft",
        "fresh item defaults to draft (E12): {req100}"
    );

    // 3. approval: draft -> review -> approved,監査ファイルを確認
    server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "ops": [{"op": "set", "item": "REQ-100", "approval": "review"}],
        }),
    );
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let req100 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-100")
        .unwrap();
    assert_eq!(req100["approval"], "review", "{req100}");

    server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "executor_kind": "human",
            "executor_id": "ryoma",
            "ops": [{"op": "set", "item": "REQ-100", "approval": "approved"}],
        }),
    );
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let req100 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-100")
        .unwrap();
    assert_eq!(req100["approval"], "approved", "{req100}");

    let approvals_dir = dir.join(".handoff/trace/approvals");
    let approval_files_after_approve = std::fs::read_dir(&approvals_dir)
        .expect("approvals dir exists")
        .count();
    assert_eq!(
        approval_files_after_approve, 1,
        "exactly one approval audit file for the -> approved transition"
    );

    // 4. 本文を変更して自動差し戻し（approved -> draft）を確認
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "doc_id": req_doc_id,
            "body": "# Requirements\n\n### REQ-100 Account lockout\n\n\
                - priority: P1\n\
                - needs: acceptance\n\
                - assignee: ryoma\n\n\
                Revised requirement statement text.\n\n\
                受入基準:\n\
                - AC1: Given 4回失敗済み When 5回目に失敗する Then アカウントがロックされる\n",
        }),
    );
    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let req100 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-100")
        .unwrap();
    assert_eq!(
        req100["approval"], "draft",
        "a def_hash change must auto-reset approval to draft: {req100}"
    );
    // The automatic rollback must not itself write a new audit file.
    let approval_files_after_rollback = std::fs::read_dir(&approvals_dir).unwrap().count();
    assert_eq!(
        approval_files_after_rollback, 1,
        "automatic draft rollback must not write a new approval audit file"
    );

    // 5. trace_baseline create でベースラインを作成
    let baseline1 = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create", "label": "before AT" }),
    );
    let baseline1_id = baseline1["baseline_id"].as_str().unwrap().to_string();
    assert!(baseline1["coverage_summary"].is_object(), "{baseline1}");

    // 6. 項目を追加して trace_baseline create -> diff で added/state_changes
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "at-m3-e2e",
            "title": "Acceptance",
            "layer": "acceptance",
            "body": "# Acceptance\n\n### AT-100 Lockout check\n\n\
                - verifies: REQ-100\n- method: manual\n\n\
                Manually confirm the lockout.\n",
        }),
    );
    let baseline2 = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "create", "label": "after AT" }),
    );
    let baseline2_id = baseline2["baseline_id"].as_str().unwrap().to_string();
    assert_ne!(baseline1_id, baseline2_id);

    let diff = server.call(
        "handoff_trace_baseline",
        json!({ "project_dir": pd, "action": "diff", "from": baseline1_id, "to": baseline2_id }),
    );
    let added: Vec<String> = diff["added"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(added, vec!["AT-100".to_string()], "{diff}");
    assert!(
        diff["state_changes"].is_object(),
        "diff must report coverage/state shifts: {diff}"
    );

    // 7. trace_update(propose=true) で delta を作成 -> trace_delta list で
    //    pending -> trace_delta apply
    let proposed = server.call(
        "handoff_trace_update",
        json!({
            "project_dir": pd,
            "propose": true,
            "description": "bump REQ-100 dev_stage",
            "ops": [{"op": "set", "item": "REQ-100", "dev_stage": "in_progress"}],
        }),
    );
    let delta_id = proposed["delta_id"].as_str().expect("delta_id").to_string();

    let deltas = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "list" }),
    );
    let pending: Vec<String> = deltas["deltas"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|d| d["status"] == "pending")
        .map(|d| d["delta_id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(pending, vec![delta_id.clone()], "{deltas}");

    let applied = server.call(
        "handoff_trace_delta",
        json!({ "project_dir": pd, "action": "apply", "delta_id": delta_id }),
    );
    assert_eq!(applied["applied"].as_array().unwrap().len(), 1, "{applied}");

    let report = server.call(
        "handoff_trace_report",
        json!({ "project_dir": pd, "include_items": true }),
    );
    let req100 = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-100")
        .unwrap();
    assert_eq!(
        req100["dev_stage"], "in_progress",
        "the applied delta must have actually written the op: {req100}"
    );

    // 8. trace_test_run create -> trace_record(test_run_id=...) ->
    //    trace_test_run progress
    let test_run = server.call(
        "handoff_trace_test_run",
        json!({
            "project_dir": pd,
            "action": "create",
            "scope": { "layers": ["acceptance"] },
            "label": "M3 E2E run",
        }),
    );
    let test_run_id = test_run["test_run_id"].as_str().unwrap().to_string();
    // The `acceptance` layer scope also picks up `REQ-100#AC1` — the
    // `minimal` profile's implicit acceptance item (step 1) materializes as
    // an `acceptance`-layer item in its own right, alongside AT-100.
    let mut target_items: Vec<String> = test_run["target_items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    target_items.sort();
    assert_eq!(
        target_items,
        vec!["AT-100".to_string(), "REQ-100#AC1".to_string()],
        "{test_run}"
    );

    server.call(
        "handoff_trace_record",
        json!({
            "project_dir": pd,
            "results": [{ "item": "AT-100", "result": "pass" }],
            "test_run_id": test_run_id,
        }),
    );
    let progress = server.call(
        "handoff_trace_test_run",
        json!({ "project_dir": pd, "action": "progress", "test_run_id": test_run_id }),
    );
    assert_eq!(progress["total"], 2, "{progress}");
    assert_eq!(progress["executed"], 1, "{progress}");
    assert_eq!(progress["pass"], 1, "{progress}");
    assert_eq!(progress["progress_pct"], 50.0, "{progress}");

    // 9. trace_lint で品質ルール（ambiguous_word, missing_acceptance）を確認
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": pd,
            "slug": "req-m3-e2e-quality",
            "title": "Quality probe requirements",
            "layer": "requirement",
            "body": "# Quality probe requirements\n\n### REQ-101 ログは適切に記録される\n\nBody.\n",
        }),
    );
    let lint = server.call("handoff_trace_lint", json!({ "project_dir": pd }));
    let findings = lint["findings"].as_array().unwrap();
    assert!(
        findings
            .iter()
            .any(|f| f["rule"] == "ambiguous_word" && f["item"] == "REQ-101"),
        "{lint}"
    );
    assert!(
        findings
            .iter()
            .any(|f| f["rule"] == "missing_acceptance" && f["item"] == "REQ-101"),
        "{lint}"
    );

    // 10. trace_lint(action="quality_prompt") でプロンプトテンプレートを取得
    let prompt = server.call(
        "handoff_trace_lint",
        json!({ "project_dir": pd, "action": "quality_prompt", "items": ["REQ-101"] }),
    );
    let prompts = prompt["prompts"].as_array().unwrap();
    assert_eq!(prompts.len(), 1, "{prompt}");
    assert_eq!(prompts[0]["item_id"], "REQ-101", "{prompt}");
    assert!(
        !prompts[0]["aspects"].as_array().unwrap().is_empty(),
        "{prompt}"
    );

    // 11. trace_next(assignee="ryoma") でフィルタ
    let next = server.call(
        "handoff_trace_next",
        json!({ "project_dir": pd, "assignee": "ryoma" }),
    );
    let actions = next["actions"].as_array().unwrap();
    assert!(
        actions.iter().any(|a| a["item"] == "REQ-100"),
        "assignee=ryoma must surface at least one action tied to REQ-100 \
         (the only item with `assignee: ryoma` in this project): {next}"
    );
    assert!(
        actions.iter().all(|a| a["item"] != "REQ-101"),
        "assignee=ryoma must exclude REQ-101, which has no assignee: {next}"
    );

    // 12. done ガードで approval_blocker が報告されること（REQ-100 は draft
    //     に自動差し戻し済み — §3.3「承認連動」の対象）
    let tasks = server.call(
        "handoff_update_task",
        json!({
            "project_dir": pd,
            "task": { "id": "t-m3-e2e-impl", "title": "Implement REQ-100", "requirement_ids": ["REQ-100"] },
        }),
    );
    let task_id = tasks["task"]["id"]
        .as_str()
        .unwrap_or("t-m3-e2e-impl")
        .to_string();

    let (is_error, text) = server.call_raw(
        "handoff_update_task",
        json!({ "project_dir": pd, "task": { "id": task_id, "status": "review" } }),
    );
    assert!(!is_error, "{text}");
    assert!(
        text.contains("done_guard") && text.contains("approval_draft"),
        "expected a done_guard warning naming the approval_draft blocker, got: {text}"
    );
}

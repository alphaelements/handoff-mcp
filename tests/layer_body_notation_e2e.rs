//! Real-binary E2E test for M2-02's body-notation extension
//! (wiki/260-vmodel-m2-design.md §2.2-§2.4): spawns the actual `handoff-mcp`
//! binary and drives it over real stdio JSON-RPC (same harness style as
//! `tests/layer_sync_e2e.rs`), then inspects the persisted `_doc.<slug>.md`
//! frontmatter directly — the new `SubItem` fields (`def_hash`/`acceptance`/
//! `rationale`/`derived`/`waivers`/`from`/`reserved_attrs`) are not yet
//! surfaced by any MCP tool's own JSON projection (`handoff_doc_verify_status`
//! hand-picks its response fields and is M2-18/other-milestone scope to
//! extend — out of M2-02's `src/storage/docs/{layer_parse,layer_sync,model}.rs`
//! scope), so the on-disk artifact is the only real-transport-reachable
//! surface this task can assert against today. This still proves the whole
//! `handoff_doc_save` -> `sync_layer_items_with_options` -> `write_frontmatter_doc`
//! path is wired end-to-end, not just `sync_layer_items_with_options`'s own
//! unit tests.
//!
//! Rework round 2 (BLOCKER fix): also covers the production wiring of
//! `implicit_acceptance` resolution (`resolve_doc_implicit_acceptance` in
//! `src/storage/docs/layer_sync.rs`, called from `src/mcp/handlers/docs.rs`'s
//! `sync_layer_items_if_needed` and `doc_verify(action="sync")`) — a real
//! `handoff_doc_save` on a `minimal`/`bugfix`-profile document must
//! materialize the implicit acceptance-verification SubItems, not just the
//! unit-level `sync_layer_items_with_options(.., implicit_acceptance: true)`
//! calls in `layer_sync.rs`'s own test module — and the MINOR fix that no
//! existing MCP tool argument can write the new body-derived `SubItem`
//! fields (`def_hash`/`acceptance`/`rationale`/`derived`/`waivers`/`from`/
//! `implicit_of`/`reserved_attrs`/`link_baselines`).

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use handoff_mcp::storage::config::{read_config, write_config};
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

/// Extracts the `---`-fenced YAML frontmatter block from a `_doc.<slug>.md`
/// file and parses it into a generic [`serde_yaml::Value`] for structured
/// field assertions.
fn read_frontmatter_yaml(path: &std::path::Path) -> serde_yaml::Value {
    let content = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    let mut lines = content.lines();
    assert_eq!(
        lines.next(),
        Some("---"),
        "document must start with a YAML frontmatter fence: {content}"
    );
    let yaml: String = lines
        .by_ref()
        .take_while(|l| *l != "---")
        .collect::<Vec<_>>()
        .join("\n");
    serde_yaml::from_str(&yaml).unwrap_or_else(|e| panic!("parse frontmatter YAML: {e}\n{yaml}"))
}

fn find_sub_item_yaml<'a>(fm: &'a serde_yaml::Value, stable_id: &str) -> &'a serde_yaml::Mapping {
    fm["verification"]["items"]
        .as_sequence()
        .expect("verification.items sequence")
        .iter()
        // `sub_items` is `skip_serializing_if = "Vec::is_empty"` (§NFR-004),
        // so a section with no sub-items has no `sub_items` key at all
        // rather than an empty sequence — treat "missing" the same as
        // "empty" here.
        .flat_map(|item| {
            item["sub_items"]
                .as_sequence()
                .map_or(&[][..], |s| s.as_slice())
        })
        .find(|sub| sub["stable_id"].as_str() == Some(stable_id))
        .unwrap_or_else(|| panic!("stable_id {stable_id} not found in frontmatter: {fm:?}"))
        .as_mapping()
        .expect("sub_item is a mapping")
}

/// wiki/260 §2.2/§2.3/§2.4 (M2-02): a `doc_save` on a `requirement` layer
/// document whose body has the acceptance-criteria block plus every new
/// attribute key must, through the real `handoff_doc_save` transport, persist
/// a `SubItem` with `def_hash`/`acceptance`/`rationale`/`derived`/`waivers`/
/// `from`/`reserved_attrs` all populated from the body.
#[test]
fn doc_save_layer_body_with_m2_notation_persists_new_sub_item_fields_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    let init = server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-body-notation-e2e" }),
    );
    assert!(
        init.get("error").is_none() || init["error"].is_null(),
        "init failed: {init}"
    );

    let slug = unique_slug("req-m2-e2e");
    let body = "# Requirements\n\n\
### REQ-003 ログイン失敗時のアカウントロック\n\n\
- rationale: 総当たり攻撃の抑止\n\
- assignee: alice\n\n\
5回連続で認証に失敗したアカウントを15分間ロックする。\n\n\
受入基準:\n\
- AC1: Given 同一アカウントで4回失敗済み When 5回目に失敗する Then アカウントがロックされる\n\
- AC2: WHEN アカウントがロック中 THE SYSTEM SHALL 正しいパスワードでもログインを拒否する\n\n\
### SPEC-020 監査ログの保存形式\n\n\
- refines: REQ-003\n\
- derived: 実装方式から必要になった項目\n\n\
監査ログはJSON Linesで保存する。\n";

    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Requirements (M2 E2E)",
            "body": body,
            "layer": "requirement",
        }),
    );
    assert!(saved.get("error").is_none(), "doc_save failed: {saved}");

    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let fm = read_frontmatter_yaml(&md_path);

    let req_003 = find_sub_item_yaml(&fm, "REQ-003");
    assert!(
        req_003
            .get("def_hash")
            .and_then(|v| v.as_str())
            .is_some_and(|s| !s.is_empty()),
        "REQ-003 must persist a non-empty def_hash: {req_003:?}"
    );
    assert_eq!(
        req_003.get("rationale").and_then(|v| v.as_str()),
        Some("総当たり攻撃の抑止")
    );
    assert_eq!(
        req_003
            .get("reserved_attrs")
            .and_then(|v| v.get("assignee"))
            .and_then(|v| v.as_str()),
        Some("alice"),
        "reserved_attrs must round-trip through the real doc_save write path: {req_003:?}"
    );
    let acceptance = req_003
        .get("acceptance")
        .and_then(|v| v.as_sequence())
        .expect("acceptance sequence");
    assert_eq!(acceptance.len(), 2, "acceptance: {acceptance:?}");
    assert_eq!(acceptance[0]["label"].as_str(), Some("AC1"));
    assert_eq!(acceptance[0]["kind"].as_str(), Some("gwt"));
    assert_eq!(acceptance[1]["label"].as_str(), Some("AC2"));
    assert_eq!(acceptance[1]["kind"].as_str(), Some("ears"));

    let spec_020 = find_sub_item_yaml(&fm, "SPEC-020");
    assert_eq!(
        spec_020.get("derived").and_then(|v| v.as_str()),
        Some("実装方式から必要になった項目")
    );
}

/// E14 (wiki/260 §2.2): a document whose body already had a literal
/// `- rationale:` line before M2 (so it stayed in M1's `statement` as
/// ordinary text) must produce the exact same `body_hash` once parsed and
/// persisted through the M2-aware `doc_save`/`sync_layer_items`, matching a
/// hand-computed M1-era hash — no pre-existing `trace_record` run becomes
/// spuriously suspect on upgrade (the unit-level
/// `body_hash_unchanged_for_m1_fixture_with_now_recognized_attribute_lines`
/// test in `layer_parse.rs` checks the same invariant directly against
/// `compute_body_hash`; this E2E confirms the identical value reaches the
/// real on-disk artifact through the real binary).
#[test]
fn body_hash_over_real_stdio_is_deterministic_for_now_recognized_attribute_lines() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-body-hash-e2e" }),
    );

    let slug = unique_slug("req-body-hash-e2e");
    let body = "# Requirements\n\n### REQ-040 タイトル\n\n\
- priority: P1\n\
- rationale: 総当たり攻撃の抑止\n\
- assignee: alice\n\n\
本文テキスト。\n";

    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Body hash E2E",
            "body": body,
            "layer": "requirement",
        }),
    );
    let doc_id = saved["doc_id"]
        .as_str()
        .unwrap_or_else(|| panic!("doc_save failed: {saved}"))
        .to_string();

    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let fm1 = read_frontmatter_yaml(&md_path);
    let body_hash_1 = find_sub_item_yaml(&fm1, "REQ-040")
        .get("body_hash")
        .and_then(|v| v.as_str())
        .expect("body_hash present")
        .to_string();

    // A metadata-only re-save (no body/append_body) must reproduce the
    // identical `body_hash` — the same code path, exercised again through
    // the real binary.
    let resaved = server.call(
        "handoff_doc_save",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": doc_id, "tags": ["e2e-rerun"] }),
    );
    assert!(resaved.get("error").is_none(), "resave failed: {resaved}");

    let fm2 = read_frontmatter_yaml(&md_path);
    let body_hash_2 = find_sub_item_yaml(&fm2, "REQ-040")
        .get("body_hash")
        .and_then(|v| v.as_str())
        .expect("body_hash present after resave");
    assert_eq!(body_hash_2, body_hash_1);
}

/// Rework round 2 (BLOCKER fix, wiki/260 §2.1/§2.5 手順 3): a document under
/// the project default `minimal` profile (`implicit_acceptance: true`) must
/// have its acceptance-criteria bullets materialized as implicit
/// acceptance-verification SubItems through the real `handoff_doc_save` ->
/// `sync_layer_items_if_needed` -> `resolve_doc_implicit_acceptance` ->
/// `sync_layer_items_with_options` path — before this fix, that production
/// path always called the 5-arg `sync_layer_items` with a hardcoded
/// `implicit_acceptance: false`, so no implicit item was ever created by any
/// real MCP call regardless of the configured profile.
#[test]
fn doc_save_under_minimal_profile_materializes_implicit_acceptance_item_over_real_stdio() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-implicit-ac-e2e" }),
    );

    let config_path = dir.join(".handoff").join("config.toml");
    let mut config = read_config(&config_path).expect("read config");
    config.trace.profile = Some("minimal".to_string());
    write_config(&config_path, &config).expect("write config");

    let slug = unique_slug("req-implicit-ac-e2e");
    let body = "# Requirements\n\n\
### REQ-100 パスワードリセット\n\n\
リセットメールは10分で失効する。\n\n\
受入基準:\n\
- AC1: Given リセットメール送信から10分経過 When リンクを踏む Then 失効エラーになる\n";

    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Requirements (implicit AC E2E)",
            "body": body,
            "layer": "requirement",
        }),
    );
    assert!(saved.get("error").is_none(), "doc_save failed: {saved}");

    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let fm = read_frontmatter_yaml(&md_path);
    let implicit_item = find_sub_item_yaml(&fm, "REQ-100#AC1");
    assert_eq!(
        implicit_item.get("implicit_of").and_then(|v| v.as_str()),
        Some("REQ-100"),
        "implicit item must record its parent: {implicit_item:?}"
    );
    assert_eq!(
        implicit_item.get("origin").and_then(|v| v.as_str()),
        Some("body")
    );
}

/// Rework round 2 (MINOR fix): done_criteria item 0 requires confirming, via
/// a test, that no existing MCP tool argument can write the new
/// body-derived `SubItem` fields — `handoff_doc_verify(action="add_item")`
/// only reads `description`/`category`/`fragment_seq`/`label` from its
/// arguments (`SubItem { .., ..Default::default() }`), so extraneous
/// `def_hash`/`acceptance`/`rationale`/`derived`/`waivers`/`from`/
/// `implicit_of`/`reserved_attrs`/`link_baselines` arguments must be
/// silently ignored rather than persisted.
#[test]
fn add_item_over_real_stdio_ignores_m2_body_derived_field_arguments() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let dir = tmp.path().join("proj");
    std::fs::create_dir_all(&dir).unwrap();

    let mut server = Server::spawn();
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "m2-write-guard-e2e" }),
    );

    let slug = unique_slug("spec-write-guard-e2e");
    let saved = server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": slug,
            "title": "Write guard doc (no layer)",
            "body": "# Spec\n\n### Section A\n\nSome text.\n",
        }),
    );
    let doc_id = saved["doc_id"]
        .as_str()
        .unwrap_or_else(|| panic!("doc_save failed: {saved}"))
        .to_string();

    let generated = server.call(
        "handoff_doc_verify",
        json!({ "project_dir": dir.to_string_lossy(), "doc_id": doc_id, "action": "generate" }),
    );
    assert!(
        generated.get("error").is_none(),
        "generate failed: {generated}"
    );

    let md_path = dir.join(".handoff/docs").join(format!("_doc.{slug}.md"));
    let fm0 = read_frontmatter_yaml(&md_path);
    let fragment_seq = fm0["verification"]["items"][0]["fragment_seq"]
        .as_u64()
        .expect("first item has a fragment_seq");

    let added = server.call(
        "handoff_doc_verify",
        json!({
            "project_dir": dir.to_string_lossy(),
            "doc_id": doc_id,
            "action": "add_item",
            "fragment_seq": fragment_seq,
            "description": "hand-added via existing API",
            // Every M2 body-derived field, attempted as a direct argument:
            "def_hash": "forged-def-hash",
            "acceptance": [{ "label": "AC1", "kind": "gwt" }],
            "rationale": "should not be writable via this API",
            "derived": "should not be writable via this API",
            "waivers": [{ "axis": "verify", "reason": "should not be writable" }],
            "from": "REQ-999",
            "implicit_of": "REQ-999",
            "reserved_attrs": { "assignee": "mallory" },
            "link_baselines": { "REQ-999": "forged-hash" },
        }),
    );
    assert!(added.get("error").is_none(), "add_item failed: {added}");

    let fm1 = read_frontmatter_yaml(&md_path);
    let added_item = fm1["verification"]["items"]
        .as_sequence()
        .unwrap()
        .iter()
        .flat_map(|item| {
            item["sub_items"]
                .as_sequence()
                .map_or(&[][..], |s| s.as_slice())
        })
        .find(|sub| sub["description"].as_str() == Some("hand-added via existing API"))
        .expect("hand-added sub_item present")
        .as_mapping()
        .expect("sub_item is a mapping");

    for forged_key in [
        "def_hash",
        "acceptance",
        "rationale",
        "derived",
        "waivers",
        "from",
        "implicit_of",
        "reserved_attrs",
        "link_baselines",
    ] {
        assert!(
            !added_item.contains_key(forged_key),
            "add_item must not let a caller write '{forged_key}' directly: {added_item:?}"
        );
    }
}

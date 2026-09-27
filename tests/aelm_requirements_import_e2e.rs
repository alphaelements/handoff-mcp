//! t360.30 (aelm referral ref-20260919-095202-454483997, wiki/250-aelm-
//! requirements-import.md): manual E2E against aelm's real 29
//! `req-c*` requirement documents, exercised through the real binary's
//! stdio JSON-RPC entry point (`process_line`) — the same path the MCP
//! server runs in production.
//!
//! Not run by default (`#[ignore]`) — these are aelm's actual project
//! documents (~2,468 requirements), which this repo does not vendor or
//! commit. Run manually with both repos checked out side-by-side under the
//! same parent directory (the default layout: `~/pro/handoff-mcp` and
//! `~/pro/aelm`):
//!
//! ```sh
//! cargo test --test aelm_requirements_import_e2e -- --ignored --nocapture
//! ```
//!
//! Point at a different aelm checkout with `HANDOFF_AELM_DOCS_DIR` (must
//! contain the `_doc.req-c*.md` files directly, i.e. aelm's
//! `.handoff/docs/`). If neither the default relative path nor the env
//! override resolves to an existing directory, the test prints a message
//! and returns (soft skip) rather than failing — this repo's CI has no
//! reason to have aelm checked out next to it.
//!
//! This test only *reads* aelm's `.handoff/docs/` — every document is
//! copied into a fresh temp directory before `handoff_init`, so aelm's own
//! `.handoff/` is never written to.

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Instant;

fn send(input: &str) -> Option<Value> {
    let result = handoff_mcp::mcp::protocol::process_line(input)?;
    Some(serde_json::from_str(&result).expect("response should be valid JSON"))
}

fn call(dir: &Path, name: &str, mut args: Value) -> Value {
    args["project_dir"] = json!(dir.to_string_lossy());
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": args }
    });
    send(&req.to_string()).unwrap()
}

fn payload(resp: &Value) -> Value {
    let text = resp["result"]["content"][0]["text"]
        .as_str()
        .expect("content text");
    serde_json::from_str(text).expect("payload should be a JSON string")
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

/// Resolves the aelm `.handoff/docs/` directory to import from, per this
/// file's doc comment. Returns `None` (soft skip) rather than panicking
/// when neither the env override nor the default relative path exists.
fn resolve_aelm_docs_dir() -> Option<PathBuf> {
    if let Ok(p) = std::env::var("HANDOFF_AELM_DOCS_DIR") {
        let p = PathBuf::from(p);
        return p.is_dir().then_some(p);
    }
    let default = Path::new(env!("CARGO_MANIFEST_DIR")).join("../aelm/.handoff/docs");
    default.is_dir().then_some(default)
}

#[test]
#[ignore = "manual E2E against aelm's real project documents; see file doc comment"]
fn aelm_req_c_docs_import_dry_run_then_apply() {
    let Some(aelm_docs_dir) = resolve_aelm_docs_dir() else {
        eprintln!(
            "SKIP: aelm docs dir not found (set HANDOFF_AELM_DOCS_DIR or check out \
             ~/pro/aelm next to ~/pro/handoff-mcp) — nothing to do"
        );
        return;
    };

    // Copy every req-c*.md doc into a fresh temp project's .handoff/docs/ —
    // read-only against aelm's own checkout, never written to.
    let tmp = tempfile::tempdir().expect("temp dir");
    let project_dir = tmp.path().join("proj");
    std::fs::create_dir_all(&project_dir).unwrap();

    let resp = call(
        &project_dir,
        "handoff_init",
        json!({ "project_name": "aelm-req-import-e2e" }),
    );
    assert!(!is_error(&resp), "handoff_init failed");

    let docs_dest = project_dir.join(".handoff").join("docs");
    std::fs::create_dir_all(&docs_dest).unwrap();

    let mut doc_slugs: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&aelm_docs_dir).expect("read aelm docs dir") {
        let entry = entry.unwrap();
        let name = entry.file_name().to_string_lossy().to_string();
        if !name.starts_with("_doc.req-c") || !name.ends_with(".md") {
            continue;
        }
        std::fs::copy(entry.path(), docs_dest.join(&name)).expect("copy doc");
        let slug = name
            .strip_prefix("_doc.")
            .and_then(|s| s.strip_suffix(".md"))
            .unwrap()
            .to_string();
        doc_slugs.push(slug);
    }
    assert_eq!(
        doc_slugs.len(),
        29,
        "expected all 29 req-c01..c29 documents, found: {doc_slugs:?}"
    );
    doc_slugs.sort();

    // dry_run every document first — must never write anything, and must
    // not error even for documents this task's column-header mapping
    // doesn't fully recognize (parse_errors/warnings only).
    let mut total_would_create = 0u64;
    let mut total_would_update = 0u64;
    let mut per_doc_ms: Vec<(String, u128)> = Vec::new();
    for slug in &doc_slugs {
        let start = Instant::now();
        let resp = call(
            &project_dir,
            "handoff_doc_req_import",
            json!({ "doc_id": slug, "dry_run": true }),
        );
        let elapsed = start.elapsed().as_millis();
        per_doc_ms.push((slug.clone(), elapsed));
        assert!(
            !is_error(&resp),
            "dry_run must not error for {slug}: {resp}"
        );
        let p = payload(&resp);
        total_would_create += p["would_create"].as_u64().unwrap_or(0);
        total_would_update += p["would_update"].as_u64().unwrap_or(0);
        if std::env::var("HANDOFF_DEBUG_PER_DOC").is_ok() {
            let preview = p["preview"].as_array().cloned().unwrap_or_default();
            let with_priority = preview.iter().filter(|e| !e["priority"].is_null()).count();
            eprintln!(
                "  {slug}: candidates={} with_priority={} parse_errors={}",
                preview.len(),
                with_priority,
                p["parse_errors"].as_array().map(|a| a.len()).unwrap_or(0)
            );
        }
    }
    eprintln!(
        "dry_run totals: would_create={total_would_create} would_update={total_would_update}"
    );
    let worst = per_doc_ms.iter().max_by_key(|(_, ms)| *ms).unwrap();
    eprintln!(
        "slowest single-document dry_run: {} ({}ms)",
        worst.0, worst.1
    );
    // NFR budget (referral: "2,500 件規模でも 1 秒以内"): a single
    // document's own req_import call must stay far under 1s — the referral
    // headline figure is aelm's *whole-corpus* item count across 29
    // separate calls, not one call's own workload.
    assert!(
        worst.1 < 1000,
        "slowest single-document dry_run exceeded 1s: {worst:?}"
    );

    // Now actually import every document.
    let mut total_created = 0u64;
    let mut total_updated = 0u64;
    for slug in &doc_slugs {
        let resp = call(
            &project_dir,
            "handoff_doc_req_import",
            json!({ "doc_id": slug, "dry_run": false }),
        );
        assert!(!is_error(&resp), "apply must not error for {slug}: {resp}");
        let p = payload(&resp);
        total_created += p["created"].as_u64().unwrap_or(0);
        total_updated += p["updated"].as_u64().unwrap_or(0);
    }
    assert_eq!(
        total_created, total_would_create,
        "apply create count must match the dry_run preview"
    );
    assert_eq!(
        total_updated, total_would_update,
        "apply update count must match the dry_run preview"
    );

    // Aggregate via the real doc_req_list/doc_req_status entry points and
    // report the priority breakdown against the referral's figures
    // (P0 404 / P1 390 / P2 829 / P3 610, ~2,468 total) — printed for manual
    // comparison, not hard-asserted (this task's fixtures are aelm's own
    // living documents, which may have moved since the referral was filed).
    let resp = call(&project_dir, "handoff_doc_req_list", json!({ "limit": 1 }));
    assert!(!is_error(&resp), "{resp}");
    let total = payload(&resp)["total"].as_u64().unwrap_or(0);
    eprintln!("total imported requirements: {total} (referral figure: ~2,468)");

    let mut by_priority: std::collections::BTreeMap<String, u64> =
        std::collections::BTreeMap::new();
    for p in ["P0", "P1", "P2", "P3"] {
        let resp = call(
            &project_dir,
            "handoff_doc_req_list",
            json!({ "priority": p, "limit": 1 }),
        );
        assert!(!is_error(&resp), "{resp}");
        by_priority.insert(p.to_string(), payload(&resp)["total"].as_u64().unwrap_or(0));
    }
    eprintln!(
        "priority breakdown: {by_priority:?} (referral figures: P0 404 / P1 390 / P2 829 / P3 610)"
    );
}

//! M0/M1 backward-compat E2E test (t360.12, wiki/220-vmodel-integration-design.md
//! §5/§7, NFR-001/002): spawns the actual **current** `handoff-mcp` binary
//! against a project frozen by the real `main`-branch binary (see
//! `tests/fixtures/pre_m1_compat/README.md` for exactly how it was produced
//! and what is deliberately excluded from this comparison) and asserts that
//! `handoff_doc_req_list`/`handoff_doc_req_status`/`handoff_doc_verify_status`
//! reproduce `expected_output.json` byte-for-byte over the three pre-existing
//! shapes M0/M1 must not disturb: a layer-less document, a `C{n}`-prefixed
//! section-attached SubItem, and a freeform (`fragment_seq: null`) SubItem.
//!
//! This test only calls **read** tools and never re-runs
//! `handoff_doc_save`/`handoff_doc_req_import`/`handoff_doc_verify` mutating
//! actions against the fixture — see the README for why (M0's FR-806 fix
//! intentionally changed what a *fresh* `req_import` produces; this test
//! guards reading an *already-frozen* pre-M0/M1 project, not re-writing one).

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::Value;

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

fn fixture_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pre_m1_compat")
}

/// Copies `src` to `dst` recursively (`std::fs` has no built-in equivalent) —
/// same technique as `tests/trace_report_contract_fixture_e2e.rs`.
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

/// Sets up a fresh project directory from the committed fixture (`project/
/// handoff/`, named without a leading dot so the repo's blanket `.handoff/`
/// `.gitignore` rule doesn't swallow it — copied to `<tmp>/proj/.handoff`,
/// the real directory name every handoff-mcp command expects). The doc ids
/// baked into the copied `.md` frontmatter (and therefore every read tool's
/// output) never change, since this test never re-creates the documents.
fn setup_project(tmp: &Path) -> PathBuf {
    let proj = tmp.join("proj");
    copy_dir_recursive(
        &fixture_dir().join("project/handoff"),
        &proj.join(".handoff"),
    );
    proj
}

fn expected_output() -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(fixture_dir().join("expected_output.json")).unwrap(),
    )
    .unwrap()
}

const DOC_A_ID: &str = "doc-20260927-045154-945488"; // req-c01-legacy-spec
const DOC_B_ID: &str = "doc-20260927-045154-958010"; // req-c02-board-setup

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
}

impl Server {
    fn spawn() -> Self {
        let mut child = Command::new(binary())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to spawn handoff-mcp server");
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
        let req = serde_json::json!({
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

#[test]
fn req_list_req_status_and_doc_verify_status_match_pre_m0_m1_main_output() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let proj = setup_project(tmp.path());

    let mut server = Server::spawn();

    let req_list = server.call(
        "handoff_doc_req_list",
        serde_json::json!({ "project_dir": proj.to_string_lossy() }),
    );
    let req_status = server.call(
        "handoff_doc_req_status",
        serde_json::json!({ "project_dir": proj.to_string_lossy() }),
    );
    let verify_status_a = server.call(
        "handoff_doc_verify_status",
        serde_json::json!({
            "project_dir": proj.to_string_lossy(), "doc_id": DOC_A_ID, "include_items": true,
        }),
    );
    let verify_status_b = server.call(
        "handoff_doc_verify_status",
        serde_json::json!({
            "project_dir": proj.to_string_lossy(), "doc_id": DOC_B_ID, "include_items": true,
        }),
    );

    let expected = expected_output();

    assert_eq!(
        req_list, expected["req_list"],
        "handoff_doc_req_list output must be byte-for-byte unchanged for a \
         layer-less doc's C{{n}} SubItem and a freeform SubItem"
    );
    assert_eq!(
        req_status, expected["req_status"],
        "handoff_doc_req_status output must be byte-for-byte unchanged"
    );
    assert_eq!(
        verify_status_a, expected["verify_status_a"],
        "handoff_doc_verify_status must be byte-for-byte unchanged for the \
         layer-less document with a section-attached C{{n}} SubItem"
    );
    assert_eq!(
        verify_status_b, expected["verify_status_b"],
        "handoff_doc_verify_status must be byte-for-byte unchanged for the \
         document whose SubItems live inside a freeform (fragment_seq: null) item"
    );
}

/// t360.42 N7 (M1 adversarial review, wiki/220 §2.5 compat clause): unlike
/// the read-only test above, this drives the two *write-back* paths
/// (`handoff_doc_save`, `handoff_update_task`) against the fixture's legacy
/// task `t-legacy` — a pre-M1-shaped `TaskLink{link_type:"requirement",
/// label:"C01-FR-001"}` with no `role` key at all, reverse-linked from
/// `C01-FR-001`'s pre-existing `SubItem.task_ids: ["t-legacy"]` — and asserts
/// neither write path corrupts that legacy link. `handoff_update_task` also
/// exercises the S7 backfill: re-supplying the same (unchanged-membership)
/// `requirement_ids` must persist an inferred `role` onto the previously
/// role-less link rather than leaving it `None`.
#[test]
fn doc_save_and_update_task_do_not_corrupt_a_legacy_role_less_task_link() {
    let tmp = tempfile::tempdir().expect("temp dir");
    let proj = setup_project(tmp.path());

    let mut server = Server::spawn();

    // Precondition: the legacy link/task_ids shape is exactly as the fixture
    // committed it (no role key at all on disk).
    let task_before = server.call(
        "handoff_get_task",
        serde_json::json!({ "project_dir": proj.to_string_lossy(), "task_id": "t-legacy" }),
    );
    let link_before = task_before["task_links"]
        .as_array()
        .expect("task_links array")
        .iter()
        .find(|l| l["link_type"] == "requirement" && l["label"] == "C01-FR-001")
        .expect("legacy link present");
    assert!(
        link_before.get("role").is_none() || link_before["role"].is_null(),
        "precondition: fixture's legacy link must carry no role: {link_before}"
    );

    // 1) `doc_save`: a metadata-only resave (no body/layer/split_level
    // change) of the legacy document must not disturb C01-FR-001's
    // task_ids.
    let resave = server.call(
        "handoff_doc_save",
        serde_json::json!({
            "project_dir": proj.to_string_lossy(),
            "doc_id": DOC_A_ID,
            "tags": ["resaved"],
        }),
    );
    assert!(
        resave.get("warnings").is_some(),
        "unexpected doc_save response shape: {resave}"
    );

    let status_after_save = server.call(
        "handoff_doc_verify_status",
        serde_json::json!({
            "project_dir": proj.to_string_lossy(), "doc_id": DOC_A_ID, "include_items": true,
        }),
    );
    let sub_after_save = status_after_save["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["fragment_seq"] == 1)
        .unwrap()["sub_items"][0]
        .clone();
    assert_eq!(sub_after_save["stable_id"], "C01-FR-001");
    assert_eq!(
        sub_after_save["task_ids"].as_array().unwrap(),
        &vec![Value::String("t-legacy".to_string())],
        "doc_save must not disturb the legacy SubItem.task_ids link: {sub_after_save}"
    );

    // 2) `update_task` with the SAME requirement_ids (unchanged membership):
    // must not drop or duplicate the link, and must backfill the
    // previously-`None` role via S7 (inferred "implements" — C01-FR-001 has
    // no layer/category "check").
    server.call(
        "handoff_update_task",
        serde_json::json!({
            "project_dir": proj.to_string_lossy(),
            "task": { "id": "t-legacy", "requirement_ids": ["C01-FR-001"] },
        }),
    );

    let task_after = server.call(
        "handoff_get_task",
        serde_json::json!({ "project_dir": proj.to_string_lossy(), "task_id": "t-legacy" }),
    );
    let links_after = task_after["task_links"]
        .as_array()
        .expect("task_links array");
    assert_eq!(
        links_after
            .iter()
            .filter(|l| l["link_type"] == "requirement" && l["label"] == "C01-FR-001")
            .count(),
        1,
        "the legacy link must not be duplicated by an unchanged-membership update_task call: \
         {links_after:?}"
    );
    let link_after = links_after
        .iter()
        .find(|l| l["link_type"] == "requirement" && l["label"] == "C01-FR-001")
        .unwrap();
    assert_eq!(
        link_after["role"], "implements",
        "S7 backfill must persist an inferred role onto the previously role-less \
         legacy link: {link_after}"
    );

    let status_after_update = server.call(
        "handoff_doc_verify_status",
        serde_json::json!({
            "project_dir": proj.to_string_lossy(), "doc_id": DOC_A_ID, "include_items": true,
        }),
    );
    let sub_after_update = status_after_update["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["fragment_seq"] == 1)
        .unwrap()["sub_items"][0]
        .clone();
    assert_eq!(
        sub_after_update["task_ids"].as_array().unwrap(),
        &vec![Value::String("t-legacy".to_string())],
        "update_task must not disturb the legacy SubItem.task_ids link either: {sub_after_update}"
    );
}

//! Real-binary E2E for the implicit acceptance-verification items
//! (`REQ-001#AC1`, generated from a requirement's acceptance criteria) and for
//! suspect/hash consistency (t-vfix.5, t-vfix.6): the real `handoff-mcp`
//! binary over stdio JSON-RPC, a `minimal`-profile project (the profile that
//! materialises the implicit items) and a `standard` one (which does not).
//!
//! Direct body edits (a person editing the `.md` by hand, not going through a
//! tool) are simulated with `write_doc_body`, which leaves the stored
//! frontmatter — hashes included — as it was.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

use handoff_mcp::storage::docs::{read_doc_hashed, write_doc_body};

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    dir: PathBuf,
}

impl Client {
    fn spawn(project_dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_handoff-mcp"))
            .current_dir(project_dir)
            .env("HANDOFF_MCP_REQUEST_TIMEOUT_SECS", "900")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn handoff-mcp binary");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut client = Client {
            child,
            stdin,
            stdout,
            next_id: 0,
            dir: project_dir.to_path_buf(),
        };
        client.rpc(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "implicit_acceptance_suspect_e2e", "version": "0"}}),
        );
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        writeln!(client.stdin, "{note}").unwrap();
        client.stdin.flush().unwrap();
        client
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let line =
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        writeln!(self.stdin, "{line}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).expect("read response line");
        let resp: Value = serde_json::from_str(&buf)
            .unwrap_or_else(|e| panic!("invalid JSON-RPC response: {e}: {buf}"));
        assert_eq!(resp["id"], json!(self.next_id), "response id mismatch");
        resp
    }

    /// The tool's response text; panics when the call failed.
    fn text(&mut self, tool: &str, args: Value) -> String {
        let resp = self.rpc("tools/call", json!({"name": tool, "arguments": args}));
        let result = &resp["result"];
        let text: String = result["content"]
            .as_array()
            .map(|c| c.iter().filter_map(|x| x["text"].as_str()).collect())
            .unwrap_or_default();
        assert!(
            resp.get("error").is_none() && result["isError"] != json!(true),
            "{tool} failed: {text}"
        );
        text
    }

    fn json(&mut self, tool: &str, args: Value) -> Value {
        let text = self.text(tool, args);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{tool}: not JSON ({e}): {text}"))
    }

    fn handoff(&self) -> PathBuf {
        self.dir.join(".handoff")
    }

    fn init(&mut self, profile: &str) {
        self.text("handoff_init", json!({"project_name": "e2e"}));
        self.text(
            "handoff_update_config",
            json!({"updates": {"trace.profile": profile}}),
        );
    }

    fn save_doc(&mut self, slug: &str, layer: &str, body: &str) -> Value {
        let mut args = json!({"slug": slug, "title": slug, "layer": layer, "body": body});
        if let Some(existing) = read_doc_hashed(&self.handoff(), slug).unwrap() {
            args["doc_id"] = json!(existing.id);
        }
        self.json("handoff_doc_save", args)
    }

    /// Creates a task linked (`implements`) to `item` and completes it, which
    /// moves the item's `dev_stage` to `implemented`.
    fn implement(&mut self, item: &str) -> String {
        let created = self.text(
            "handoff_update_task",
            json!({"task": {"title": format!("Implement {item}")}}),
        );
        let task_id = created
            .trim_start_matches("Created task ")
            .split(':')
            .next()
            .unwrap()
            .to_string();
        self.text(
            "handoff_update_task",
            json!({"task": {"id": task_id, "requirement_ids": [item],
                            "schedule": {"estimate_hours": 1.0}}}),
        );
        self.text(
            "handoff_update_task",
            json!({"task": {"id": task_id, "status": "done"}}),
        );
        task_id
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn ids(v: &Value) -> Vec<String> {
    v.as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn new_requirement_ids(saved: &Value) -> Vec<String> {
    let mut out: Vec<String> = saved["warnings"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|w| w["code"] == "NEW_REQUIREMENT_DETECTED")
        .filter_map(|w| w["message"].as_str())
        .filter_map(|m| m.strip_prefix("New requirement "))
        .filter_map(|m| m.split_whitespace().next())
        .map(String::from)
        .collect();
    out.sort();
    out
}

const CHECKOUT_V1: &str = "# Cart\n\n\
### REQ-001 Checkout\n\n- kind: group\n- priority: P1\n\nTurn a cart into a paid order.\n\n\
受入基準:\n- AC1: Given a paid order When the shopper finishes Then a confirmation number is shown\n\n\
### REQ-002 Add item to cart\n\n- parent: REQ-001\n- priority: P1\n\nThe shopper adds one product.\n\n\
受入基準:\n- AC1: Given an empty cart When the shopper adds product X Then the cart holds 1 x X\n\
- AC2: Given X is out of stock When the shopper adds X Then the cart is unchanged\n";

/// Same document with REQ-002's AC1 reworded.
fn checkout_with_ac1_reworded() -> String {
    CHECKOUT_V1.replace("the cart holds 1 x X", "the cart shows exactly 1 x X")
}

#[test]
fn implicit_acceptance_items_in_a_minimal_project() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::spawn(tmp.path());
    c.init("minimal");

    // --- first save: only the authored leaf is announced; a group and the
    // generated `#ACn` items are not, and everything is "created", not
    // "changed".
    let saved = c.save_doc("cart", "requirement", CHECKOUT_V1);
    assert_eq!(new_requirement_ids(&saved), vec!["REQ-002"], "{saved}");
    let si = &saved["suspect_introduced"];
    assert_eq!(si["changed"], json!([]), "{si}");
    assert!(
        ids(&si["created"]).contains(&"REQ-002#AC1".to_string()),
        "{si}"
    );

    // --- the stored implicit item holds no link (and so no baseline).
    let doc = read_doc_hashed(&c.handoff(), "cart").unwrap().unwrap();
    let implicit = doc
        .verification
        .unwrap()
        .items
        .into_iter()
        .flat_map(|i| i.sub_items)
        .find(|s| s.stable_id.as_deref() == Some("REQ-002#AC1"))
        .expect("implicit item");
    assert_eq!(implicit.implicit_of.as_deref(), Some("REQ-002"));
    assert!(implicit.verifies.is_empty(), "{:?}", implicit.verifies);
    assert!(implicit.link_baselines.is_empty());

    // --- trace_tree without a layer: the generated items are not roots.
    let tree = c.json("handoff_trace_tree", json!({}));
    let root_ids: Vec<String> = tree["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_string())
        .collect();
    assert!(
        root_ids.iter().all(|id| !id.contains("#AC")),
        "implicit items must not be listed as roots: {root_ids:?}"
    );
    assert!(root_ids.contains(&"REQ-001".to_string()), "{root_ids:?}");
    let acceptance_tree = c.json("handoff_trace_tree", json!({"layer": "acceptance"}));
    let acceptance_roots: Vec<&str> = acceptance_tree["roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["id"].as_str().unwrap())
        .collect();
    assert!(
        acceptance_roots.contains(&"REQ-002#AC1"),
        "asking for the acceptance layer still shows them: {acceptance_roots:?}"
    );

    // --- trace_next for the implementing task reaches the implicit items,
    // asks for a record (not an ingest), and never for a refine.
    let task_id = c.implement("REQ-002");
    let next = c.json(
        "handoff_trace_next",
        json!({"task_id": task_id, "limit": 50}),
    );
    let actions = next["actions"].as_array().unwrap();
    let rerun_items: HashSet<&str> = actions
        .iter()
        .filter(|a| a["kind"] == "rerun")
        .filter_map(|a| a["item"].as_str())
        .collect();
    assert!(
        rerun_items.contains("REQ-002#AC1") && rerun_items.contains("REQ-002#AC2"),
        "task-scoped trace_next must include its items' implicit verifications: {next}"
    );
    assert!(
        !rerun_items.contains("REQ-001#AC1"),
        "another item's implicit verification is out of the task's scope: {next}"
    );
    for a in actions {
        assert_ne!(
            a["kind"], "refine",
            "minimal has no layer to refine into: {a}"
        );
        assert_ne!(
            a["suggest"]["tool"], "handoff_trace_ingest",
            "a criterion run by hand is recorded, not ingested: {a}"
        );
    }
    let whole = c.json("handoff_trace_next", json!({"limit": 50}));
    for a in whole["actions"].as_array().unwrap() {
        assert_ne!(a["kind"], "refine", "{a}");
    }

    // --- a recorded pass goes stale when its criterion is reworded: a
    // result suspect and a re-verify, but no link suspect pointing at itself.
    c.text(
        "handoff_trace_record",
        json!({"results": [{"item": "REQ-002#AC1", "result": "pass"}]}),
    );
    let edited = c.save_doc("cart", "requirement", &checkout_with_ac1_reworded());
    let si = &edited["suspect_introduced"];
    assert!(
        ids(&si["changed"]).contains(&"REQ-002#AC1".to_string()),
        "{si}"
    );
    assert_eq!(si["created"], json!([]), "{si}");
    assert!(
        ids(&si["reverify"]).contains(&"REQ-002#AC1".to_string()),
        "{si}"
    );

    let suspects = c.json("handoff_trace_suspect", json!({"action": "list"}));
    for s in suspects["suspects"].as_array().unwrap() {
        assert!(
            s["upstream"].as_str() != s["item"].as_str(),
            "a suspect must never point an item at itself: {s}"
        );
        assert_ne!(
            (s["kind"].as_str(), s["item"].as_str()),
            (Some("link"), Some("REQ-002#AC1")),
            "{s}"
        );
    }
    assert!(
        suspects["suspects"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| { s["kind"] == "result" && s["item"] == "REQ-002#AC1" }),
        "the recorded pass is stale: {suspects}"
    );
    assert!(
        ids(&suspects["reverify"]).contains(&"REQ-002#AC1".to_string()),
        "{suspects}"
    );
    assert_eq!(suspects["unbaselined"]["links"], json!(0), "{suspects}");

    let lint = c.json("handoff_trace_lint", json!({}));
    for f in lint["findings"].as_array().unwrap() {
        assert!(
            !(f["rule"] == "suspect_link" && f["item"].as_str().is_some_and(|i| i.contains("#AC"))),
            "no suspect link on an implicit item: {f}"
        );
    }

    // --- the stale implicit item is the thing to re-run, and it is recorded
    // (the task-scoped call still sees it).
    let next = c.json(
        "handoff_trace_next",
        json!({"task_id": task_id, "limit": 50}),
    );
    let rerun = next["actions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|a| a["kind"] == "rerun" && a["item"] == "REQ-002#AC1")
        .unwrap_or_else(|| panic!("stale implicit item must be a rerun: {next}"));
    assert_eq!(rerun["suggest"]["tool"], "handoff_trace_record", "{rerun}");
}

#[test]
fn report_items_show_the_implicit_item_verifying_its_parent_criterion() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::spawn(tmp.path());
    c.init("minimal");
    c.save_doc("cart", "requirement", CHECKOUT_V1);

    // The stored item holds no link, but the report must agree with the graph
    // (which treats `REQ-002#AC1` as verifying the `REQ-002#AC1` sub-reference,
    // i.e. AC1 of REQ-002): consumers build parent -> verifier relations from
    // `items[].verifies`.
    let report = c.json("handoff_trace_report", json!({"include_items": true}));
    let item = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-002#AC1")
        .unwrap_or_else(|| panic!("implicit item missing from report: {report}"));
    assert_eq!(item["implicit_of"], json!("REQ-002"), "{item}");
    assert_eq!(item["verifies"], json!(["REQ-002#AC1"]), "{item}");

    let authored = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-002")
        .unwrap();
    assert_eq!(authored["verifies"], json!([]), "{authored}");
}

const SYSTEM_TEST_AC1: &str =
    "# System tests\n\n### ST-001 Lock check\n\n- verifies: FR-001#AC1\n- method: manual\n\nTry five bad passwords.\n";

const LOCK_V1: &str = "# Auth\n\n### FR-001 Account lock\n\nLock after repeated failures.\n\n\
受入基準:\n- AC1: Given 4 failures When the 5th fails Then the account is locked\n\
- AC2: Given a locked account When 15 minutes pass Then login works again\n";

#[test]
fn ac_level_link_goes_suspect_without_implicit_items() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::spawn(tmp.path());
    c.init("standard");

    c.save_doc("auth", "requirement", LOCK_V1);
    c.save_doc("auth-st", "system_test", SYSTEM_TEST_AC1);
    let suspects = c.json("handoff_trace_suspect", json!({"action": "list"}));
    assert_eq!(suspects["counts"]["link"], json!(0), "{suspects}");
    assert_eq!(suspects["unbaselined"]["links"], json!(0), "{suspects}");

    // AC2 reworded: a link to AC1 is unaffected.
    c.save_doc(
        "auth",
        "requirement",
        &LOCK_V1.replace("login works again", "login works"),
    );
    let suspects = c.json("handoff_trace_suspect", json!({"action": "list"}));
    assert_eq!(suspects["counts"]["link"], json!(0), "{suspects}");

    // AC1 reworded: the link to AC1 is suspect, and lint says which way.
    let saved = c.save_doc(
        "auth",
        "requirement",
        &LOCK_V1.replace("the account is locked", "the account is locked out"),
    );
    assert!(
        saved["suspect_introduced"]["links"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l["child"] == "ST-001" && l["upstream"] == "FR-001#AC1"),
        "{saved}"
    );
    let suspects = c.json("handoff_trace_suspect", json!({"action": "list"}));
    let link = suspects["suspects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["kind"] == "link" && s["item"] == "ST-001")
        .unwrap_or_else(|| panic!("AC-level link must be suspect: {suspects}"));
    assert_eq!(link["upstream"], "FR-001#AC1");

    let lint = c.json("handoff_trace_lint", json!({}));
    let finding = lint["findings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["rule"] == "suspect_link")
        .unwrap_or_else(|| panic!("suspect_link finding: {lint}"));
    let message = finding["message"].as_str().unwrap();
    assert!(
        message.starts_with("ST-001 verifies FR-001#AC1, which has changed"),
        "the verifier comes first, the changed upstream second: {message}"
    );
}

fn junit(cases: &[(&str, &str)]) -> String {
    let mut out = String::from("<testsuites><testsuite name=\"suite\">");
    for (name, _) in cases {
        out.push_str(&format!("<testcase classname=\"suite\" name=\"{name}\"/>"));
    }
    out.push_str("</testsuite></testsuites>");
    out
}

const STS_V1: &str = "# System tests\n\n\
### ST-001 First\n\n- verifies: FR-001\n- method: auto\n- test: suite::case_one\n\nFirst check.\n";

fn sts_hand_edited() -> String {
    format!(
        "{}\n### ST-002 Second\n\n- verifies: FR-001\n- method: auto\n- test: suite::case_two\n\nSecond check.\n",
        STS_V1.replace("First check.", "First check, reworded by hand.")
    )
}

fn result_suspects(c: &mut Client) -> Vec<String> {
    let suspects = c.json("handoff_trace_suspect", json!({"action": "list"}));
    suspects["suspects"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| s["kind"] == "result")
        .map(|s| s["item"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn ingest_resyncs_documents_edited_by_hand_before_recording() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::spawn(tmp.path());
    c.init("standard");
    c.save_doc("auth", "requirement", LOCK_V1);
    c.save_doc("auth-st", "system_test", STS_V1);

    // Edit the body by hand: ST-001 reworded, ST-002 added.
    write_doc_body(&c.handoff(), "auth-st", &sts_hand_edited()).unwrap();

    // A dry run sees the hand-added item and writes nothing.
    let before = std::fs::read_to_string(c.handoff().join("docs/_doc.auth-st.md")).unwrap();
    let dry = c.json(
        "handoff_trace_ingest",
        json!({"format": "junit_xml", "dry_run": true,
               "output": junit(&[("case_one", "pass"), ("case_two", "pass")])}),
    );
    let matched: HashSet<&str> = dry["matched"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|m| m["item"].as_str())
        .collect();
    assert!(matched.contains("ST-002"), "{dry}");
    assert_eq!(
        before,
        std::fs::read_to_string(c.handoff().join("docs/_doc.auth-st.md")).unwrap(),
        "a dry run must not write"
    );

    // The real run records against today's hashes: no stale-result suspect.
    let out = c.json(
        "handoff_trace_ingest",
        json!({"format": "junit_xml",
               "output": junit(&[("case_one", "pass"), ("case_two", "pass")])}),
    );
    assert_eq!(out["recorded"], json!(2), "{out}");
    assert_eq!(result_suspects(&mut c), Vec::<String>::new());
}

#[test]
fn record_stores_the_hash_of_an_item_added_by_hand() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::spawn(tmp.path());
    c.init("standard");
    c.save_doc("auth", "requirement", LOCK_V1);
    c.save_doc("auth-st", "system_test", STS_V1);
    write_doc_body(&c.handoff(), "auth-st", &sts_hand_edited()).unwrap();

    c.text(
        "handoff_trace_record",
        json!({"results": [{"item": "ST-002", "result": "pass"}]}),
    );
    assert_eq!(result_suspects(&mut c), Vec::<String>::new());

    // Reword ST-002: the recorded pass must now be stale, which it can only
    // be if the record carried the hash of the definition it ran against.
    c.save_doc(
        "auth-st",
        "system_test",
        &sts_hand_edited().replace("Second check.", "Second check, changed."),
    );
    assert_eq!(result_suspects(&mut c), vec!["ST-002".to_string()]);
}

#[test]
fn test_run_create_sees_items_added_by_hand() {
    let tmp = tempfile::tempdir().unwrap();
    let mut c = Client::spawn(tmp.path());
    c.init("standard");
    c.save_doc("auth", "requirement", LOCK_V1);
    c.save_doc("auth-st", "system_test", STS_V1);
    write_doc_body(&c.handoff(), "auth-st", &sts_hand_edited()).unwrap();

    let run = c.json(
        "handoff_trace_test_run",
        json!({"action": "create", "scope": {"layers": ["system_test"]}}),
    );
    let targets = ids(&run["target_items"]);
    assert!(
        targets.contains(&"ST-001".to_string()) && targets.contains(&"ST-002".to_string()),
        "the frozen target set must include the item added by hand: {run}"
    );
}

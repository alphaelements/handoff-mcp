//! Real-binary E2E tests for `_trace_report.json` schema v3 (requirement
//! tree, wiki/300-requirement-tree-design.md SPEC-TREE-007): the per-item
//! `parents` / `parent_declared` / `children` / `kind` / `order` / `subtree`
//! fields, the top-level `tree` and `totals` blocks, and the tree-structure
//! entries in `gaps[]`.
//!
//! Also guards the contract fixtures handoff-vscode reads
//! (`tests/fixtures/trace_report_v3_sample.json` + its JSON Schema
//! `tests/fixtures/trace_report_v3.schema.json`): the committed sample must
//! satisfy the schema, and so must a freshly produced report.
//!
//! Regenerate the sample after an intentional shape change with
//! `UPDATE_TRACE_REPORT_V3_SAMPLE=1 cargo test --test trace_report_v3_e2e`.

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

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
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
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            break;
                        }
                    }
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

fn init_project(server: &mut Server, dir: &Path) {
    server.call(
        "handoff_init",
        json!({ "project_dir": dir.to_string_lossy(), "project_name": "trace-report-v3-e2e" }),
    );
}

fn save_requirements(server: &mut Server, dir: &Path, body: &str) {
    server.call(
        "handoff_doc_save",
        json!({
            "project_dir": dir.to_string_lossy(),
            "slug": "requirements-v3-e2e",
            "title": "Requirements",
            "layer": "requirement",
            "body": body,
        }),
    );
}

/// A DAG: `REQ-003` is shared by `REQ-001` and `REQ-002`; `REQ-005` is a
/// `decision` under `REQ-004`; `REQ-006` is an empty `group` (a tree gap).
const DAG_BODY: &str = "# Requirements\n\n\
### REQ-001 Root one\n\n- order: 1\n\nFirst root.\n\n\
### REQ-002 Root two\n\n- order: 2\n\nSecond root.\n\n\
### REQ-003 Shared child\n\n- parent: REQ-001, REQ-002\n\nShared by both roots.\n\n\
### REQ-004 Branch\n\n- parent: REQ-001\n- order: 1\n\nA branch.\n\n\
### REQ-005 Decision\n\n- parent: REQ-004\n- kind: decision\n\nA decision.\n\n\
### REQ-006 Empty group\n\n- kind: group\n\nNo children yet.\n";

fn build_dag_report(dir: &Path) -> Value {
    let mut server = Server::spawn();
    init_project(&mut server, dir);
    save_requirements(&mut server, dir, DAG_BODY);
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": dir.to_string_lossy() }),
    );
    read_report(dir)
}

fn read_report(dir: &Path) -> Value {
    let path = dir.join(".handoff/docs/_trace_report.json");
    serde_json::from_str(&std::fs::read_to_string(path).expect("report written"))
        .expect("report is JSON")
}

fn item<'a>(report: &'a Value, id: &str) -> &'a Value {
    report["items"]
        .as_array()
        .expect("items[]")
        .iter()
        .find(|i| i["id"] == id)
        .unwrap_or_else(|| panic!("item {id} missing"))
}

fn strings(v: &Value) -> Vec<&str> {
    v.as_array()
        .expect("array")
        .iter()
        .map(|s| s.as_str().expect("string"))
        .collect()
}

#[test]
fn report_is_schema_version_3_with_per_item_tree_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let report = build_dag_report(tmp.path());
    assert_eq!(report["schema_version"], 3);

    let shared = item(&report, "REQ-003");
    assert_eq!(strings(&shared["parents"]), ["REQ-001", "REQ-002"]);
    assert_eq!(strings(&shared["parent_declared"]), ["REQ-001", "REQ-002"]);
    assert_eq!(shared["kind"], "leaf");
    assert_eq!(shared["children"], json!([]));
    assert_eq!(shared["order"], Value::Null);

    let root = item(&report, "REQ-001");
    assert_eq!(root["parents"], json!([]));
    assert_eq!(root["parent_declared"], Value::Null);
    assert_eq!(root["kind"], "composite");
    // REQ-004 (order 1) sorts before REQ-003 (no order).
    assert_eq!(strings(&root["children"]), ["REQ-004", "REQ-003"]);
    assert_eq!(root["order"], 1);

    assert_eq!(item(&report, "REQ-005")["kind"], "decision");
    assert_eq!(item(&report, "REQ-006")["kind"], "composite");
}

#[test]
fn subtree_always_carries_every_key_and_double_counts_a_shared_node() {
    let tmp = tempfile::tempdir().unwrap();
    let report = build_dag_report(tmp.path());

    let leaf = &item(&report, "REQ-003")["subtree"];
    let keys: Vec<&str> = leaf
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    for k in [
        "depth",
        "parents",
        "children",
        "leaves",
        "state",
        "dev_stage",
        "approval",
        "excluded",
        "suspect",
        "progress",
        "tasks",
        "all_tasks",
    ] {
        assert!(keys.contains(&k), "subtree.{k} must always be present");
    }
    // Zeroes are emitted, not omitted.
    assert_eq!(leaf["suspect"], 0);
    assert_eq!(leaf["all_tasks"], 0);
    assert_eq!(leaf["excluded"]["waived"], 0);
    assert!(leaf["excluded"]["na"].is_u64());
    assert_eq!(leaf["depth"], 1);

    // REQ-001's subtree = REQ-003 + REQ-005 (REQ-004 is a branch);
    // REQ-002's subtree = REQ-003. The shared leaf appears in both.
    assert_eq!(item(&report, "REQ-001")["subtree"]["leaves"], 2);
    assert_eq!(item(&report, "REQ-002")["subtree"]["leaves"], 1);
}

#[test]
fn totals_and_tree_blocks_are_deduplicated() {
    let tmp = tempfile::tempdir().unwrap();
    let report = build_dag_report(tmp.path());

    let totals = &report["totals"];
    assert_eq!(totals["nodes"], 6);
    // Roll-up leaves: REQ-003 and REQ-005 (a childless decision is a leaf);
    // each counted once although REQ-003 has two parents.
    assert_eq!(totals["leaves"], 2);
    assert_eq!(totals["dag_shared"], 1);
    assert_eq!(totals["max_depth"], 2);
    assert!(totals["state"].is_object());
    assert!(totals["progress"].is_object());

    let tree = &report["tree"];
    assert_eq!(
        strings(&tree["roots"]["requirement"]),
        ["REQ-001", "REQ-002", "REQ-006"]
    );
    assert_eq!(tree["max_depth"], totals["max_depth"]);
    assert_eq!(tree["dag_shared_count"], 1);
    assert_eq!(tree["leaf_count"], totals["leaves"]);
    assert_eq!(tree["composite_count"], totals["composites"]);
    assert_eq!(tree["decision_count"], totals["decisions"]);
}

#[test]
fn tree_structure_findings_are_listed_in_gaps_and_gap_counts() {
    let tmp = tempfile::tempdir().unwrap();
    let report = build_dag_report(tmp.path());

    let gap = report["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .find(|g| g["kind"] == "empty_group")
        .expect("empty_group gap for the childless group");
    assert_eq!(gap["item"], "REQ-006");
    assert_eq!(gap["layer"], "requirement");
    assert_eq!(report["gap_counts"]["empty_group"], 1);
}

#[test]
fn project_without_parents_is_all_roots_and_leaves() {
    let tmp = tempfile::tempdir().unwrap();
    let mut server = Server::spawn();
    init_project(&mut server, tmp.path());
    save_requirements(
        &mut server,
        tmp.path(),
        "# Requirements\n\n### REQ-001 One\n\nA.\n\n### REQ-002 Two\n\nB.\n",
    );
    server.call(
        "handoff_trace_report",
        json!({ "project_dir": tmp.path().to_string_lossy() }),
    );
    let report = read_report(tmp.path());

    assert_eq!(report["schema_version"], 3);
    for id in ["REQ-001", "REQ-002"] {
        let it = item(&report, id);
        assert_eq!(it["parents"], json!([]));
        assert_eq!(it["children"], json!([]));
        assert_eq!(it["parent_declared"], Value::Null);
        assert_eq!(it["kind"], "leaf");
        assert_eq!(it["subtree"]["depth"], 0);
        assert_eq!(it["subtree"]["leaves"], 1);
    }
    assert_eq!(
        strings(&report["tree"]["roots"]["requirement"]),
        ["REQ-001", "REQ-002"]
    );
    assert_eq!(report["totals"]["nodes"], 2);
    assert_eq!(report["totals"]["leaves"], 2);
    assert_eq!(report["totals"]["composites"], 0);
    assert_eq!(report["totals"]["dag_shared"], 0);
    assert_eq!(report["gap_counts"].get("empty_group"), None);
}

#[test]
fn mcp_response_include_items_shares_the_tree_item_fields() {
    let tmp = tempfile::tempdir().unwrap();
    let mut server = Server::spawn();
    init_project(&mut server, tmp.path());
    save_requirements(&mut server, tmp.path(), DAG_BODY);
    let resp = server.call(
        "handoff_trace_report",
        json!({ "project_dir": tmp.path().to_string_lossy(), "include_items": true }),
    );
    let persisted = read_report(tmp.path());
    assert_eq!(resp["items"], persisted["items"]);
    assert_eq!(resp["totals"], persisted["totals"]);
    assert_eq!(resp["tree"], persisted["tree"]);
}

// ---------------------------------------------------------------------
// Contract fixtures
// ---------------------------------------------------------------------

fn load_json(path: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap_or_else(|e| {
        panic!("{}: {e}", path.display());
    }))
    .unwrap()
}

/// A minimal JSON-Schema (draft 2020-12 subset) validator: `type` (string or
/// array), `properties`, `required`, `additionalProperties` (bool), `items`,
/// `enum`, `minimum`, `oneOf` (valid when any branch matches), `$ref` into
/// `#/$defs/`. Enough for the contract
/// schema; any other keyword the schema uses is rejected so it cannot be
/// silently ignored.
fn validate(schema: &Value, root: &Value, value: &Value, path: &str, errors: &mut Vec<String>) {
    const KNOWN: &[&str] = &[
        "$schema",
        "$id",
        "$defs",
        "$ref",
        "title",
        "description",
        "type",
        "properties",
        "required",
        "additionalProperties",
        "items",
        "enum",
        "minimum",
        "oneOf",
    ];
    let Some(obj) = schema.as_object() else {
        return;
    };
    for key in obj.keys() {
        assert!(
            KNOWN.contains(&key.as_str()),
            "schema keyword {key} is not supported by the test validator"
        );
    }
    if let Some(r) = obj.get("$ref").and_then(Value::as_str) {
        let name = r
            .strip_prefix("#/$defs/")
            .unwrap_or_else(|| panic!("unsupported $ref {r}"));
        validate(&root["$defs"][name], root, value, path, errors);
        return;
    }
    if let Some(branches) = obj.get("oneOf").and_then(Value::as_array) {
        let matches_any = branches.iter().any(|b| {
            let mut branch_errors = Vec::new();
            validate(b, root, value, path, &mut branch_errors);
            branch_errors.is_empty()
        });
        if !matches_any {
            errors.push(format!("{path}: {value} matches no oneOf branch"));
        }
    }
    if let Some(t) = obj.get("type") {
        let types: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => panic!("bad type"),
        };
        let ok = types.iter().any(|t| match *t {
            "object" => value.is_object(),
            "array" => value.is_array(),
            "string" => value.is_string(),
            "integer" => value.is_i64() || value.is_u64(),
            "number" => value.is_number(),
            "boolean" => value.is_boolean(),
            "null" => value.is_null(),
            other => panic!("unknown type {other}"),
        });
        if !ok {
            errors.push(format!("{path}: expected {types:?}, got {value}"));
            return;
        }
    }
    if let Some(e) = obj.get("enum").and_then(Value::as_array) {
        if !e.contains(value) {
            errors.push(format!("{path}: {value} not in enum {e:?}"));
        }
    }
    if let (Some(min), Some(n)) = (obj.get("minimum").and_then(Value::as_f64), value.as_f64()) {
        if n < min {
            errors.push(format!("{path}: {n} < minimum {min}"));
        }
    }
    if let Some(map) = value.as_object() {
        let props = obj.get("properties").and_then(Value::as_object);
        if let Some(req) = obj.get("required").and_then(Value::as_array) {
            for r in req.iter().filter_map(Value::as_str) {
                if !map.contains_key(r) {
                    errors.push(format!("{path}: missing required key {r}"));
                }
            }
        }
        for (k, v) in map {
            match props.and_then(|p| p.get(k)) {
                Some(sub) => validate(sub, root, v, &format!("{path}.{k}"), errors),
                None => {
                    if obj.get("additionalProperties") == Some(&json!(false)) {
                        errors.push(format!("{path}: unexpected key {k}"));
                    } else if let Some(sub) = obj.get("additionalProperties") {
                        validate(sub, root, v, &format!("{path}.{k}"), errors);
                    }
                }
            }
        }
    }
    if let (Some(arr), Some(items)) = (value.as_array(), obj.get("items")) {
        for (i, v) in arr.iter().enumerate() {
            validate(items, root, v, &format!("{path}[{i}]"), errors);
        }
    }
}

fn assert_valid(report: &Value) {
    let schema = load_json(&fixtures_dir().join("trace_report_v3.schema.json"));
    let mut errors = Vec::new();
    validate(&schema, &schema, report, "$", &mut errors);
    assert!(
        errors.is_empty(),
        "schema violations:\n{}",
        errors.join("\n")
    );
}

/// Fields that differ between two runs of the same project (random document
/// ids, wall-clock mtimes) and so are not part of the sample's contract.
fn normalize(mut report: Value) -> Value {
    if let Some(inputs) = report.get_mut("inputs").and_then(Value::as_object_mut) {
        inputs.insert("docs_max_mtime_ns".into(), json!(0));
        inputs.insert("tasks_max_mtime_ns".into(), json!(0));
    }
    report
}

#[test]
fn live_report_satisfies_the_v3_json_schema() {
    let tmp = tempfile::tempdir().unwrap();
    assert_valid(&build_dag_report(tmp.path()));
}

#[test]
fn committed_sample_satisfies_the_v3_json_schema_and_is_a_dag() {
    let sample = load_json(&fixtures_dir().join("trace_report_v3_sample.json"));
    assert_valid(&sample);
    assert_eq!(sample["schema_version"], 3);
    assert!(
        sample["totals"]["dag_shared"].as_u64().unwrap() >= 1,
        "the sample must contain a node with several parents"
    );
}

#[test]
fn committed_sample_is_internally_consistent() {
    let sample = load_json(&fixtures_dir().join("trace_report_v3_sample.json"));
    let items = sample["items"].as_array().unwrap();
    let find = |id: &str| items.iter().find(|i| i["id"] == id).unwrap();
    for it in items {
        let id = it["id"].as_str().unwrap();
        for p in strings(&it["parents"]) {
            assert!(
                strings(&find(p)["children"]).contains(&id),
                "{id} lists {p} as parent but {p} does not list it as a child"
            );
        }
        for c in strings(&it["children"]) {
            assert!(
                strings(&find(c)["parents"]).contains(&id),
                "{id} lists {c} as child but {c} does not list it as a parent"
            );
        }
    }
}

#[test]
fn sample_matches_what_the_binary_produces_for_the_dag_project() {
    let tmp = tempfile::tempdir().unwrap();
    let live = normalize(build_dag_report(tmp.path()));
    let path = fixtures_dir().join("trace_report_v3_sample.json");
    if std::env::var_os("UPDATE_TRACE_REPORT_V3_SAMPLE").is_some() {
        // Document ids and timestamps are random per run; blank them so the
        // regenerated file is stable and reviewable.
        let mut sample = live.clone();
        stabilize(&mut sample);
        std::fs::write(&path, serde_json::to_string_pretty(&sample).unwrap() + "\n").unwrap();
        return;
    }
    let mut live = live;
    stabilize(&mut live);
    assert_eq!(
        live,
        load_json(&path),
        "tests/fixtures/trace_report_v3_sample.json is stale; regenerate with \
         UPDATE_TRACE_REPORT_V3_SAMPLE=1"
    );
}

/// Replaces every value that is random per run with a fixed placeholder.
fn stabilize(report: &mut Value) {
    fn walk(v: &mut Value) {
        match v {
            Value::Object(map) => {
                let keys: Vec<String> = map.keys().cloned().collect();
                for k in keys {
                    let randomish = matches!(
                        k.as_str(),
                        "doc" | "docs_max_mtime_ns" | "tasks_max_mtime_ns"
                    );
                    if randomish {
                        if map[&k].is_string() {
                            map.insert(k, json!("doc-sample"));
                        } else if map[&k].is_number() {
                            map.insert(k, json!(0));
                        }
                    } else if let Some(child) = map.get_mut(&k) {
                        walk(child);
                    }
                }
            }
            Value::Array(arr) => arr.iter_mut().for_each(walk),
            _ => {}
        }
    }
    walk(report);
}

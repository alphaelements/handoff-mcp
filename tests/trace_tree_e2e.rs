//! Real-binary E2E for the requirement tree (wiki/300-requirement-tree-design.md,
//! `.handoff` docs `req-requirement-tree` / `spec-requirement-tree` /
//! `test-requirement-tree`): spawns the actual `handoff-mcp` binary over
//! stdio JSON-RPC, drives `handoff_doc_save` / `handoff_trace_update` /
//! `handoff_trace_report`, and inspects the persisted `_doc.<slug>.md`
//! frontmatter. Covers ST-TREE-001 (attribute parsing + hash exclusion) and
//! ST-TREE-002 (create-time validation); the engine-level graph / kind /
//! coverage / rollup tests (ST-TREE-003..006) live in the second half of this
//! file and drive `TraceGraph::build` directly.

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

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    project: PathBuf,
}

impl Server {
    /// Spawns the server and initializes a fresh project in `dir`.
    fn start(dir: &Path) -> Self {
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
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut server = Server {
            child,
            stdin,
            lines: rx,
            next_id: 1,
            project: dir.to_path_buf(),
        };
        let init = server.raw(
            "handoff_init",
            json!({"project_dir": dir.to_string_lossy(), "project_name": "tree-e2e"}),
        );
        assert!(init.get("error").is_none(), "init failed: {init}");
        server
    }

    /// Calls a tool without injecting `project_dir`.
    fn raw(&mut self, name: &str, arguments: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        });
        writeln!(self.stdin, "{req}").expect("write to server stdin");
        self.stdin.flush().expect("flush server stdin");
        let line = self
            .lines
            .recv_timeout(Duration::from_secs(20))
            .unwrap_or_else(|_| panic!("no response for {name} within 20s"));
        let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
        assert_eq!(resp["id"], id, "response id must match request id");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_default();
        if resp["result"]["isError"].as_bool().unwrap_or(false) {
            return json!({"error": {"message": text}});
        }
        serde_json::from_str(text).unwrap_or(Value::Null)
    }

    /// Calls a tool against this server's project.
    fn call(&mut self, name: &str, mut arguments: Value) -> Value {
        arguments["project_dir"] = json!(self.project.to_string_lossy());
        self.raw(name, arguments)
    }

    fn save_layer(&mut self, slug: &str, layer: &str, body: &str) -> Value {
        self.call(
            "handoff_doc_save",
            json!({"slug": slug, "title": slug, "body": body, "layer": layer}),
        )
    }

    fn doc_path(&self, slug: &str) -> PathBuf {
        self.project
            .join(".handoff/docs")
            .join(format!("_doc.{slug}.md"))
    }

    fn body_of(&self, slug: &str) -> String {
        let content = std::fs::read_to_string(self.doc_path(slug)).expect("doc file");
        // Strip the frontmatter fence.
        let mut parts = content.splitn(3, "---\n");
        parts.next();
        parts.next();
        parts.next().unwrap_or_default().to_string()
    }

    fn sub_item(&self, slug: &str, stable_id: &str) -> serde_yaml::Value {
        let content = std::fs::read_to_string(self.doc_path(slug)).expect("doc file");
        let mut lines = content.lines();
        assert_eq!(lines.next(), Some("---"));
        let yaml: String = lines
            .by_ref()
            .take_while(|l| *l != "---")
            .collect::<Vec<_>>()
            .join("\n");
        let fm: serde_yaml::Value = serde_yaml::from_str(&yaml).expect("frontmatter yaml");
        fm["verification"]["items"]
            .as_sequence()
            .expect("verification.items")
            .iter()
            .flat_map(|item| {
                item["sub_items"]
                    .as_sequence()
                    .map_or(&[][..], |s| s.as_slice())
            })
            .find(|sub| sub["stable_id"].as_str() == Some(stable_id))
            .unwrap_or_else(|| panic!("{stable_id} not in frontmatter of {slug}"))
            .clone()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn warnings_of(v: &Value) -> Vec<String> {
    v["warnings"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|w| w.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn yaml_strings(v: &serde_yaml::Value) -> Vec<String> {
    v.as_sequence()
        .map(|s| {
            s.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------------------
// ST-TREE-001
// ---------------------------------------------------------------------------

#[test]
fn st_tree_001_parse_attrs_and_hash_exclusion() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());

    let roots = "# Req\n\n### REQ-001 Root A\n\nA.\n\n### REQ-002 Root B\n\nB.\n\n";
    let body =
        |attrs: &str| format!("{roots}### REQ-010 Child\n\n- priority: P1\n{attrs}\nBody text.\n");

    // parent: single, comma list, and two repeated lines (merged, de-duplicated);
    // order: valid; order@<parent>: per-parent; kind: decision.
    let saved = s.save_layer(
        "req-tree",
        "requirement",
        &body(
            "- parent: REQ-001\n- parent: REQ-002 , REQ-001\n- order: 3\n\
             - order@REQ-002: 7\n- kind: decision\n",
        ),
    );
    assert!(saved.get("error").is_none(), "{saved}");
    let sub = s.sub_item("req-tree", "REQ-010");
    assert_eq!(
        yaml_strings(&sub["tree_parents"]),
        vec!["REQ-001", "REQ-002"]
    );
    assert_eq!(sub["tree_order"].as_u64(), Some(3));
    assert_eq!(sub["tree_order_per_parent"]["REQ-002"].as_u64(), Some(7));
    assert_eq!(sub["tree_kind_decl"].as_str(), Some("decision"));
    let (def_hash, body_hash) = (
        sub["def_hash"].as_str().unwrap().to_string(),
        sub["body_hash"].as_str().unwrap().to_string(),
    );

    // Changing only parent / order / kind must not move either hash.
    let id = {
        let content = std::fs::read_to_string(s.doc_path("req-tree")).unwrap();
        content
            .lines()
            .find_map(|l| l.strip_prefix("id: "))
            .expect("doc id in frontmatter")
            .to_string()
    };
    let resaved = s.call(
        "handoff_doc_save",
        json!({"doc_id": id, "body": body("- parent: REQ-002\n- order: 9\n- kind: group\n")}),
    );
    assert!(resaved.get("error").is_none(), "{resaved}");
    let sub = s.sub_item("req-tree", "REQ-010");
    assert_eq!(yaml_strings(&sub["tree_parents"]), vec!["REQ-002"]);
    assert_eq!(sub["tree_kind_decl"].as_str(), Some("group"));
    assert_eq!(sub["def_hash"].as_str(), Some(def_hash.as_str()));
    assert_eq!(sub["body_hash"].as_str(), Some(body_hash.as_str()));

    // Removing the lines clears the fields (re-derived from the body).
    let cleared = s.call("handoff_doc_save", json!({"doc_id": id, "body": body("")}));
    assert!(cleared.get("error").is_none(), "{cleared}");
    let sub = s.sub_item("req-tree", "REQ-010");
    assert!(sub.get("tree_parents").is_none() && sub.get("tree_kind_decl").is_none());
    assert_eq!(sub["def_hash"].as_str(), Some(def_hash.as_str()));

    // Invalid order (negative / text) and invalid kind: saved, unspecified, warned.
    let invalid = s.call(
        "handoff_doc_save",
        json!({"doc_id": id, "body": body("- order: -1\n- order@REQ-001: abc\n- kind: foo\n")}),
    );
    assert!(invalid.get("error").is_none(), "{invalid}");
    let warnings = warnings_of(&invalid);
    assert!(
        warnings
            .iter()
            .filter(|w| w.contains("invalid order"))
            .count()
            >= 2,
        "{warnings:?}"
    );
    assert!(
        warnings.iter().any(|w| w.contains("invalid kind")),
        "{warnings:?}"
    );
    let sub = s.sub_item("req-tree", "REQ-010");
    assert!(sub.get("tree_order").is_none());
    assert!(sub.get("tree_order_per_parent").is_none());
    assert!(sub.get("tree_kind_decl").is_none());
}

// ---------------------------------------------------------------------------
// ST-TREE-002
// ---------------------------------------------------------------------------

fn upsert(doc: &str, id: &str, attrs: Value) -> Value {
    json!({"op": "upsert_item", "doc": doc, "id": id, "title": id, "statement": "S.", "attrs": attrs})
}

fn error_codes(failed: &Value) -> Vec<String> {
    failed["errors"]
        .as_array()
        .unwrap_or_else(|| panic!("no structured errors in {failed}"))
        .iter()
        .map(|e| e["code"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn st_tree_002_create_time_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    assert!(s
        .save_layer(
            "req-doc",
            "requirement",
            "# Req\n\n### REQ-001 One\n\nA.\n\n### REQ-002 Two\n\n- parent: REQ-001\n\nB.\n"
        )
        .get("error")
        .is_none());
    assert!(s
        .save_layer(
            "spec-doc",
            "basic_spec",
            "# Spec\n\n### SPEC-001 Spec\n\nS.\n"
        )
        .get("error")
        .is_none());
    let before = s.body_of("req-doc");

    // --- trace_update path: one error kind per call, nothing written. ---
    let cases: Vec<(&str, Value, &str)> = vec![
        (
            "dotted_id",
            upsert("req-doc", "REQ-9.1", json!({})),
            "dotted_id",
        ),
        (
            "duplicate_id",
            upsert("spec-doc", "REQ-001", json!({})),
            "duplicate_id",
        ),
        (
            "self_parent",
            upsert("req-doc", "REQ-003", json!({"parent": "REQ-001, REQ-003"})),
            "self_parent",
        ),
        (
            "parent_dangling",
            upsert("req-doc", "REQ-003", json!({"parent": "REQ-001, REQ-404"})),
            "parent_dangling",
        ),
        (
            "parent_cross_layer",
            upsert("req-doc", "REQ-003", json!({"parent": "REQ-001, SPEC-001"})),
            "parent_cross_layer",
        ),
        (
            "tree_cycle",
            upsert("req-doc", "REQ-001", json!({"parent": "REQ-002"})),
            "tree_cycle",
        ),
    ];
    for (label, op, code) in cases {
        let out = s.call("handoff_trace_update", json!({"ops": [op]}));
        assert_eq!(out["failed"]["op_index"], 0, "{label}: {out}");
        assert_eq!(
            error_codes(&out["failed"]),
            vec![code.to_string()],
            "{label}: {out}"
        );
        assert!(
            out["failed"]["error"].as_str().unwrap().contains(code),
            "{label}"
        );
        assert_eq!(
            s.body_of("req-doc"),
            before,
            "{label}: a rejected call must write nothing"
        );
    }
    // The cycle error reports the closed path.
    let out = s.call(
        "handoff_trace_update",
        json!({"ops": [upsert("req-doc", "REQ-001", json!({"parent": "REQ-002"}))]}),
    );
    let path = out["failed"]["errors"][0]["path"].as_array().unwrap();
    assert_eq!(path.first(), path.last());
    assert!(path.len() >= 3);

    // A cycle that exists only through the combination of two ops of one call.
    let out = s.call(
        "handoff_trace_update",
        json!({"ops": [
            upsert("req-doc", "REQ-003", json!({})),
            upsert("req-doc", "REQ-004", json!({"parent": "REQ-003"})),
            upsert("req-doc", "REQ-003", json!({"parent": "REQ-004"})),
        ]}),
    );
    assert_eq!(out["failed"]["op_index"], 2, "{out}");
    assert_eq!(error_codes(&out["failed"]), vec!["tree_cycle".to_string()]);
    assert_eq!(
        s.body_of("req-doc"),
        before,
        "atomic: ops 0 and 1 must not be written"
    );

    // A valid multi-parent (DAG) declaration is accepted.
    let ok = s.call(
        "handoff_trace_update",
        json!({"ops": [upsert("req-doc", "REQ-003", json!({"parent": "REQ-001, REQ-002"}))]}),
    );
    assert!(ok.get("failed").is_none(), "{ok}");
    assert_eq!(
        yaml_strings(&s.sub_item("req-doc", "REQ-003")["tree_parents"]),
        vec!["REQ-001", "REQ-002"]
    );

    // --- doc_save / doc_update_section path. ---
    let doc_id = {
        let content = std::fs::read_to_string(s.doc_path("req-doc")).unwrap();
        content
            .lines()
            .find_map(|l| l.strip_prefix("id: "))
            .unwrap()
            .to_string()
    };
    let current = s.body_of("req-doc");
    let save_cases = [
        ("self_parent", "### REQ-005 Five\n\n- parent: REQ-005\n\nF.\n"),
        ("parent_dangling", "### REQ-005 Five\n\n- parent: REQ-404\n\nF.\n"),
        ("parent_cross_layer", "### REQ-005 Five\n\n- parent: SPEC-001\n\nF.\n"),
        (
            "tree_cycle",
            "### REQ-005 Five\n\n- parent: REQ-006\n\nF.\n\n### REQ-006 Six\n\n- parent: REQ-005\n\nS.\n",
        ),
    ];
    for (code, items) in save_cases {
        let out = s.call(
            "handoff_doc_save",
            json!({"doc_id": doc_id, "body": format!("{current}\n{items}")}),
        );
        let message = out["error"]["message"]
            .as_str()
            .unwrap_or_else(|| panic!("{code}: {out}"));
        assert!(message.contains(code), "{code}: {message}");
        assert!(message.contains("errors: ["), "{code}: {message}");
        assert_eq!(
            s.body_of("req-doc"),
            current,
            "{code}: doc_save must write nothing"
        );
    }
    // doc_save accepts a dotted heading with a warning (trace_update rejects it).
    let warned = s.call(
        "handoff_doc_save",
        json!({"doc_id": doc_id, "body": format!("{current}\n### REQ-7.2 Dotted\n\nD.\n")}),
    );
    assert!(warned.get("error").is_none(), "{warned}");
    assert!(
        warnings_of(&warned).iter().any(|w| w.contains("dotted_id")),
        "{warned}"
    );
    // doc_update_section rejects a bad parent and keeps the body.
    let body_now = s.body_of("req-doc");
    let sections = s.call("handoff_doc_get", json!({"doc_id": doc_id}));
    let seq = sections["sections"]
        .as_array()
        .and_then(|a| a.last())
        .and_then(|sec| sec["seq"].as_u64())
        .unwrap_or_else(|| panic!("no sections in {sections}"));
    let rejected = s.call(
        "handoff_doc_update_section",
        json!({"doc_id": doc_id, "seq": seq,
               "new_content": "### REQ-008 Eight\n\n- parent: REQ-008\n\nE.\n"}),
    );
    let message = rejected["error"]["message"]
        .as_str()
        .unwrap_or_else(|| panic!("{rejected}"));
    assert!(message.contains("self_parent"), "{message}");
    assert_eq!(s.body_of("req-doc"), body_now);
}

// ---------------------------------------------------------------------------
// ST-TREE-003..006 — engine behavior over documents written by the real
// binary. The documents are saved through `handoff_doc_save` /
// `handoff_trace_update` (so parsing, validation and persistence are the
// production path), then read back with the library's own loader and run
// through `TraceGraph::build`. Run results and task links, which have no
// bearing on the tree, are supplied directly.
// ---------------------------------------------------------------------------

use std::collections::HashMap;

use handoff_mcp::storage::docs::read_all_docs;
use handoff_mcp::trace::adapter::collect_trace_items;
use handoff_mcp::trace::tree::{TreeGapKind, TreeKind};
use handoff_mcp::trace::types::{
    CoverageStatus, GapKind, ItemState, TaskLinkRole, TaskRequirementLink, TraceInput,
};
use handoff_mcp::trace::TraceGraph;

fn graph_of(project: &Path, runs: &[(&str, &str)], implements: &[(&str, &str)]) -> TraceGraph {
    let docs = read_all_docs(&project.join(".handoff")).expect("read docs");
    let input = TraceInput {
        items: collect_trace_items(&docs),
        task_requirement_links: implements
            .iter()
            .map(|(task, item)| TaskRequirementLink {
                task_id: task.to_string(),
                stable_id: item.to_string(),
                role: TaskLinkRole::Implements,
                baseline_hash: None,
            })
            .collect(),
        runs_latest: runs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        ..Default::default()
    };
    TraceGraph::build(&input)
}

fn ids(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

#[test]
fn st_tree_003_tree_index_build_and_order() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    // Two documents (sibling order across documents), a multi-parent item
    // with a per-parent order, and a refines/verifies chain alongside.
    for (slug, layer, body) in [
        (
            "req-a",
            "requirement",
            "# A\n\n### REQ-001 Arch\n\n- order: 2\n\nA.\n\n### REQ-002 Scope\n\n- order: 1\n\nS.\n\n\
             ### REQ-010 Child A\n\n- parent: REQ-001\n\nC.\n",
        ),
        ("req-c", "requirement", "# C\n\n### REQ-014 Soon deleted\n\nD.\n"),
        (
            "req-b",
            "requirement",
            "# B\n\n### REQ-011 Shared\n\n- parent: REQ-001, REQ-002\n- order@REQ-002: 1\n\nS.\n\n\
             ### REQ-012 Ordered\n\n- parent: REQ-001\n- order: 1\n\nO.\n\n\
             ### REQ-013 Orphaned by a deleted parent\n\n- parent: REQ-014\n\nO.\n",
        ),
        (
            "spec-a",
            "basic_spec",
            "# S\n\n### SPEC-001 Spec\n\n- refines: REQ-010\n\nS.\n",
        ),
        (
            "at-a",
            "acceptance",
            "# AT\n\n### AT-001 Acceptance\n\n- verifies: REQ-010\n- method: manual\n\nT.\n",
        ),
    ] {
        let saved = s.save_layer(slug, layer, body);
        assert!(saved.get("error").is_none(), "{slug}: {saved}");
    }
    // Deleting the parent leaves REQ-013's declaration dangling.
    let deleted = s.call("handoff_doc_delete", json!({"doc_id": "req-c"}));
    assert!(deleted.get("error").is_none(), "{deleted}");

    let graph = graph_of(tmp.path(), &[], &[]);
    let tree = graph.tree();
    // order first (ascending), then document path, then position in the document.
    assert_eq!(
        ids(tree.children("REQ-001")),
        vec!["REQ-012", "REQ-010", "REQ-011"]
    );
    // The per-parent order is used under REQ-002 only.
    assert_eq!(ids(tree.children("REQ-002")), vec!["REQ-011"]);
    assert_eq!(ids(tree.parents("REQ-011")), vec!["REQ-001", "REQ-002"]);
    // Roots: ordered ones first; the dangling-parent item is a root.
    assert_eq!(
        ids(&tree.roots_by_layer["requirement"]),
        vec!["REQ-002", "REQ-001", "REQ-013"]
    );
    assert_eq!(ids(&tree.roots_by_layer["basic_spec"]), vec!["SPEC-001"]);
    assert_eq!(tree.depth("REQ-010"), Some(1));
    assert_eq!(tree.dag_shared_count(), 1);
    let gaps: Vec<_> = graph
        .tree_gaps()
        .iter()
        .map(|g| (g.kind, g.item.as_str()))
        .collect();
    assert_eq!(gaps, vec![(TreeGapKind::ParentDangling, "REQ-013")]);

    // refines / verifies evaluation is exactly what it was before the tree
    // existed: the same items with their tree data removed evaluate equally.
    let docs = read_all_docs(&tmp.path().join(".handoff")).unwrap();
    let with_tree = collect_trace_items(&docs);
    let mut without_tree = with_tree.clone();
    for it in &mut without_tree {
        it.tree.parents.clear();
        it.tree.kind_decl = None;
    }
    let build = |items| {
        TraceGraph::build(&TraceInput {
            items,
            runs_latest: HashMap::from([("AT-001".to_string(), "pass".to_string())]),
            ..Default::default()
        })
    };
    let (a, b) = (build(with_tree), build(without_tree));
    for id in [
        "REQ-001", "REQ-002", "REQ-010", "REQ-011", "REQ-012", "REQ-013", "SPEC-001", "AT-001",
    ] {
        assert_eq!(
            a.refines_parents(id),
            b.refines_parents(id),
            "{id} refines up"
        );
        assert_eq!(
            a.refines_children(id),
            b.refines_children(id),
            "{id} refines down"
        );
        assert_eq!(
            a.verifies_targets(id),
            b.verifies_targets(id),
            "{id} verifies"
        );
        assert_eq!(a.verified_by(id), b.verified_by(id), "{id} verified_by");
    }
    // Leaves (and the refines-linked chain) keep their coverage and state.
    for id in [
        "REQ-010", "REQ-011", "REQ-012", "REQ-013", "SPEC-001", "AT-001",
    ] {
        assert_eq!(a.state(id), b.state(id), "{id} state");
        assert_eq!(
            a.item_horizontal(id),
            b.item_horizontal(id),
            "{id} horizontal"
        );
        assert_eq!(a.item_vertical(id), b.item_vertical(id), "{id} vertical");
    }
}

#[test]
fn st_tree_004_kind_resolution() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    let body = "# K\n\n\
        ### REQ-001 Plain parent\n\nP.\n\n\
        ### REQ-002 Plain child\n\n- parent: REQ-001\n\nC.\n\n\
        ### REQ-003 Plain leaf\n\nL.\n\n\
        ### REQ-004 Empty group\n\n- kind: group\n\nG.\n\n\
        ### REQ-005 Leaf with a child\n\n- kind: leaf\n\nL.\n\n\
        ### REQ-006 The child\n\n- parent: REQ-005\n\nC.\n\n\
        ### REQ-007 Childless decision\n\n- kind: decision\n\nD.\n\n\
        ### REQ-008 Decision with branches\n\n- kind: decision\n\nD.\n\n\
        ### REQ-009 Branch yes\n\n- parent: REQ-008\n- condition: yes\n\nY.\n\n\
        ### REQ-010 Branch no\n\n- parent: REQ-008\n- condition: no\n\nN.\n";
    assert!(s
        .save_layer("req-k", "requirement", body)
        .get("error")
        .is_none());
    let graph = graph_of(tmp.path(), &[], &[]);
    let kind = |id: &str| graph.tree_kind(id).unwrap_or_else(|| panic!("{id}"));
    assert_eq!(kind("REQ-001"), TreeKind::Composite);
    assert_eq!(kind("REQ-002"), TreeKind::Leaf);
    assert_eq!(kind("REQ-003"), TreeKind::Leaf);
    assert_eq!(kind("REQ-004"), TreeKind::Composite);
    assert_eq!(kind("REQ-005"), TreeKind::Composite);
    assert_eq!(kind("REQ-007"), TreeKind::Decision);
    assert_eq!(kind("REQ-008"), TreeKind::Decision);
    assert_eq!(
        ids(graph.tree().children("REQ-008")),
        vec!["REQ-009", "REQ-010"]
    );
    let gaps: Vec<_> = graph
        .tree_gaps()
        .iter()
        .map(|g| (g.kind, g.item.as_str()))
        .collect();
    assert_eq!(
        gaps,
        vec![
            (TreeGapKind::EmptyGroup, "REQ-004"),
            (TreeGapKind::KindConflict, "REQ-005"),
            (TreeGapKind::EmptyBranch, "REQ-007"),
        ],
        "a childless decision gets empty_branch, never empty_group"
    );
}

#[test]
fn st_tree_005_composite_semantics() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    for (slug, layer, body) in [
        (
            "req-c",
            "requirement",
            "# C\n\n\
             ### REQ-001 Composite\n\nTop.\n\n\
             ### REQ-002 Verified leaf\n\n- parent: REQ-001\n\nL.\n\n\
             ### REQ-003 Unverified leaf\n\n- parent: REQ-001\n\nL.\n\n\
             ### REQ-004 Integrated composite\n\n- parent: REQ-001\n\nI.\n\n\
             受入基準:\n- AC1: Given the whole When it is used Then it works\n\n\
             ### REQ-005 Child of the integrated one\n\n- parent: REQ-004\n\nL.\n\n\
             ### REQ-006 Waived composite\n\n- waive-verify: reviewed by hand\n\nW.\n\n\
             ### REQ-007 Its leaf\n\n- parent: REQ-006\n\nL.\n",
        ),
        (
            "at-c",
            "acceptance",
            "# AT\n\n\
             ### AT-001 Verifies the verified leaf\n\n- verifies: REQ-002\n- method: manual\n\nT.\n\n\
             ### AT-002 Verifies the unverified leaf\n\n- verifies: REQ-003\n- method: manual\n\nT.\n\n\
             ### AT-003 Verifies a child\n\n- verifies: REQ-005\n- method: manual\n\nT.\n\n\
             ### AT-004 Verifies another leaf\n\n- verifies: REQ-007\n- method: manual\n\nT.\n",
        ),
    ] {
        assert!(s.save_layer(slug, layer, body).get("error").is_none(), "{slug}");
    }

    // AT-002 fails, the rest pass; every requirement leaf is implemented, so
    // each leaf's own state is its verifier's.
    let implements = [
        ("t1", "REQ-002"),
        ("t1", "REQ-003"),
        ("t1", "REQ-005"),
        ("t1", "REQ-007"),
    ];
    let graph = graph_of(
        tmp.path(),
        &[
            ("AT-001", "pass"),
            ("AT-002", "fail"),
            ("AT-003", "pass"),
            ("AT-004", "pass"),
        ],
        &implements,
    );

    // horizontal: aggregated from the leaves for a composite without its own
    // acceptance criteria / verifier.
    let h = |id: &str| graph.item_horizontal(id);
    assert_eq!(
        h("REQ-001"),
        Some(CoverageStatus::Covered),
        "all leaves covered"
    );
    // The integrated composite keeps its own judgment: it has an acceptance
    // block but no verifier of its own.
    assert_eq!(h("REQ-004"), Some(CoverageStatus::Uncovered));
    // state: the worst of the leaves below.
    assert_eq!(graph.state("REQ-001"), Some(ItemState::Failing));
    assert_eq!(graph.state("REQ-006"), Some(ItemState::Passing));
    assert_eq!(graph.state("REQ-002"), Some(ItemState::Passing));

    let has_gap = |kind: GapKind, id: &str| {
        graph
            .gaps()
            .iter()
            .any(|g| g.kind == kind && g.item.as_deref() == Some(id))
    };
    // A composite without integrated acceptance criteria still gets the
    // unverified gap ...
    assert!(has_gap(GapKind::Unverified, "REQ-001"));
    // ... unless it waives verification; the leaf-level rules are unchanged.
    assert!(!has_gap(GapKind::Unverified, "REQ-006"));
    assert!(!has_gap(GapKind::Unverified, "REQ-002"));
    // The integrated composite is asked for its own verification.
    assert!(has_gap(GapKind::Unverified, "REQ-004"));
    // unrefined is never reported for a composite, and still is for a leaf
    // that has nothing implementing it.
    for composite in ["REQ-001", "REQ-004", "REQ-006"] {
        assert!(!has_gap(GapKind::Unrefined, composite), "{composite}");
    }
    let no_task = graph_of(tmp.path(), &[], &[]);
    assert!(no_task
        .gaps()
        .iter()
        .any(|g| g.kind == GapKind::Unrefined && g.item.as_deref() == Some("REQ-002")));

    // The same through the production report tool.
    let report = s.call("handoff_trace_report", json!({"include_items": true}));
    let item = |id: &str| {
        report["items"]
            .as_array()
            .unwrap_or_else(|| panic!("{report}"))
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("{id} not in report"))
            .clone()
    };
    assert_eq!(item("REQ-001")["coverage"]["horizontal"], "covered");
    assert_eq!(item("REQ-004")["coverage"]["horizontal"], "uncovered");
    let reported = |kind: &str, id: &str| {
        report["gaps"]
            .as_array()
            .unwrap()
            .iter()
            .any(|g| g["kind"] == kind && g["item"] == id)
    };
    assert!(reported("unverified", "REQ-001"));
    assert!(!reported("unrefined", "REQ-001"));
}

#[test]
fn st_tree_006_subtree_rollup() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    let body = "# R\n\n\
        ### REQ-001 Root\n\nR.\n\n\
        ### REQ-002 Left\n\n- parent: REQ-001\n\nL.\n\n\
        ### REQ-003 Right\n\n- parent: REQ-001\n\nR.\n\n\
        ### REQ-004 Shared leaf\n\n- parent: REQ-002, REQ-003\n\nS.\n\n\
        ### REQ-005 Left leaf\n\n- parent: REQ-002\n- waive-verify: not testable\n\nS.\n\n\
        ### REQ-006 Right leaf\n\n- parent: REQ-003\n\nS.\n\n\
        ### REQ-007 Empty group\n\n- kind: group\n\nG.\n";
    let at = "# AT\n\n\
        ### AT-001 Shared\n\n- verifies: REQ-004\n- method: manual\n\nT.\n\n\
        ### AT-002 Right\n\n- verifies: REQ-006\n- method: manual\n\nT.\n";
    assert!(s
        .save_layer("req-r", "requirement", body)
        .get("error")
        .is_none());
    assert!(s
        .save_layer("at-r", "acceptance", at)
        .get("error")
        .is_none());
    for (item, stage) in [("REQ-004", "tested"), ("REQ-006", "implemented")] {
        let out = s.call(
            "handoff_trace_update",
            json!({"ops": [{"op": "set", "item": item, "dev_stage": stage}]}),
        );
        assert!(out.get("failed").is_none(), "{out}");
    }

    let graph = graph_of(
        tmp.path(),
        &[("AT-001", "pass"), ("AT-002", "fail")],
        &[("t1", "REQ-004"), ("t2", "REQ-006"), ("t2", "REQ-003")],
    );
    // Each parent of the shared leaf sees it; the root counts it once.
    let left = graph.subtree("REQ-002").unwrap();
    let right = graph.subtree("REQ-003").unwrap();
    let root = graph.subtree("REQ-001").unwrap();
    assert_eq!((left.leaves, right.leaves, root.leaves), (2, 2, 3));
    assert_eq!(
        root.state.passing, 1,
        "the shared leaf is counted once under the root"
    );
    assert_eq!(root.state.failing, 1);
    // The waived leaf is excluded from states and ratios but counted as a leaf.
    assert_eq!(left.excluded.waived, 1);
    assert_eq!(left.state.passing, 1);
    assert_eq!(left.progress.verified_ratio, Some(1.0));
    assert_eq!(root.excluded.waived, 1);
    assert_eq!(root.progress.verified_ratio, Some(0.5));
    // dev_stage: unset counts as not_started; the waived leaf is still counted.
    assert_eq!(root.dev_stage.tested, 1);
    assert_eq!(root.dev_stage.implemented, 1);
    assert_eq!(root.dev_stage.not_started, 1);
    assert_eq!(root.progress.implemented_ratio, Some(1.0));
    // tasks: direct vs unique below.
    assert_eq!(
        graph.subtree("REQ-003").unwrap().tasks,
        vec!["t2".to_string()]
    );
    assert_eq!(root.all_tasks, 2);
    assert!(root.tasks.is_empty());
    // An empty group has no leaves and no ratios.
    let empty = graph.subtree("REQ-007").unwrap();
    assert_eq!(empty.leaves, 0);
    assert_eq!(empty.progress.verified_ratio, None);
    assert_eq!(empty.progress.implemented_ratio, None);
    // Project totals are de-duplicated.
    let totals = graph.tree_totals();
    assert_eq!(totals.dag_shared, 1);
    assert_eq!(totals.max_depth, 2);
    // Both layers' trees are in the totals: 7 requirement + 2 acceptance items.
    assert_eq!(totals.nodes, 9);
    // Serialization rounds to four decimals.
    let json = serde_json::to_value(root).unwrap();
    assert_eq!(json["progress"]["verified_ratio"], 0.5);

    // Depth 200 chain: completes (no recursion) with the expected numbers.
    use handoff_mcp::trace::types::{TraceItemInput, TreeItemInput};
    let item = |n: usize| TraceItemInput {
        stable_id: format!("REQ-{n:04}"),
        doc_id: "d".into(),
        layer: Some("requirement".into()),
        refines: vec![],
        verifies: vec![],
        method: Some("manual".into()),
        has_test_refs: false,
        acceptance_labels: vec![],
        derived: false,
        waived_axes: vec![],
        def_hash: None,
        body_hash: None,
        link_baselines: Default::default(),
        needs: None,
        approval: "draft".into(),
        tree: TreeItemInput {
            parents: if n == 0 {
                vec![]
            } else {
                vec![format!("REQ-{:04}", n - 1)]
            },
            doc_path: "d".into(),
            seq: n,
            ..Default::default()
        },
    };
    let chain = TraceGraph::build(&TraceInput {
        items: (0..200).map(item).collect(),
        runs_latest: HashMap::from([("REQ-0199".to_string(), "pass".to_string())]),
        ..Default::default()
    });
    let top = chain.subtree("REQ-0000").unwrap();
    assert_eq!(top.leaves, 1);
    assert_eq!(chain.subtree("REQ-0199").unwrap().depth, 199);
    assert_eq!(chain.tree_totals().max_depth, 199);
}

// ---------------------------------------------------------------------------
// ST-TREE-010 — lint rules, through `handoff_trace_lint` on the real binary
// ---------------------------------------------------------------------------

impl Server {
    /// Replaces a document's body on disk, keeping its frontmatter — a
    /// "direct edit", the only way to put a tree that create-time validation
    /// would refuse (dangling / cyclic parents) in front of `trace_lint`.
    fn overwrite_body(&self, slug: &str, body: &str) {
        let content = std::fs::read_to_string(self.doc_path(slug)).expect("doc file");
        let mut parts = content.splitn(3, "---\n");
        parts.next();
        let frontmatter = parts.next().expect("frontmatter");
        std::fs::write(
            self.doc_path(slug),
            format!("---\n{frontmatter}---\n{body}"),
        )
        .expect("rewrite doc");
    }
}

/// `item` ids of every finding of `rule`, in output order.
fn lint_items(lint: &Value, rule: &str) -> Vec<String> {
    lint["findings"]
        .as_array()
        .unwrap_or_else(|| panic!("no findings in {lint}"))
        .iter()
        .filter(|f| f["rule"] == rule)
        .map(|f| f["item"].as_str().unwrap_or_default().to_string())
        .collect()
}

fn lint_severities(lint: &Value, rule: &str) -> Vec<String> {
    lint["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == rule)
        .map(|f| f["severity"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn st_tree_010_lint_rules() {
    const AC: &str = "\n受入基準:\n- 期待どおり動く\n";
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    assert!(s
        .save_layer("req-lint", "requirement", "# R\n\n### REQ-001 Root\n\nR.\n")
        .get("error")
        .is_none());
    assert!(s
        .save_layer(
            "spec-lint",
            "basic_spec",
            "# S\n\n### SPEC-001 Spec\n\nS.\n"
        )
        .get("error")
        .is_none());

    let item = |heading: &str, attrs: &str, text: &str, ac: bool| {
        format!(
            "### {heading}\n\n{attrs}\n{text}\n{}\n",
            if ac { AC } else { "" }
        )
    };
    let req_body = [
        "# R\n".to_string(),
        item("REQ-001 Root", "- kind: group\n", "Root.", true),
        item("REQ-001-001 Child", "- parent: REQ-001\n", "Child.", true),
        item("REQ-001-002 Orphan-named", "", "Named like a child.", true),
        item("REQ-002 Dangling", "- parent: REQ-404\n", "D.", true),
        item("REQ-003 Self", "- parent: REQ-003\n", "S.", true),
        item("REQ-004 Cycle A", "- parent: REQ-005\n", "A.", true),
        item("REQ-005 Cycle B", "- parent: REQ-004\n", "B.", true),
        item("REQ-006 Empty group", "- kind: group\n", "G.", true),
        item("REQ-007 Leaf with child", "- kind: leaf\n", "L.", true),
        item("REQ-008 The child", "- parent: REQ-007\n", "C.", true),
        item("REQ-009 Bad order", "- order: abc\n", "O.", true),
        item("REQ-010.1 Dotted", "", "Dotted.", true),
        item("REQ-011. Boundary", "", "Not dotted.", true),
        item("REQ-012 Bare leaf", "", "Does one thing.", false),
        item("REQ-013 Shallow", "", "Logs in and saves history.", true),
        item("REQ-014 Composite with and", "", "Does A and B.", true),
        item("REQ-014-001 Part", "- parent: REQ-014\n", "Part.", true),
        item("REQ-015 Composite without acceptance", "", "Bundle.", false),
        item("REQ-015-001 Part", "- parent: REQ-015\n", "Part.", true),
    ]
    .join("\n");
    s.overwrite_body("req-lint", &req_body);
    let spec_body = format!(
        "# S\n\n{}",
        item(
            "SPEC-001 Spec",
            "- parent: REQ-001\n",
            "Cross-layer parent.",
            true
        )
    );
    s.overwrite_body("spec-lint", &spec_body);

    let lint = s.call("handoff_trace_lint", json!({}));
    assert!(lint.get("error").is_none(), "{lint}");

    let expect: [(&str, &[&str], &str); 11] = [
        ("parent_cross_layer", &["SPEC-001"], "error"),
        ("parent_dangling", &["REQ-002"], "error"),
        ("self_parent", &["REQ-003"], "error"),
        ("tree_cycle", &["REQ-004", "REQ-005"], "error"),
        ("empty_group", &["REQ-006"], "warning"),
        ("kind_conflict", &["REQ-007"], "warning"),
        ("invalid_order", &["REQ-009"], "warning"),
        ("dotted_id", &["REQ-010"], "warning"),
        ("leaf_without_acceptance", &["REQ-012"], "warning"),
        ("shallow_leaf", &["REQ-013"], "info"),
        ("id_parent_mismatch", &["REQ-001-002"], "info"),
    ];
    for (rule, items, severity) in expect {
        assert_eq!(lint_items(&lint, rule), items, "{rule}: {lint}");
        assert!(
            lint_severities(&lint, rule).iter().all(|v| v == severity),
            "{rule} severity: {lint}"
        );
    }
    // Errors present -> exit code 1.
    assert_eq!(lint["exit_code"], 1, "{lint}");

    // The `rules` filter restricts evaluation; a warning-only rule passes the
    // default `fail_on=error` gate and fails the `warning` one.
    let only = s.call("handoff_trace_lint", json!({"rules": ["empty_group"]}));
    assert_eq!(lint_items(&only, "empty_group"), ["REQ-006"]);
    assert_eq!(only["findings"].as_array().unwrap().len(), 1, "{only}");
    assert_eq!(only["exit_code"], 0);
    let strict = s.call(
        "handoff_trace_lint",
        json!({"rules": ["empty_group"], "fail_on": "warning"}),
    );
    assert_eq!(strict["exit_code"], 1);

    // multiple_parent does not exist: several parents make a DAG.
    let unknown = s.call("handoff_trace_lint", json!({"rules": ["multiple_parent"]}));
    assert!(
        unknown["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("unknown rule id")),
        "{unknown}"
    );

    // `[trace.lint.rules]` overrides: raise shallow_leaf, silence dotted_id,
    // downgrade parent_dangling.
    let config_path = tmp.path().join(".handoff/config.toml");
    let mut config = std::fs::read_to_string(&config_path).unwrap_or_default();
    config.push_str(
        "\n[trace.lint.rules]\nshallow_leaf = \"warning\"\ndotted_id = \"off\"\n\
         parent_dangling = \"info\"\n",
    );
    std::fs::write(&config_path, config).unwrap();
    let overridden = s.call("handoff_trace_lint", json!({}));
    assert!(overridden.get("error").is_none(), "{overridden}");
    assert_eq!(lint_severities(&overridden, "shallow_leaf"), ["warning"]);
    assert!(lint_items(&overridden, "dotted_id").is_empty());
    assert_eq!(lint_severities(&overridden, "parent_dangling"), ["info"]);
}

#[test]
fn st_tree_010_lint_is_quiet_on_a_valid_tree_and_allows_several_parents() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    let ac = "\n受入基準:\n- 期待どおり動く\n";
    let body = format!(
        "# R\n\n### REQ-001 A\n\nA.\n{ac}\n### REQ-002 B\n\nB.\n{ac}\n\
         ### REQ-001-001 Shared\n\n- parent: REQ-001, REQ-002\n\nShared.\n{ac}\n"
    );
    let saved = s.save_layer("req-dag", "requirement", &body);
    assert!(saved.get("error").is_none(), "{saved}");

    let lint = s.call("handoff_trace_lint", json!({}));
    for rule in [
        "parent_cross_layer",
        "parent_dangling",
        "tree_cycle",
        "self_parent",
        "empty_group",
        "kind_conflict",
        "invalid_order",
        "dotted_id",
        "leaf_without_acceptance",
        "shallow_leaf",
        "id_parent_mismatch",
    ] {
        assert!(
            lint_items(&lint, rule).is_empty(),
            "{rule} must not fire on a valid DAG: {lint}"
        );
    }
}

// ---------------------------------------------------------------------------
// ST-TREE-008 — handoff_trace_tree, through the real binary
// ---------------------------------------------------------------------------

/// The ST-TREE-006 fixture: a diamond (REQ-004 under REQ-002 and REQ-003), a
/// waived leaf, an empty group, and two acceptance items (one passing, one
/// failing).
fn save_tree_fixture(s: &mut Server) {
    let body = "# R\n\n\
        ### REQ-001 Root\n\n- priority: P0\n\nR.\n\n\
        ### REQ-002 Left\n\n- parent: REQ-001\n- priority: P1\n\nL.\n\n\
        ### REQ-003 Right\n\n- parent: REQ-001\n\nR.\n\n\
        ### REQ-004 Shared leaf\n\n- parent: REQ-002, REQ-003\n- priority: P1\n\nS.\n\n\
        ### REQ-005 Left leaf\n\n- parent: REQ-002\n- waive-verify: not testable\n\nS.\n\n\
        ### REQ-006 Right leaf\n\n- parent: REQ-003\n- priority: P2\n\nS.\n\n\
        ### REQ-007 Empty group\n\n- kind: group\n\nG.\n";
    let at = "# AT\n\n\
        ### AT-001 Shared\n\n- verifies: REQ-004\n- method: manual\n\nT.\n\n\
        ### AT-002 Right\n\n- verifies: REQ-006\n- method: manual\n\nT.\n";
    assert!(s
        .save_layer("req-r", "requirement", body)
        .get("error")
        .is_none());
    assert!(s
        .save_layer("at-r", "acceptance", at)
        .get("error")
        .is_none());
    let rec = s.call(
        "handoff_trace_record",
        json!({"results": [
            {"item": "AT-001", "result": "pass"},
            {"item": "AT-002", "result": "fail"},
        ]}),
    );
    assert!(rec.get("error").is_none(), "{rec}");
    // A state needs an implementing task plus the implemented dev stage.
    for (task, ids) in [
        ("t1", json!(["REQ-004"])),
        ("t2", json!(["REQ-006", "REQ-003"])),
    ] {
        let out = s.call(
            "handoff_update_task",
            json!({"task": {"id": task, "title": task, "requirement_ids": ids,
                "status": "in_progress", "schedule": {"estimate_hours": 1.0}}}),
        );
        assert!(out.get("error").is_none(), "{out}");
    }
    for (item, stage) in [("REQ-004", "tested"), ("REQ-006", "implemented")] {
        let out = s.call(
            "handoff_trace_update",
            json!({"ops": [{"op": "set", "item": item, "dev_stage": stage}]}),
        );
        assert!(out.get("failed").is_none(), "{out}");
    }
}

fn node_ids(nodes: &Value) -> Vec<String> {
    nodes
        .as_array()
        .unwrap_or_else(|| panic!("expected an array, got {nodes}"))
        .iter()
        .map(|n| n["id"].as_str().unwrap().to_string())
        .collect()
}

fn find_node<'a>(nodes: &'a Value, id: &str) -> Option<&'a Value> {
    for node in nodes.as_array()? {
        if node["id"] == id {
            return Some(node);
        }
        if let Some(found) = find_node(&node["children"], id) {
            return Some(found);
        }
    }
    None
}

fn error_message(out: &Value) -> String {
    out["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

#[test]
fn st_tree_008_full_tree_shape_and_totals() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_tree_fixture(&mut s);

    let out = s.call("handoff_trace_tree", json!({}));
    assert!(out.get("error").is_none(), "{out}");
    // Roots of every layer; the requirement layer's in the stable order.
    let roots = node_ids(&out["roots"]);
    for expected in ["REQ-001", "REQ-007", "AT-001", "AT-002"] {
        assert!(roots.contains(&expected.to_string()), "{roots:?}");
    }
    assert_eq!(roots.len(), 4, "{roots:?}");

    let root = find_node(&out["roots"], "REQ-001").unwrap();
    assert_eq!(root["title"], "Root");
    assert_eq!(root["kind"], "composite");
    assert_eq!(root["layer"], "requirement");
    assert_eq!(root["priority"], "P0");
    assert_eq!(root["has_children"], true);
    assert_eq!(root["parents"], json!([]));
    assert_eq!(node_ids(&root["children"]), ["REQ-002", "REQ-003"]);
    assert!(root["doc"].is_string());
    assert!(root["seq"].is_number());
    // own facts and the roll-up.
    assert_eq!(root["own"]["approval"], "draft");
    assert_eq!(root["own"]["suspect"], json!([]));
    assert_eq!(root["own"]["tasks"], json!([]));
    assert_eq!(root["own"]["acceptance_criteria"], 0);
    assert_eq!(root["subtree"]["leaves"], 3);
    assert_eq!(root["subtree"]["state"]["passing"], 1);
    assert_eq!(root["subtree"]["state"]["failing"], 1);
    assert_eq!(root["subtree"]["excluded"]["waived"], 1);
    assert_eq!(root["subtree"]["progress"]["verified_ratio"], 0.5);

    // A leaf has `children: []`, a shared node appears under both parents.
    let leaf = find_node(&out["roots"], "REQ-006").unwrap();
    assert_eq!(leaf["kind"], "leaf");
    assert_eq!(leaf["has_children"], false);
    assert_eq!(leaf["children"], json!([]));
    assert_eq!(leaf["own"]["state"], "failing");
    let left = find_node(&out["roots"], "REQ-002").unwrap();
    let right = find_node(&out["roots"], "REQ-003").unwrap();
    assert_eq!(node_ids(&left["children"]), ["REQ-004", "REQ-005"]);
    assert_eq!(node_ids(&right["children"]), ["REQ-004", "REQ-006"]);
    let shared = &left["children"][0];
    assert_eq!(shared["parents"], json!(["REQ-002", "REQ-003"]));
    assert_eq!(shared["own"]["state"], "passing");

    // Totals: 7 requirement + 2 acceptance nodes, one DAG-shared node.
    let totals = &out["totals"];
    assert_eq!(totals["nodes"], 9);
    assert_eq!(totals["dag_shared"], 1);
    assert_eq!(totals["max_depth"], 2);
    assert_eq!(totals["composites"], 4);
    assert_eq!(totals["decisions"], 0);
    assert_eq!(totals["leaves"], 5);
    assert_eq!(totals["truncated_subtrees"], 0);
    assert_eq!(out["warnings"], json!([]));
}

#[test]
fn st_tree_008_depth_limits_expansion_but_keeps_has_children() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_tree_fixture(&mut s);

    let out = s.call("handoff_trace_tree", json!({"root": "REQ-001", "depth": 1}));
    assert!(out.get("error").is_none(), "{out}");
    assert_eq!(node_ids(&out["roots"]), ["REQ-001"]);
    let root = &out["roots"][0];
    assert_eq!(node_ids(&root["children"]), ["REQ-002", "REQ-003"]);
    for child in root["children"].as_array().unwrap() {
        assert_eq!(child["has_children"], true);
        assert!(child["children"].is_null(), "{child}");
    }
    assert_eq!(out["totals"]["truncated_subtrees"], 2);
    // The roll-up is unaffected by the display depth.
    assert_eq!(root["subtree"]["leaves"], 3);

    let flat = s.call("handoff_trace_tree", json!({"root": "REQ-001", "depth": 0}));
    assert!(flat["roots"][0]["children"].is_null());
    assert_eq!(flat["roots"][0]["has_children"], true);
    assert_eq!(flat["totals"]["nodes"], 1);
    assert_eq!(flat["totals"]["truncated_subtrees"], 1);
}

#[test]
fn st_tree_008_root_scope_and_layer_selection() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_tree_fixture(&mut s);

    // root: any node, even a non-root one.
    let sub = s.call("handoff_trace_tree", json!({"root": "REQ-003"}));
    assert_eq!(node_ids(&sub["roots"]), ["REQ-003"]);
    assert_eq!(sub["totals"]["nodes"], 3);
    // An unknown root, root+scope together, and an unknown layer are errors.
    let missing = s.call("handoff_trace_tree", json!({"root": "REQ-999"}));
    assert!(error_message(&missing).contains("REQ-999"), "{missing}");
    let both = s.call(
        "handoff_trace_tree",
        json!({"root": "REQ-001", "scope": "REQ"}),
    );
    assert!(error_message(&both).contains("exclusive"), "{both}");
    let bad_layer = s.call("handoff_trace_tree", json!({"layer": "nonsense"}));
    assert!(
        error_message(&bad_layer).contains("nonsense"),
        "{bad_layer}"
    );

    // scope: an ID prefix keeps matching nodes; roots are those without a
    // matching parent.
    let at = s.call("handoff_trace_tree", json!({"scope": "AT-"}));
    assert_eq!(node_ids(&at["roots"]), ["AT-001", "AT-002"]);
    let req = s.call("handoff_trace_tree", json!({"scope": "REQ-00"}));
    assert_eq!(node_ids(&req["roots"]), ["REQ-001", "REQ-007"]);
    let narrow = s.call("handoff_trace_tree", json!({"scope": "REQ-002"}));
    assert_eq!(node_ids(&narrow["roots"]), ["REQ-002"]);
    assert!(node_ids(&narrow["roots"][0]["children"]).is_empty());

    // layer: only that layer's roots.
    let layer = s.call("handoff_trace_tree", json!({"layer": "acceptance"}));
    assert_eq!(node_ids(&layer["roots"]), ["AT-001", "AT-002"]);
    let layer = s.call("handoff_trace_tree", json!({"layer": "requirement"}));
    assert_eq!(node_ids(&layer["roots"]), ["REQ-001", "REQ-007"]);
}

#[test]
fn st_tree_008_filters_keep_matches_and_their_ancestors_only() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_tree_fixture(&mut s);

    // state=failing: REQ-006 fails (via AT-002), so its ancestors stay; the
    // roll-up of a kept ancestor is the unfiltered one.
    let out = s.call(
        "handoff_trace_tree",
        json!({"layer": "requirement", "filter": {"state": ["failing"]}}),
    );
    assert!(out.get("error").is_none(), "{out}");
    assert_eq!(node_ids(&out["roots"]), ["REQ-001"]);
    let root = &out["roots"][0];
    assert_eq!(node_ids(&root["children"]), ["REQ-003"]);
    assert_eq!(node_ids(&root["children"][0]["children"]), ["REQ-006"]);
    assert_eq!(root["has_children"], true);
    assert_eq!(root["subtree"]["leaves"], 3);
    assert_eq!(out["totals"]["nodes"], 3);

    // priority.
    let out = s.call(
        "handoff_trace_tree",
        json!({"layer": "requirement", "filter": {"priority": ["P2"]}}),
    );
    assert_eq!(node_ids(&out["roots"]), ["REQ-001"]);
    assert!(find_node(&out["roots"], "REQ-006").is_some());
    assert!(find_node(&out["roots"], "REQ-004").is_none());

    // kind=decision matches nothing here.
    let out = s.call(
        "handoff_trace_tree",
        json!({"filter": {"kind": ["decision"]}}),
    );
    assert_eq!(out["roots"], json!([]));
    // kind=leaf keeps every leaf plus ancestors.
    let out = s.call(
        "handoff_trace_tree",
        json!({"layer": "requirement", "filter": {"kind": ["leaf"]}}),
    );
    assert!(find_node(&out["roots"], "REQ-005").is_some());
    assert!(find_node(&out["roots"], "REQ-007").is_none(), "{out}");

    // gaps_only: only nodes with a gap (and their ancestors) remain.
    let out = s.call(
        "handoff_trace_tree",
        json!({"layer": "requirement", "filter": {"gaps_only": true}}),
    );
    assert!(out.get("error").is_none(), "{out}");
    assert!(find_node(&out["roots"], "REQ-006").is_some(), "{out}");
    assert!(find_node(&out["roots"], "REQ-004").is_none(), "{out}");

    // suspect=true matches nothing in a clean project; suspect=false is all.
    let out = s.call("handoff_trace_tree", json!({"filter": {"suspect": true}}));
    assert_eq!(out["roots"], json!([]));
    let out = s.call("handoff_trace_tree", json!({"filter": {"suspect": false}}));
    assert_eq!(out["totals"]["nodes"], 9);

    // Invalid filter values are errors.
    for bad in [
        json!({"state": ["bogus"]}),
        json!({"priority": ["P9"]}),
        json!({"kind": ["bogus"]}),
        json!({"state": "failing"}),
        json!({"suspect": "yes"}),
    ] {
        let out = s.call("handoff_trace_tree", json!({"filter": bad}));
        assert!(out.get("error").is_some(), "{bad} must be rejected: {out}");
    }
}

#[test]
fn st_tree_008_fields_projection_keeps_structure() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_tree_fixture(&mut s);

    let out = s.call(
        "handoff_trace_tree",
        json!({"root": "REQ-001", "fields": ["own.state", "subtree.leaves"]}),
    );
    assert!(out.get("error").is_none(), "{out}");
    let root = &out["roots"][0];
    assert_eq!(root["id"], "REQ-001");
    assert_eq!(root["kind"], "composite");
    assert_eq!(root["parents"], json!([]));
    assert_eq!(root["has_children"], true);
    assert_eq!(node_ids(&root["children"]), ["REQ-002", "REQ-003"]);
    let own = root["own"].as_object().unwrap();
    assert_eq!(own.keys().collect::<Vec<_>>(), ["state"]);
    let subtree = root["subtree"].as_object().unwrap();
    assert_eq!(subtree.keys().collect::<Vec<_>>(), ["leaves"]);
    assert_eq!(subtree["leaves"], 3);

    // Only `own.*` requested: no subtree object at all.
    let out = s.call(
        "handoff_trace_tree",
        json!({"root": "REQ-006", "fields": ["own.state"]}),
    );
    assert!(out["roots"][0].get("subtree").is_none(), "{out}");

    for bad in [
        json!(["state"]),
        json!(["own.bogus"]),
        json!(["subtree.bogus"]),
        json!("own.state"),
    ] {
        let out = s.call("handoff_trace_tree", json!({"fields": bad}));
        assert!(out.get("error").is_some(), "{bad} must be rejected: {out}");
    }
}

#[test]
fn st_tree_008_parameter_validation() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_tree_fixture(&mut s);
    for bad in [
        json!({"depth": -1}),
        json!({"depth": "2"}),
        json!({"root": 5}),
        json!({"layer": 5}),
        json!({"filter": "failing"}),
    ] {
        let out = s.call("handoff_trace_tree", bad.clone());
        assert!(out.get("error").is_some(), "{bad} must be rejected: {out}");
    }
}

#[test]
fn st_tree_008_shared_node_is_expanded_once_and_deep_chain_is_safe() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    // A: A1, A2; both have child S; S has child T. S and T are shared.
    let body = "# R\n\n\
        ### REQ-001 A\n\nA.\n\n\
        ### REQ-002 A1\n\n- parent: REQ-001\n\nA.\n\n\
        ### REQ-003 A2\n\n- parent: REQ-001\n\nA.\n\n\
        ### REQ-004 S\n\n- parent: REQ-002, REQ-003\n\nA.\n\n\
        ### REQ-005 T\n\n- parent: REQ-004\n\nA.\n";
    assert!(s
        .save_layer("req-dag", "requirement", body)
        .get("error")
        .is_none());
    let out = s.call("handoff_trace_tree", json!({}));
    let a1 = find_node(&out["roots"], "REQ-002").unwrap();
    let a2 = find_node(&out["roots"], "REQ-003").unwrap();
    // The first occurrence is expanded, the second is cut with has_children.
    assert_eq!(node_ids(&a1["children"]), ["REQ-004"]);
    assert_eq!(a1["children"][0]["children"][0]["id"], "REQ-005");
    assert_eq!(node_ids(&a2["children"]), ["REQ-004"]);
    assert_eq!(a2["children"][0]["has_children"], true);
    assert!(a2["children"][0]["children"].is_null());
    assert_eq!(out["totals"]["nodes"], 5);
    assert_eq!(out["totals"]["dag_shared"], 1);
    assert_eq!(out["totals"]["truncated_subtrees"], 1);
}

#[test]
fn st_tree_008_empty_tree_reports_diagnostics() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    let out = s.call("handoff_trace_tree", json!({}));
    assert!(out.get("error").is_none(), "{out}");
    assert_eq!(out["roots"], json!([]));
    assert_eq!(out["totals"]["nodes"], 0);
    let codes: Vec<&str> = out["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|w| w["code"].as_str())
        .collect();
    assert!(codes.contains(&"DIAG-T001"), "{out}");
    assert!(codes.contains(&"DIAG-T002"), "{out}");
}

#[test]
fn st_tree_008_is_read_only_and_matches_the_report_numbers() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_tree_fixture(&mut s);
    // Materialize every lazily-created cache first, then require byte
    // invariance for the tool itself.
    let report = s.call("handoff_trace_report", json!({"include_items": true}));
    assert!(report.get("error").is_none(), "{report}");

    fn snapshot(dir: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        let mut out = Vec::new();
        let mut stack = vec![dir.to_path_buf()];
        while let Some(d) = stack.pop() {
            for entry in std::fs::read_dir(&d).unwrap().flatten() {
                let p = entry.path();
                if p.is_dir() {
                    stack.push(p);
                } else {
                    let bytes = std::fs::read(&p).unwrap();
                    out.push((p, bytes));
                }
            }
        }
        out.sort();
        out
    }
    let before = snapshot(&tmp.path().join(".handoff"));
    let out = s.call("handoff_trace_tree", json!({}));
    assert!(out.get("error").is_none(), "{out}");
    assert_eq!(before, snapshot(&tmp.path().join(".handoff")));

    // Same numbers as the report's per-item sub-tree (NFR-005).
    let report_item = report["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-001")
        .unwrap();
    let node = find_node(&out["roots"], "REQ-001").unwrap();
    assert_eq!(node["subtree"], report_item["subtree"]);
    assert_eq!(node["own"]["state"], report_item["state"]);
}

/// REQ-TREE-086 AC2 / SPEC-TREE-008: `gaps_only` matches uncovered, failing,
/// suspect and tree-structure gaps -- not merely unfinished (`not_run`)
/// nodes -- and `own.suspect` lists the suspect kinds.
#[test]
fn st_tree_008_gaps_only_semantics_and_suspect_shape() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    let req = "# R\n\n\
        ### REQ-001 Root\n\n\
        Root.\n\n\
        ### REQ-002 Verified leaf\n\n- parent: REQ-001\n\nA.\n\n\
        ### REQ-003 Not run leaf\n\n- parent: REQ-001\n\nB.\n\n\
        ### REQ-004 Mislabelled leaf\n\n- kind: leaf\n- parent: REQ-001\n\nC.\n\n\
        ### REQ-005 Child of a leaf\n\n- parent: REQ-004\n\nD.\n";
    let at = "# AT\n\n\
        ### AT-001 Passing\n\n- verifies: REQ-002\n- method: manual\n\nT.\n\n\
        ### AT-002 Not run\n\n- verifies: REQ-003\n- method: manual\n\nT.\n";
    assert!(s
        .save_layer("req-g", "requirement", req)
        .get("error")
        .is_none());
    assert!(s
        .save_layer("at-g", "acceptance", at)
        .get("error")
        .is_none());
    let rec = s.call(
        "handoff_trace_record",
        json!({"results": [{"item": "AT-001", "result": "pass"}]}),
    );
    assert!(rec.get("error").is_none(), "{rec}");

    // Clean of suspects and failures: a not_run leaf is NOT a gap; a tree
    // gap (kind_conflict: a `leaf` with children) is.
    let out = s.call("handoff_trace_tree", json!({"filter": {"gaps_only": true}}));
    assert!(out.get("error").is_none(), "{out}");
    assert!(find_node(&out["roots"], "REQ-003").is_none(), "{out}");
    assert!(find_node(&out["roots"], "AT-002").is_none(), "{out}");
    assert!(find_node(&out["roots"], "AT-001").is_none(), "{out}");
    assert!(find_node(&out["roots"], "REQ-004").is_some(), "{out}");

    // Edit REQ-002's body: AT-001 (passing) now carries a link suspect and
    // is included, with own.suspect = ["link"].
    let edited = s.body_of("req-g").replace("A.\n", "A changed.\n");
    let doc_id = std::fs::read_to_string(s.doc_path("req-g"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("id: "))
        .unwrap()
        .to_string();
    let saved = s.call(
        "handoff_doc_save",
        json!({"doc_id": doc_id, "body": edited}),
    );
    assert!(saved.get("error").is_none(), "{saved}");
    let out = s.call("handoff_trace_tree", json!({"filter": {"gaps_only": true}}));
    let at1 = find_node(&out["roots"], "AT-001").unwrap_or_else(|| panic!("{out}"));
    assert_eq!(at1["own"]["state"], "passing");
    assert_eq!(at1["own"]["suspect"], json!(["link"]));
    assert!(find_node(&out["roots"], "AT-002").is_none(), "{out}");
    let out = s.call("handoff_trace_tree", json!({"filter": {"suspect": true}}));
    assert_eq!(node_ids(&out["roots"]), ["AT-001"]);
}

/// SPEC-TREE-008: the tree-structure branch of `gaps_only` is independent of
/// the node's own state. A node that is `passing` and has no suspect is still
/// a gap when it carries a tree finding (`kind_conflict`: a declared `leaf`
/// with children); an equally passing, correctly-declared sibling is not.
#[test]
fn st_tree_008_gaps_only_includes_passing_node_with_tree_gap() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    let req = "# R\n\n\
        ### REQ-001 Mislabelled leaf\n\n- kind: leaf\n- waive-refine: fixture\n\nA.\n\n\
        ### REQ-002 Child of the leaf\n\n- parent: REQ-001\n- waive-refine: fixture\n\nB.\n\n\
        ### REQ-010 Proper group\n\n- kind: group\n- waive-refine: fixture\n\nC.\n\n\
        ### REQ-011 Child of the group\n\n- parent: REQ-010\n- waive-refine: fixture\n\nD.\n";
    let at = "# AT\n\n\
        ### AT-001 A\n\n- verifies: REQ-001\n- method: manual\n\nT.\n\n\
        ### AT-002 B\n\n- verifies: REQ-002\n- method: manual\n\nT.\n\n\
        ### AT-010 C\n\n- verifies: REQ-010\n- method: manual\n\nT.\n\n\
        ### AT-011 D\n\n- verifies: REQ-011\n- method: manual\n\nT.\n";
    assert!(s
        .save_layer("req-tg", "requirement", req)
        .get("error")
        .is_none());
    assert!(s
        .save_layer("at-tg", "acceptance", at)
        .get("error")
        .is_none());
    let rec = s.call(
        "handoff_trace_record",
        json!({"results": [
            {"item": "AT-001", "result": "pass"},
            {"item": "AT-002", "result": "pass"},
            {"item": "AT-010", "result": "pass"},
            {"item": "AT-011", "result": "pass"},
        ]}),
    );
    assert!(rec.get("error").is_none(), "{rec}");

    // Precondition: REQ-001 is passing with no suspect, so only its tree
    // finding can make it a gap.
    let full = s.call("handoff_trace_tree", json!({"layer": "requirement"}));
    let n = find_node(&full["roots"], "REQ-001").unwrap_or_else(|| panic!("{full}"));
    assert_eq!(n["own"]["state"], "passing", "{full}");
    assert_eq!(n["own"]["suspect"], json!([]), "{full}");

    let out = s.call(
        "handoff_trace_tree",
        json!({"layer": "requirement", "filter": {"gaps_only": true}}),
    );
    assert!(out.get("error").is_none(), "{out}");
    assert!(find_node(&out["roots"], "REQ-001").is_some(), "{out}");
    assert!(find_node(&out["roots"], "REQ-010").is_none(), "{out}");
    assert!(find_node(&out["roots"], "REQ-011").is_none(), "{out}");
}

// ---------------------------------------------------------------------------
// Branch labels: `kind: select`, `condition`, `condition@<parent>`
// ---------------------------------------------------------------------------

/// REQ-001 decision and REQ-002 select share REQ-003 (`condition@REQ-001:
/// yes`, `condition@REQ-002: case:2`); REQ-004 is the select's `default`
/// case; AT-001 verifies REQ-003.
fn save_branch_fixture(s: &mut Server) {
    let req = "# R\n\n\
        ### REQ-001 Decision\n\n- kind: decision\n\nD.\n\n\
        ### REQ-002 Select\n\n- kind: select\n\nS.\n\n\
        ### REQ-003 Shared\n\n- parent: REQ-001, REQ-002\n\
        - condition@REQ-001: yes\n- condition@REQ-002: case:2\n\nX.\n\n\
        ### REQ-004 Fallback\n\n- parent: REQ-002\n- condition: default\n\nY.\n";
    let at = "# AT\n\n### AT-001 Shared\n\n- verifies: REQ-003\n- method: manual\n\nT.\n";
    assert!(s
        .save_layer("req-branch", "requirement", req)
        .get("error")
        .is_none());
    assert!(s
        .save_layer("at-branch", "acceptance", at)
        .get("error")
        .is_none());
    let rec = s.call(
        "handoff_trace_record",
        json!({"results": [{"item": "AT-001", "result": "pass"}]}),
    );
    assert!(rec.get("error").is_none(), "{rec}");
}

fn gap_items(report: &Value, kind: &str) -> Vec<String> {
    report["gaps"]
        .as_array()
        .unwrap_or_else(|| panic!("no gaps in {report}"))
        .iter()
        .filter(|g| g["kind"] == kind)
        .map(|g| g["item"].as_str().unwrap().to_string())
        .collect()
}

#[test]
fn branch_labels_resolve_per_edge_in_the_report_and_turn_verifiers_suspect_when_flipped() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_branch_fixture(&mut s);

    let report = s.call("handoff_trace_report", json!({"include_items": true}));
    assert!(report.get("error").is_none(), "{report}");
    let item = |id: &str| {
        report["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == id)
            .unwrap_or_else(|| panic!("{id} missing"))
            .clone()
    };
    assert_eq!(item("REQ-002")["kind"], "select");
    assert_eq!(
        item("REQ-003")["conditions"],
        json!({"REQ-001": "yes", "REQ-002": "case:2"})
    );
    assert_eq!(item("REQ-004")["conditions"], json!({"REQ-002": "default"}));
    assert!(item("REQ-001").get("conditions").is_none());
    assert_eq!(report["totals"]["selects"], 1);
    assert_eq!(report["totals"]["decisions"], 1);
    assert_eq!(report["tree"]["select_count"], 1);
    assert!(report["gaps"]
        .as_array()
        .unwrap()
        .iter()
        .all(|g| g["kind"] != "condition_missing"));

    // Flip a label: def_hash moves, so the verifier of REQ-003 turns suspect.
    let before = s.sub_item("req-branch", "REQ-003")["def_hash"]
        .as_str()
        .unwrap()
        .to_string();
    let none = s.call("handoff_trace_tree", json!({"filter": {"suspect": true}}));
    assert_eq!(none["roots"], json!([]), "{none}");
    let flipped = s
        .body_of("req-branch")
        .replace("condition@REQ-001: yes", "condition@REQ-001: no");
    let doc_id = std::fs::read_to_string(s.doc_path("req-branch"))
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("id: "))
        .unwrap()
        .to_string();
    let saved = s.call(
        "handoff_doc_save",
        json!({"doc_id": doc_id, "body": flipped}),
    );
    assert!(saved.get("error").is_none(), "{saved}");
    let after = s.sub_item("req-branch", "REQ-003")["def_hash"]
        .as_str()
        .unwrap()
        .to_string();
    assert_ne!(before, after, "a flipped label must change def_hash");
    let sus = s.call("handoff_trace_tree", json!({"filter": {"suspect": true}}));
    let at = find_node(&sus["roots"], "AT-001").unwrap_or_else(|| panic!("{sus}"));
    assert_eq!(at["own"]["suspect"], json!(["link"]));
}

#[test]
fn branch_label_findings_reach_the_report_gaps_and_the_lint_output() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    let req = "# R\n\n\
        ### REQ-001 Plain group\n\n- kind: group\n\nG.\n\n\
        ### REQ-002 Labelled under a group\n\n- parent: REQ-001\n- condition: yes\n\nA.\n\n\
        ### REQ-003 Decision\n\n- kind: decision\n\nD.\n\n\
        ### REQ-004 No label\n\n- parent: REQ-003\n\nB.\n\n\
        ### REQ-005 Dup a\n\n- parent: REQ-003\n- condition: Same\n\nC.\n\n\
        ### REQ-006 Dup b\n\n- parent: REQ-003\n- condition:  same \n\nD.\n\n\
        ### REQ-007 Empty select\n\n- kind: select\n\nE.\n\n\
        ### REQ-008 Stray attr\n\n- parent: REQ-003\n- condition: z\n- condition@REQ-999: q\n\nF.\n";
    let saved = s.save_layer("req-gaps", "requirement", req);
    assert!(saved.get("error").is_none(), "{saved}");
    // Create-time validation refuses a dangling parent, so a forward
    // reference is written straight into the body.
    s.overwrite_body(
        "req-gaps",
        &format!(
            "{req}\n### REQ-009 Forward ref\n\n- parent: REQ-404\n- condition@REQ-404: q\n\nG.\n"
        ),
    );

    let report = s.call("handoff_trace_report", json!({}));
    assert!(report.get("error").is_none(), "{report}");
    assert_eq!(gap_items(&report, "condition_on_non_branch"), ["REQ-002"]);
    assert_eq!(gap_items(&report, "condition_missing"), ["REQ-004"]);
    assert_eq!(gap_items(&report, "condition_duplicate"), ["REQ-003"]);
    assert_eq!(gap_items(&report, "attr_unknown_parent"), ["REQ-008"]);
    assert_eq!(gap_items(&report, "decision_arity"), ["REQ-003"]);
    assert_eq!(gap_items(&report, "empty_branch"), ["REQ-007"]);
    assert_eq!(report["gap_counts"]["condition_duplicate"], 1);
    // A label aimed at a dangling parent is left to parent_dangling.
    assert_eq!(gap_items(&report, "parent_dangling"), ["REQ-009"]);

    // The label under a non-branch parent never reaches the index.
    let items = s.call("handoff_trace_report", json!({"include_items": true}));
    let req2 = items["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["id"] == "REQ-002")
        .unwrap();
    assert!(req2.get("conditions").is_none(), "{req2}");

    let lint = s.call("handoff_trace_lint", json!({}));
    for (rule, item) in [
        ("condition_on_non_branch", "REQ-002"),
        ("condition_missing", "REQ-004"),
        ("condition_duplicate", "REQ-003"),
        ("attr_unknown_parent", "REQ-008"),
        ("decision_arity", "REQ-003"),
        ("empty_branch", "REQ-007"),
    ] {
        assert_eq!(lint_items(&lint, rule), [item], "{rule}: {lint}");
        assert_eq!(lint_severities(&lint, rule), ["warning"], "{rule}");
    }
}

#[test]
fn trace_tree_returns_the_label_of_the_edge_each_node_is_emitted_under() {
    let tmp = tempfile::tempdir().unwrap();
    let mut s = Server::start(tmp.path());
    save_branch_fixture(&mut s);

    let out = s.call("handoff_trace_tree", json!({"layer": "requirement"}));
    assert!(out.get("error").is_none(), "{out}");
    assert_eq!(node_ids(&out["roots"]), ["REQ-001", "REQ-002"]);
    let decision = &out["roots"][0];
    let select = &out["roots"][1];
    assert_eq!(decision["kind"], "decision");
    assert_eq!(select["kind"], "select");
    assert!(decision.get("condition").is_none(), "roots carry no label");
    assert!(select.get("condition").is_none());

    // The same node, emitted under two parents, carries a different label.
    let under_decision = &decision["children"][0];
    assert_eq!(under_decision["id"], "REQ-003");
    assert_eq!(under_decision["condition"], "yes");
    let under_select = &select["children"][0];
    assert_eq!(under_select["id"], "REQ-003");
    assert_eq!(under_select["condition"], "case:2");
    let expected = json!({"REQ-001": "yes", "REQ-002": "case:2"});
    assert_eq!(under_decision["conditions"], expected);
    assert_eq!(under_select["conditions"], expected);
    let fallback = &select["children"][1];
    assert_eq!(fallback["condition"], "default");
    assert_eq!(fallback["conditions"], json!({"REQ-002": "default"}));

    // totals carry selects; filter.kind accepts select.
    assert_eq!(out["totals"]["selects"], 1);
    assert_eq!(out["totals"]["decisions"], 1);
    let only = s.call(
        "handoff_trace_tree",
        json!({"layer": "requirement", "filter": {"kind": ["select"]}}),
    );
    assert!(only.get("error").is_none(), "{only}");
    assert_eq!(node_ids(&only["roots"]), ["REQ-002"]);
    assert_eq!(only["totals"]["selects"], 1);
    assert_eq!(only["totals"]["decisions"], 0);

    // A `fields` projection trims own/subtree and optional keys but keeps the
    // edge labels (they are structure, like parents).
    let projected = s.call(
        "handoff_trace_tree",
        json!({"layer": "requirement", "fields": ["own.state"]}),
    );
    let node = &projected["roots"][1]["children"][0];
    assert_eq!(node["condition"], "case:2");
    assert_eq!(node["conditions"], expected);
    assert!(node.get("title").is_none());
    assert!(node.get("subtree").is_none());

    // A label under the wrong parent kind never shows up as a condition.
    let rooted = s.call("handoff_trace_tree", json!({"root": "REQ-003"}));
    assert!(rooted["roots"][0].get("condition").is_none(), "{rooted}");
    assert_eq!(rooted["roots"][0]["conditions"], expected);
}

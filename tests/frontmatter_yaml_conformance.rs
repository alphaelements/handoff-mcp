//! YAML conformance tests for document frontmatter (FR-804, E11,
//! wiki/260-vmodel-m2-design.md §4.12): every frontmatter block this crate
//! writes must parse the same way under `serde_yaml`'s own re-parse (already
//! covered as a self-check inside `serialize_frontmatter` itself — see
//! `src/storage/docs/frontmatter.rs`) *and* under an independent YAML 1.1
//! reader (PyYAML) — the reader `handoff-vscode`'s own frontmatter parser
//! (js-yaml) and this repository's CI both actually use. Per §4.12: "Python
//! / Node がない環境では skip を明示" — this test prints an explicit skip
//! message and returns early rather than failing when the interpreter or
//! library isn't available, for both PyYAML and js-yaml independently.
//!
//! Covers two corpora:
//! 1. A synthetic representative document exercising the specific
//!    cross-reader risk this task exists for: `serde_yaml` (YAML 1.2) does
//!    not quote a bare `yes`/`no`/`on`/`off` value, but PyYAML (YAML 1.1)
//!    would silently read it as a *boolean* rather than a string unless
//!    `serialize_frontmatter`'s YAML-1.1-ambiguous-scalar quoting (§4.12)
//!    kicks in.
//! 2. Every real `_doc.*.md` fixture already checked into `tests/fixtures/`
//!    (§4.12: "リポジトリの全fixture...のfrontmatterを...確認する").

use std::io::Write;
use std::process::{Command, Stdio};

use handoff_mcp::storage::docs::frontmatter::serialize_frontmatter;
use handoff_mcp::storage::docs::model::{DocMetadata, SubItem, Verification, VerificationItem};

/// `true` when `python3 -c 'import yaml'` succeeds (PyYAML installed).
fn pyyaml_available() -> bool {
    Command::new("python3")
        .args(["-c", "import yaml"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `true` when `node -e "require.resolve('js-yaml')"` succeeds (js-yaml
/// resolvable from the current working directory).
fn js_yaml_available() -> bool {
    Command::new("node")
        .args(["-e", "require.resolve('js-yaml')"])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Runs `script` (a Python program reading the frontmatter YAML on stdin)
/// via `python3 -c`, returning `(success, stderr)`.
fn run_pyyaml_check(yaml: &str, script: &str) -> (bool, String) {
    let mut child = Command::new("python3")
        .args(["-c", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn python3");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(yaml.as_bytes())
        .expect("write yaml to python3 stdin");
    let output = child.wait_with_output().expect("wait for python3");
    (
        output.status.success(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// A representative frontmatter document exercising: a `source:` nested
/// mapping, a `related[]` list, `tags` containing YAML-1.1-ambiguous
/// spellings (`yes`/`no`) alongside a normal tag, an `extra` (unknown-key,
/// `#[serde(flatten)]`) field with an ambiguous value, and a verification
/// matrix with one `SubItem` (stable_id/priority/dev_stage) — the shapes
/// most likely to actually hold free-text/tag-like string content.
fn representative_doc() -> DocMetadata {
    let mut doc = DocMetadata::new(
        "doc-20260928-000000-000001".to_string(),
        "conformance-fixture".to_string(),
        "Conformance Fixture".to_string(),
        "spec".to_string(),
        "2026-09-28T00:00:00Z".to_string(),
    );
    doc.tags = vec![
        "yes".to_string(),
        "no".to_string(),
        "normal-tag".to_string(),
    ];
    doc.scope_paths = vec!["src/storage/docs/".to_string()];
    doc.source.origin = "authored".to_string();
    doc.extra.insert(
        "custom_ambiguous_field".to_string(),
        serde_json::Value::String("off".to_string()),
    );
    doc.content_hash = Some("abc123def456".to_string());
    doc.verification = Some(Verification {
        status: "pending".to_string(),
        created_at: "2026-09-28T00:00:00Z".to_string(),
        updated_at: "2026-09-28T00:00:00Z".to_string(),
        items: vec![VerificationItem {
            fragment_seq: Some(1),
            heading: "Section".to_string(),
            status: "pending".to_string(),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            content_hash_at_verify: None,
            category: "section".to_string(),
            sub_items: vec![SubItem {
                index: 0,
                description: "A requirement".to_string(),
                stable_id: Some("C01-1.1".to_string()),
                dev_stage: Some("in_progress".to_string()),
                priority: Some("P0".to_string()),
                ..Default::default()
            }],
            label: None,
        }],
    });
    doc
}

#[test]
fn representative_frontmatter_is_readable_by_pyyaml_with_correct_string_types() {
    if !pyyaml_available() {
        eprintln!(
            "SKIP: python3 with PyYAML is not available in this environment — \
             representative_frontmatter_is_readable_by_pyyaml_with_correct_string_types skipped \
             (§4.12: environments without it must skip explicitly, not fail)"
        );
        return;
    }

    let yaml = serialize_frontmatter(&representative_doc()).unwrap();

    // PyYAML (YAML 1.1) must (a) parse without raising, (b) keep the
    // ambiguous tag values as `str`, not `bool` — the exact divergence
    // `serialize_frontmatter`'s YAML-1.1 quoting pass exists to close — and
    // (c) keep the flattened `extra` field's ambiguous value as `str` too.
    let script = r#"
import sys, yaml
doc = yaml.safe_load(sys.stdin.read())
assert isinstance(doc, dict), f"top-level frontmatter must be a mapping, got {type(doc)}"
tags = doc["tags"]
assert tags == ["yes", "no", "normal-tag"], f"tags mismatch: {tags!r}"
for t in tags:
    assert isinstance(t, str), f"tag {t!r} must be a string under PyYAML, got {type(t)}"
assert doc["custom_ambiguous_field"] == "off", f"custom_ambiguous_field: {doc['custom_ambiguous_field']!r}"
assert isinstance(doc["custom_ambiguous_field"], str)
assert doc["id"] == "doc-20260928-000000-000001"
assert doc["source"]["origin"] == "authored"
sub_items = doc["verification"]["items"][0]["sub_items"]
assert sub_items[0]["stable_id"] == "C01-1.1"
print("OK")
"#;
    let (ok, stderr) = run_pyyaml_check(&yaml, script);
    assert!(ok, "PyYAML conformance check failed: {stderr}\n---\n{yaml}");
}

#[test]
fn representative_frontmatter_is_readable_by_js_yaml() {
    if !js_yaml_available() {
        eprintln!(
            "SKIP: Node's js-yaml module is not resolvable in this environment — \
             representative_frontmatter_is_readable_by_js_yaml skipped (§4.12: environments \
             without it must skip explicitly, not fail)"
        );
        return;
    }

    let yaml = serialize_frontmatter(&representative_doc()).unwrap();
    let script = r#"
const yaml = require('js-yaml');
let data = '';
process.stdin.on('data', (chunk) => { data += chunk; });
process.stdin.on('end', () => {
    const doc = yaml.load(data);
    if (doc.tags.join(',') !== 'yes,no,normal-tag') {
        throw new Error('tags mismatch: ' + JSON.stringify(doc.tags));
    }
    for (const t of doc.tags) {
        if (typeof t !== 'string') {
            throw new Error('tag ' + t + ' must be a string, got ' + typeof t);
        }
    }
    if (doc.custom_ambiguous_field !== 'off') {
        throw new Error('custom_ambiguous_field: ' + JSON.stringify(doc.custom_ambiguous_field));
    }
    console.log('OK');
});
"#;
    let mut child = Command::new("node")
        .args(["-e", script])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn node");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(yaml.as_bytes())
        .expect("write yaml to node stdin");
    let output = child.wait_with_output().expect("wait for node");
    assert!(
        output.status.success(),
        "js-yaml conformance check failed: {}\n---\n{yaml}",
        String::from_utf8_lossy(&output.stderr)
    );
}

/// §4.12: "リポジトリの全fixtureと...の frontmatter を...PyYAML...で読めることを
/// 確認する" — every `_doc.*.md` already checked into `tests/fixtures/` must
/// have its frontmatter block parse under PyYAML too, not just under
/// `serde_yaml` (which every fixture already implicitly passes, since the
/// fixtures are read by this crate's own tests).
#[test]
fn every_committed_doc_fixture_frontmatter_is_readable_by_pyyaml() {
    if !pyyaml_available() {
        eprintln!(
            "SKIP: python3 with PyYAML is not available in this environment — \
             every_committed_doc_fixture_frontmatter_is_readable_by_pyyaml skipped"
        );
        return;
    }

    let fixtures_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut fixture_paths = Vec::new();
    collect_doc_fixtures(&fixtures_dir, &mut fixture_paths);
    assert!(
        !fixture_paths.is_empty(),
        "expected at least one _doc.*.md fixture under tests/fixtures/"
    );

    let script = r#"
import sys, yaml
text = sys.stdin.read()
assert text.startswith("---\n"), "fixture must start with a YAML frontmatter fence"
end = text.index("\n---", 4)
fm = text[4:end]
doc = yaml.safe_load(fm)
assert isinstance(doc, dict), f"top-level frontmatter must be a mapping, got {type(doc)}"
print("OK")
"#;

    for path in &fixture_paths {
        let content = std::fs::read_to_string(path).unwrap();
        let (ok, stderr) = run_pyyaml_check(&content, script);
        assert!(
            ok,
            "PyYAML failed to parse fixture {}: {stderr}",
            path.display()
        );
    }
}

fn collect_doc_fixtures(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_doc_fixtures(&path, out);
        } else if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            if name.starts_with("_doc.") && name.ends_with(".md") {
                out.push(path);
            }
        }
    }
}

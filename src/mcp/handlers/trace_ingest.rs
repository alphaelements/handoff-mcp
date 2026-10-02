//! `handoff_trace_ingest` (wiki/260-vmodel-m2-design.md §4.6, M2-11, FR-306):
//! ingests cargo's libtest JSON-per-line output or a JUnit XML report (e.g.
//! `cargo nextest run`'s `[profile.<name>.junit]`) and records matched
//! results — one aggregated run entry per layer item, one `test_refs` label
//! update per layer-less item (M1's convention, NFR-001).
//!
//! `handoff_doc_req_test_sync` (`src/mcp/handlers/docs_query.rs`) delegates
//! its cargo-JSON parsing and 3-stage matching to the same
//! `crate::storage::test_results` primitives this handler uses, but keeps
//! its own pre-M2 per-test (not per-item-aggregated) output shape — see that
//! module's doc comment.

use std::collections::HashSet;

use anyhow::{bail, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::docs::write_requirements_summary;
use super::HandlerContext;
use crate::storage::docs::{read_all_docs, write_doc, CodeRef, DocMetadata};
use crate::storage::runs::{record_run, RunResultInput};
use crate::storage::test_results::{
    aggregate_outcomes, declared_attr_matches, legacy_prefix_match, parse_cargo_test_jsonl,
    parse_junit_xml, test_name_module_path, ParsedTestResult, TestOutcome,
};

/// `(doc_index, item_index, sub_index, [(test_name, outcome)])` — which
/// SubItem to write labels onto, and which (test name, outcome) pairs to
/// write (layer-less items only; see `handle_trace_ingest`'s doc comment).
type LabelWrite = (usize, usize, usize, Vec<(String, TestOutcome)>);

fn to_json(v: &Value) -> String {
    serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
}

fn outcome_str(outcome: TestOutcome) -> &'static str {
    match outcome {
        TestOutcome::Pass => "pass",
        TestOutcome::Fail => "fail",
        TestOutcome::Skipped => "skipped",
    }
}

/// One item this ingestion recorded a result for.
#[derive(Debug, Clone, Serialize)]
struct IngestMatch {
    item: String,
    result: &'static str,
    tests: Vec<String>,
}

/// One declared `- test:` value an item has that no test in this
/// ingestion's output covered (§4.6: reported, not recorded — "部分実行を
/// not_run で上書きしないため").
#[derive(Debug, Clone, Serialize)]
struct MissingRef {
    item: String,
    test: String,
}

/// A single candidate item gathered from the document corpus, independent
/// of any particular ingestion's test names.
struct Candidate {
    doc_index: usize,
    item_index: usize,
    sub_index: usize,
    stable_id: String,
    /// Raw `- test: <value>` values (`SubItem.test_refs[].path`) — empty
    /// when the item has none (legacy-only matching, stage 3).
    test_attrs: Vec<String>,
    is_layer_item: bool,
}

fn collect_candidates(docs: &[DocMetadata]) -> Vec<Candidate> {
    let mut out = Vec::new();
    for (doc_index, doc) in docs.iter().enumerate() {
        let Some(v) = &doc.verification else { continue };
        for (item_index, item) in v.items.iter().enumerate() {
            for (sub_index, sub) in item.sub_items.iter().enumerate() {
                let Some(stable_id) = &sub.stable_id else {
                    continue;
                };
                let is_layer_item = doc.layer.is_some() || sub.origin.as_deref() == Some("body");
                // §4.6's 3-stage match (stages 1-2 against a declared `test`
                // attribute) applies to layer items only. A layer-less
                // item's `test_refs` holds M1 code refs *and* the
                // `pass:`/`fail:` labels this very tool writes back onto it
                // — treating those as "declared" values would make a test
                // that passed once, then failed on a later ingest, stop
                // matching at all (the stale "pass: ..." label never equals
                // the new test name). Layer-less items therefore keep M1's
                // stage-3-only legacy stable_id-prefix match, unconditional
                // on `test_refs`'s contents.
                let test_attrs = if is_layer_item {
                    sub.test_refs.iter().map(|r| r.path.clone()).collect()
                } else {
                    Vec::new()
                };
                out.push(Candidate {
                    doc_index,
                    item_index,
                    sub_index,
                    stable_id: stable_id.clone(),
                    test_attrs,
                    is_layer_item,
                });
            }
        }
    }
    out
}

/// `handoff_trace_ingest` (§4.6). Input: `format: "cargo_json" |
/// "junit_xml"` (required), `output` or `output_file` (exactly one, `output`
/// takes priority when both are given — same convention as
/// `handoff_doc_req_test_sync`), `commit?`, `task_id?`, `executor_kind?`
/// (default `"ai"`), `dry_run?` (default `false`).
///
/// Matching (§4.6, 3 stages via `storage::test_results::match_item_test`,
/// tried in priority order): (1) exact match against one of the item's
/// declared `- test:` values, (2) a `::`-boundary suffix match against one,
/// (3) M1's legacy `stable_id`->prefix convention (independent of any
/// declared value). An item with at least one declared `- test:` value is
/// recorded only when *every* declared value is covered by this ingestion's
/// output (§4.6: a partial match is reported via `missing_refs`, not
/// recorded, so it can never look like a `not_run` result overwriting a
/// fuller prior run). An item with no declared value is recorded on any
/// stage-3 match.
///
/// One ingestion = one `runs/<run_id>.json` file (all matched layer items in
/// a single batch, aggregated per item via `aggregate_outcomes`: any `fail`
/// wins, all-`skipped` wins next, else `pass`). Layer-less items instead get
/// a `test_refs` label update per matched test (M1's convention, NFR-001) —
/// not aggregated, since that mechanism already stores one label per test
/// name.
pub fn handle_trace_ingest(ctx: &HandlerContext, arguments: &Value) -> Result<String> {
    let handoff = &ctx.handoff_dir;

    let format = arguments
        .get("format")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'format' is required (\"cargo_json\" | \"junit_xml\")"))?;

    let output_arg = arguments.get("output").and_then(|v| v.as_str());
    let output_file_arg = arguments.get("output_file").and_then(|v| v.as_str());
    let input: String = if let Some(s) = output_arg {
        s.to_string()
    } else if let Some(path) = output_file_arg {
        std::fs::read_to_string(path)
            .map_err(|e| anyhow::anyhow!("failed to read output_file {path:?}: {e}"))?
    } else {
        bail!("handoff_trace_ingest requires either 'output' or 'output_file'");
    };

    // Unlike `handle_doc_req_test_sync`'s pre-M2 byte-compat shim (which
    // filters `Skipped` out to keep its matching/counts identical to before
    // M2), `handoff_trace_ingest` is a new tool with no compat constraint —
    // libtest `ignored` (-> `Skipped`) must reach matching so §4.6's
    // ignored -> skipped mapping and its all-skipped -> skipped aggregation
    // actually fire for `cargo_json` input, the same as they already do for
    // `junit_xml`.
    let tests: Vec<ParsedTestResult> = match format {
        "cargo_json" => parse_cargo_test_jsonl(&input),
        "junit_xml" => parse_junit_xml(&input)?,
        other => bail!("unknown format \"{other}\" (expected \"cargo_json\" | \"junit_xml\")"),
    };

    let dry_run = arguments
        .get("dry_run")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    let executor_kind = arguments
        .get("executor_kind")
        .and_then(|v| v.as_str())
        .unwrap_or("ai")
        .to_string();
    let executor_id = arguments
        .get("executor_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    let commit = arguments
        .get("commit")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| crate::storage::git::short_head_or_empty(&ctx.project_dir));
    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .map(str::to_string);

    let mut docs = read_all_docs(handoff)?;
    let candidates = collect_candidates(&docs);

    let mut matched: Vec<IngestMatch> = Vec::new();
    let mut missing_refs: Vec<MissingRef> = Vec::new();
    let mut warnings: Vec<String> = Vec::new();
    let mut consumed: HashSet<usize> = HashSet::new();
    // (doc_index, item_index, sub_index) -> test names to write as
    // "pass:"/"fail:" labels (layer-less items only).
    let mut label_writes: Vec<LabelWrite> = Vec::new();
    let mut run_inputs: Vec<(String, TestOutcome, Vec<String>)> = Vec::new();

    for candidate in &candidates {
        let matched_names_outcomes: Vec<(usize, &ParsedTestResult)>;
        if candidate.test_attrs.is_empty() {
            matched_names_outcomes = tests
                .iter()
                .enumerate()
                .filter(|(_, t)| legacy_prefix_match(&candidate.stable_id, &t.name))
                .collect();
            if matched_names_outcomes.is_empty() {
                continue;
            }
        } else {
            let mut collected: Vec<(usize, &ParsedTestResult)> = Vec::new();
            let mut any_attr_missing = false;
            for attr in &candidate.test_attrs {
                let hits: Vec<(usize, &ParsedTestResult)> = tests
                    .iter()
                    .enumerate()
                    .filter(|(_, t)| declared_attr_matches(attr, &t.name))
                    .collect();
                if hits.is_empty() {
                    missing_refs.push(MissingRef {
                        item: candidate.stable_id.clone(),
                        test: attr.clone(),
                    });
                    any_attr_missing = true;
                } else {
                    collected.extend(hits);
                }
            }
            if any_attr_missing {
                continue;
            }
            matched_names_outcomes = collected;
        }

        for (idx, _) in &matched_names_outcomes {
            consumed.insert(*idx);
        }
        let outcomes: Vec<TestOutcome> = matched_names_outcomes
            .iter()
            .map(|(_, t)| t.outcome)
            .collect();
        let Some(aggregated) = aggregate_outcomes(&outcomes) else {
            continue;
        };
        let mut names: Vec<String> = matched_names_outcomes
            .iter()
            .map(|(_, t)| t.name.clone())
            .collect();
        names.sort();
        names.dedup();

        matched.push(IngestMatch {
            item: candidate.stable_id.clone(),
            result: outcome_str(aggregated),
            tests: names.clone(),
        });

        if candidate.is_layer_item {
            run_inputs.push((candidate.stable_id.clone(), aggregated, names));
        } else {
            let per_test: Vec<(String, TestOutcome)> = matched_names_outcomes
                .iter()
                .map(|(_, t)| (t.name.clone(), t.outcome))
                .collect();
            label_writes.push((
                candidate.doc_index,
                candidate.item_index,
                candidate.sub_index,
                per_test,
            ));
        }
    }

    let unmatched_tests_count = tests.len() - consumed.len();

    let mut run_id: Option<String> = None;
    if !dry_run {
        for (doc_index, item_index, sub_index, per_test) in &label_writes {
            let sub = &mut docs[*doc_index]
                .verification
                .as_mut()
                .expect("layer-less candidate implies verification present")
                .items[*item_index]
                .sub_items[*sub_index];
            for (test_name, outcome) in per_test {
                // A layer-less item's `test_refs` label keeps M1's
                // pass/fail-only convention — a `Skipped` (cargo_json
                // `ignored`, or JUnit `<skipped>`) result is never written
                // as a label; only a layer item's aggregated *run* result
                // (via `aggregate_outcomes`) surfaces `skipped`.
                if *outcome == TestOutcome::Skipped {
                    continue;
                }
                let label = format!(
                    "{}: {test_name}",
                    if *outcome == TestOutcome::Pass {
                        "pass"
                    } else {
                        "fail"
                    }
                );
                let existing = sub.test_refs.iter_mut().find(|r| {
                    r.label.as_deref().is_some_and(|l| {
                        l.ends_with(test_name.as_str())
                            && (l.starts_with("pass: ") || l.starts_with("fail: "))
                    })
                });
                match existing {
                    Some(coderef) => coderef.label = Some(label),
                    None => sub.test_refs.push(CodeRef {
                        path: test_name_module_path(test_name).to_string(),
                        lines: None,
                        label: Some(label),
                    }),
                }
            }
        }
        let touched_doc_ids: HashSet<String> = label_writes
            .iter()
            .map(|(doc_index, _, _, _)| docs[*doc_index].id.clone())
            .collect();
        for doc in &docs {
            if touched_doc_ids.contains(&doc.id) {
                write_doc(handoff, doc)?;
            }
        }

        let all_docs = if touched_doc_ids.is_empty() {
            docs
        } else {
            read_all_docs(handoff)?
        };

        if !run_inputs.is_empty() {
            let inputs: Vec<RunResultInput> = run_inputs
                .iter()
                .map(|(item, outcome, names)| RunResultInput {
                    item: item.as_str(),
                    result: outcome_str(*outcome),
                    note: None,
                    evidence: names.clone(),
                })
                .collect();
            let (recorded_run_id, run_warnings) = record_run(
                handoff,
                &all_docs,
                &inputs,
                &executor_kind,
                executor_id.as_deref(),
                Some(commit),
                task_id,
            )?;
            warnings.extend(run_warnings);
            run_id = Some(recorded_run_id);
        }

        write_requirements_summary(handoff, &all_docs)?;
    }

    Ok(to_json(&json!({
        "run_id": run_id,
        "recorded": matched.len(),
        "matched": matched,
        "missing_refs": missing_refs,
        "unmatched_tests_count": unmatched_tests_count,
        "warnings": warnings,
        "dry_run": dry_run,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::{write_doc, CodeRef, SubItem, Verification, VerificationItem};
    use std::path::PathBuf;
    use tempfile::TempDir;

    fn ctx(handoff: PathBuf) -> HandlerContext {
        HandlerContext {
            agent_id: None,
            project_dir: handoff.parent().unwrap().to_path_buf(),
            handoff_dir: handoff,
        }
    }

    fn setup() -> (TempDir, std::path::PathBuf) {
        let tmp = TempDir::new().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        (tmp, handoff)
    }

    fn sub_item_with_test(stable_id: &str, test_attrs: &[&str]) -> SubItem {
        SubItem {
            index: 0,
            description: format!("desc {stable_id}"),
            stable_id: Some(stable_id.to_string()),
            test_refs: test_attrs
                .iter()
                .map(|t| CodeRef {
                    path: t.to_string(),
                    lines: None,
                    label: None,
                })
                .collect(),
            ..Default::default()
        }
    }

    fn section_item(sub_items: Vec<SubItem>) -> VerificationItem {
        VerificationItem {
            fragment_seq: Some(1),
            heading: "heading".to_string(),
            status: "pending".to_string(),
            impl_refs: Vec::new(),
            test_refs: Vec::new(),
            reviewer: None,
            verified_at: None,
            notes: String::new(),
            content_hash_at_verify: None,
            category: "section".to_string(),
            sub_items,
            label: None,
        }
    }

    fn doc_with_items(
        id: &str,
        slug: &str,
        layer: Option<&str>,
        items: Vec<VerificationItem>,
    ) -> DocMetadata {
        let mut d = DocMetadata::new(
            id.to_string(),
            slug.to_string(),
            format!("Title {id}"),
            "spec".to_string(),
            "2026-09-28T00:00:00Z".to_string(),
        );
        d.layer = layer.map(str::to_string);
        d.verification = Some(Verification {
            status: "in_review".to_string(),
            created_at: "2026-09-28T00:00:00Z".to_string(),
            updated_at: "2026-09-28T00:00:00Z".to_string(),
            items,
        });
        d
    }

    /// A layer item with a single declared `test` attribute, matched exactly
    /// once (stage 1) by a cargo_json `ok` result, is recorded as a single
    /// aggregated `pass` run entry (not a `test_refs` write — layer items
    /// are body-owned, wiki/220 §2.6).
    #[test]
    fn cargo_json_records_one_run_entry_for_a_layer_item_exact_match() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-a",
            "st-a",
            Some("system_test"),
            vec![section_item(vec![sub_item_with_test(
                "ST-001",
                &["tests::lock::lock_after_5"],
            )])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = r#"{"type":"test","event":"ok","name":"tests::lock::lock_after_5"}"#;
        let out: Value = serde_json::from_str(
            &handle_trace_ingest(&c, &json!({ "format": "cargo_json", "output": input })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["recorded"], 1);
        assert_eq!(out["matched"][0]["item"], "ST-001");
        assert_eq!(out["matched"][0]["result"], "pass");
        assert!(out["missing_refs"].as_array().unwrap().is_empty());
        let run_id = out["run_id"].as_str().expect("run_id must be present");
        let run_content =
            std::fs::read_to_string(handoff.join("runs").join(format!("{run_id}.json"))).unwrap();
        let run_json: Value = serde_json::from_str(&run_content).unwrap();
        assert_eq!(run_json["results"][0]["item"], "ST-001");
        assert_eq!(run_json["results"][0]["result"], "pass");
    }

    /// Two tests matching the same item (a fail and a pass) aggregate to
    /// `fail` (§4.6: "1つでも fail なら fail").
    #[test]
    fn multiple_matches_for_one_item_aggregate_any_fail_wins() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-b",
            "st-b",
            Some("system_test"),
            vec![section_item(vec![sub_item_with_test(
                "ST-002",
                &["tests::a", "tests::b"],
            )])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = concat!(
            "{\"type\":\"test\",\"event\":\"ok\",\"name\":\"tests::a\"}\n",
            "{\"type\":\"test\",\"event\":\"failed\",\"name\":\"tests::b\"}\n",
        );
        let out: Value = serde_json::from_str(
            &handle_trace_ingest(&c, &json!({ "format": "cargo_json", "output": input })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"][0]["result"], "fail");
        let tests = out["matched"][0]["tests"].as_array().unwrap();
        assert_eq!(tests.len(), 2);
    }

    /// A declared `test` attribute with zero matches in this ingestion's
    /// output is reported via `missing_refs`, and the item is not recorded
    /// at all this call (even though its *other* declared test did match).
    #[test]
    fn declared_test_missing_from_output_is_reported_and_item_is_not_recorded() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-c",
            "st-c",
            Some("system_test"),
            vec![section_item(vec![sub_item_with_test(
                "ST-003",
                &["tests::present", "tests::absent"],
            )])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = r#"{"type":"test","event":"ok","name":"tests::present"}"#;
        let out: Value = serde_json::from_str(
            &handle_trace_ingest(&c, &json!({ "format": "cargo_json", "output": input })).unwrap(),
        )
        .unwrap();

        assert!(out["matched"].as_array().unwrap().is_empty());
        assert_eq!(out["missing_refs"][0]["item"], "ST-003");
        assert_eq!(out["missing_refs"][0]["test"], "tests::absent");
        assert!(out["run_id"].is_null());
    }

    /// A layer-less item's matched test is written as a `test_refs` label
    /// (M1's convention, NFR-001), not a run entry.
    #[test]
    fn layer_less_item_gets_a_test_refs_label_not_a_run() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-d",
            "req-d",
            None,
            vec![section_item(vec![SubItem {
                index: 0,
                description: "desc".to_string(),
                stable_id: Some("C01-2.1.1.1".to_string()),
                ..Default::default()
            }])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = r#"{"type":"test","event":"ok","name":"tests::test_c01_2_1_1_1_rect"}"#;
        let out: Value = serde_json::from_str(
            &handle_trace_ingest(&c, &json!({ "format": "cargo_json", "output": input })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"][0]["item"], "C01-2.1.1.1");
        assert!(
            out["run_id"].is_null(),
            "no layer-item run for a layer-less match"
        );

        let reloaded = crate::storage::docs::read_doc(&handoff, "req-d")
            .unwrap()
            .unwrap();
        let sub = &reloaded.verification.unwrap().items[0].sub_items[0];
        assert_eq!(sub.test_refs.len(), 1);
        assert_eq!(
            sub.test_refs[0].label.as_deref(),
            Some("pass: tests::test_c01_2_1_1_1_rect")
        );
    }

    /// `dry_run` parses and matches without writing a run file or touching
    /// any document.
    #[test]
    fn dry_run_does_not_write_anything() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-e",
            "st-e",
            Some("system_test"),
            vec![section_item(vec![sub_item_with_test(
                "ST-004",
                &["tests::x"],
            )])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = r#"{"type":"test","event":"ok","name":"tests::x"}"#;
        let out: Value = serde_json::from_str(
            &handle_trace_ingest(
                &c,
                &json!({ "format": "cargo_json", "output": input, "dry_run": true }),
            )
            .unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"][0]["item"], "ST-004");
        assert!(out["run_id"].is_null());
        assert!(
            !handoff.join("runs").exists()
                || std::fs::read_dir(handoff.join("runs"))
                    .unwrap()
                    .next()
                    .is_none(),
            "dry_run must not write a run file"
        );
    }

    /// `format: "junit_xml"` ingests a JUnit report end to end, including a
    /// `<failure>` child mapping to `fail`.
    #[test]
    fn junit_xml_format_ingests_pass_and_fail() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-f",
            "st-f",
            Some("system_test"),
            vec![section_item(vec![
                sub_item_with_test("ST-005", &["mod1::t1"]),
                sub_item_with_test("ST-006", &["mod2::t2"]),
            ])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let xml = r#"<testsuite>
  <testcase classname="mod1" name="t1"/>
  <testcase classname="mod2" name="t2"><failure/></testcase>
</testsuite>"#;
        let out: Value = serde_json::from_str(
            &handle_trace_ingest(&c, &json!({ "format": "junit_xml", "output": xml })).unwrap(),
        )
        .unwrap();

        let matched = out["matched"].as_array().unwrap();
        assert_eq!(matched.len(), 2);
        let by_item = |id: &str| matched.iter().find(|m| m["item"] == id).unwrap();
        assert_eq!(by_item("ST-005")["result"], "pass");
        assert_eq!(by_item("ST-006")["result"], "fail");
    }

    /// A layer-less item's matched test is re-matched correctly on a
    /// *repeat* ingest, even though the first ingest already wrote its own
    /// `pass:`/`fail:` label onto the item's `test_refs` (M2-11 rework:
    /// `collect_candidates` must not treat a layer-less item's own
    /// previously-written result label as a "declared `- test:` value" for
    /// stage-1/2 matching — only layer items have real declared `test`
    /// attributes; a layer-less item always matches via stage 3 (M1's
    /// legacy stable_id prefix convention), regardless of what `test_refs`
    /// currently holds).
    #[test]
    fn layer_less_item_matches_again_after_a_prior_ingest_wrote_its_own_label() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-g",
            "req-g",
            None,
            vec![section_item(vec![SubItem {
                index: 0,
                description: "desc".to_string(),
                stable_id: Some("C01-2.1.1.1".to_string()),
                ..Default::default()
            }])],
        );
        write_doc(&handoff, &doc).unwrap();
        let c = ctx(handoff.clone());

        let ok_input = r#"{"type":"test","event":"ok","name":"tests::test_c01_2_1_1_1_rect"}"#;
        let out1: Value = serde_json::from_str(
            &handle_trace_ingest(&c, &json!({ "format": "cargo_json", "output": ok_input }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(out1["matched"][0]["item"], "C01-2.1.1.1");
        assert!(out1["missing_refs"].as_array().unwrap().is_empty());

        // Same test, now failing: must still match — not fall into
        // `missing_refs` because the first ingest's own "pass: ..." label
        // got mistaken for a declared `test` attribute.
        let fail_input =
            r#"{"type":"test","event":"failed","name":"tests::test_c01_2_1_1_1_rect"}"#;
        let out2: Value = serde_json::from_str(
            &handle_trace_ingest(&c, &json!({ "format": "cargo_json", "output": fail_input }))
                .unwrap(),
        )
        .unwrap();
        assert!(
            out2["missing_refs"].as_array().unwrap().is_empty(),
            "expected no missing_refs on repeat ingest: {out2}"
        );
        assert_eq!(out2["matched"][0]["item"], "C01-2.1.1.1");
        assert_eq!(out2["matched"][0]["result"], "fail");

        let reloaded = crate::storage::docs::read_doc(&handoff, "req-g")
            .unwrap()
            .unwrap();
        let sub = &reloaded.verification.unwrap().items[0].sub_items[0];
        assert!(
            sub.test_refs
                .iter()
                .any(|r| r.label.as_deref() == Some("fail: tests::test_c01_2_1_1_1_rect")),
            "expected the label to be updated to fail: {:?}",
            sub.test_refs
        );
    }

    /// libtest `ignored` maps to `skipped` for a layer item (§4.6): the
    /// aggregated run result is `skipped`, not a dropped/missing match — the
    /// tool must not filter `Skipped` results out of the `cargo_json` input
    /// itself (that filter belongs only to `handoff_doc_req_test_sync`'s
    /// pre-M2 byte-compat shim).
    #[test]
    fn cargo_json_ignored_test_maps_to_skipped_result_for_a_layer_item() {
        let (_tmp, handoff) = setup();
        let doc = doc_with_items(
            "doc-h",
            "st-h",
            Some("system_test"),
            vec![section_item(vec![sub_item_with_test(
                "ST-007",
                &["tests::slow"],
            )])],
        );
        write_doc(&handoff, &doc).unwrap();

        let c = ctx(handoff.clone());
        let input = r#"{"type":"test","event":"ignored","name":"tests::slow"}"#;
        let out: Value = serde_json::from_str(
            &handle_trace_ingest(&c, &json!({ "format": "cargo_json", "output": input })).unwrap(),
        )
        .unwrap();

        assert_eq!(out["matched"][0]["item"], "ST-007");
        assert_eq!(out["matched"][0]["result"], "skipped");
        assert!(out["missing_refs"].as_array().unwrap().is_empty());
        let run_id = out["run_id"].as_str().expect("run_id must be present");
        let run_content =
            std::fs::read_to_string(handoff.join("runs").join(format!("{run_id}.json"))).unwrap();
        let run_json: Value = serde_json::from_str(&run_content).unwrap();
        assert_eq!(run_json["results"][0]["item"], "ST-007");
        assert_eq!(run_json["results"][0]["result"], "skipped");
    }

    #[test]
    fn unknown_format_is_an_error() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_ingest(&c, &json!({ "format": "yaml", "output": "" }));
        assert!(err.is_err());
    }

    #[test]
    fn missing_output_and_output_file_is_an_error() {
        let (_tmp, handoff) = setup();
        let c = ctx(handoff);
        let err = handle_trace_ingest(&c, &json!({ "format": "cargo_json" }));
        assert!(err.is_err());
    }
}

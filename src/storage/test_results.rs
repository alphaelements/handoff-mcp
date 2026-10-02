//! Test-result ingestion shared core (wiki/260-vmodel-m2-design.md §4.6,
//! M2-11): parses cargo's libtest JSON-per-line output and JUnit XML into a
//! normalized shape, and matches an ingested test name against a layer
//! item's `test` attribute (3-stage) or M1's legacy stable_id->prefix
//! convention. `handoff_trace_ingest`
//! (`src/mcp/handlers/trace_ingest.rs`) and the now-delegating
//! `handoff_doc_req_test_sync` (`src/mcp/handlers/docs_query.rs`) are this
//! module's only consumers.

use anyhow::{Context, Result};
use quick_xml::events::Event;
use quick_xml::Reader;

/// One ingested test's outcome, normalized across cargo JSON and JUnit XML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TestOutcome {
    Pass,
    Fail,
    Skipped,
}

/// One test result as ingested, before matching against any item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedTestResult {
    pub name: String,
    pub outcome: TestOutcome,
}

/// Which of wiki/260 §4.6's 3 matching stages matched an item against a
/// test name — priority order is the declaration order here (a caller
/// checking `match_item_test`'s `Some` already got the highest-priority
/// match; this is only for callers that want to report *how* it matched).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MatchKind {
    ExactTestAttr,
    SuffixTestAttr,
    LegacyPrefix,
}

/// Parses `cargo test`'s libtest JSON-per-line output (one JSON object per
/// line — §4.6's nightly/`RUSTC_BOOTSTRAP=1` `-Z unstable-options --format
/// json`). Malformed lines, non-`type=="test"` lines (e.g. `type=="suite"`
/// summaries), and `type=="test"` lines whose `event` is none of
/// `ok`/`failed`/`ignored` (e.g. `"started"`) are silently skipped.
/// `event=="ignored"` maps to [`TestOutcome::Skipped`] (§4.6: "`ignored` ->
/// skipped" — M1's now-superseded `parse_cargo_test_jsonl` only read
/// `ok`/`failed`).
pub fn parse_cargo_test_jsonl(input: &str) -> Vec<ParsedTestResult> {
    let mut results = Vec::new();
    for line in input.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) else {
            continue;
        };
        if value.get("type").and_then(|v| v.as_str()) != Some("test") {
            continue;
        }
        let Some(name) = value.get("name").and_then(|v| v.as_str()) else {
            continue;
        };
        let outcome = match value.get("event").and_then(|v| v.as_str()) {
            Some("ok") => TestOutcome::Pass,
            Some("failed") => TestOutcome::Fail,
            Some("ignored") => TestOutcome::Skipped,
            _ => continue,
        };
        results.push(ParsedTestResult {
            name: name.to_string(),
            outcome,
        });
    }
    results
}

/// Parses a JUnit XML report (`<testsuite>`/`<testsuites>` containing
/// `<testcase classname="..." name="...">`, e.g. `cargo nextest run`'s
/// `[profile.<name>.junit]` output, wiki/260 §4.6's recommended source) into
/// [`ParsedTestResult`]s. A `<testcase>` is `Fail` if it has a `<failure>`
/// or `<error>` child, `Skipped` if it has a `<skipped>` child, else `Pass`.
/// `classname` (with `.`-separated segments converted to `::`) and `name`
/// are joined as `classname::name`; a `testcase` with no `classname`
/// attribute uses `name` alone.
pub fn parse_junit_xml(input: &str) -> Result<Vec<ParsedTestResult>> {
    let mut reader = Reader::from_str(input);
    reader.config_mut().trim_text(true);

    let mut results = Vec::new();
    // The `testcase` currently open, as (name, outcome-so-far) — `Start`
    // sets it, a `failure`/`error`/`skipped` child (in any order, or
    // multiple) escalates it, and the matching `End`/self-closing `Empty`
    // pushes it. JUnit does not nest `testcase` elements, so a single slot
    // (not a stack) is enough.
    let mut current: Option<(String, TestOutcome)> = None;
    let mut buf = Vec::new();

    loop {
        match reader
            .read_event_into(&mut buf)
            .context("JUnit XML parse error")?
        {
            Event::Start(e) => {
                if e.local_name().as_ref() == "testcase" {
                    current = Some((testcase_full_name(&e)?, TestOutcome::Pass));
                } else if is_failure_tag(e.local_name().as_ref()) {
                    if let Some((_, outcome)) = &mut current {
                        *outcome = TestOutcome::Fail;
                    }
                } else if e.local_name().as_ref() == "skipped" {
                    if let Some((_, outcome)) = &mut current {
                        if *outcome != TestOutcome::Fail {
                            *outcome = TestOutcome::Skipped;
                        }
                    }
                }
            }
            Event::Empty(e) => {
                if e.local_name().as_ref() == "testcase" {
                    results.push(ParsedTestResult {
                        name: testcase_full_name(&e)?,
                        outcome: TestOutcome::Pass,
                    });
                } else if is_failure_tag(e.local_name().as_ref()) {
                    if let Some((_, outcome)) = &mut current {
                        *outcome = TestOutcome::Fail;
                    }
                } else if e.local_name().as_ref() == "skipped" {
                    if let Some((_, outcome)) = &mut current {
                        if *outcome != TestOutcome::Fail {
                            *outcome = TestOutcome::Skipped;
                        }
                    }
                }
            }
            Event::End(e) => {
                if e.local_name().as_ref() == "testcase" {
                    if let Some((name, outcome)) = current.take() {
                        results.push(ParsedTestResult { name, outcome });
                    }
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }

    Ok(results)
}

fn is_failure_tag(local_name: &str) -> bool {
    local_name == "failure" || local_name == "error"
}

/// `classname` (`.`-segments converted to `::`) joined with `name` as
/// `classname::name`; `name` alone when `classname` is absent/empty.
fn testcase_full_name(e: &quick_xml::events::BytesStart) -> Result<String> {
    let mut classname = String::new();
    let mut name = String::new();
    for attr in e.attributes() {
        let attr = attr.context("JUnit XML: malformed attribute")?;
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)
            .context("JUnit XML: malformed attribute value")?
            .into_owned();
        match attr.key.local_name().as_ref() {
            "classname" => classname = value,
            "name" => name = value,
            _ => {}
        }
    }
    Ok(if classname.is_empty() {
        name
    } else {
        format!("{}::{}", classname.replace('.', "::"), name)
    })
}

/// The M1 legacy convention (`docs_query::stable_id_to_test_name_prefix`,
/// requirements-traceability P2 §5.1): a `stable_id` (e.g. `"C01-2.1.1.1"`)
/// converts to the lowercase, underscore-joined prefix a `test_name`-pattern
/// test function is expected to start with (e.g. `"test_c01_2_1_1_1"`).
pub fn stable_id_to_test_name_prefix(stable_id: &str) -> String {
    let mut out = String::from("test_");
    let mut last_was_sep = false;
    for ch in stable_id.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_sep = false;
        } else if !last_was_sep {
            out.push('_');
            last_was_sep = true;
        }
    }
    out
}

/// `attr`'s `::`-separated segments are a trailing subsequence of
/// `test_name`'s own segments (wiki/260 §4.6's stage 2 example: `test:
/// lock::lock_after_5` matches `tests::e2e::lock::lock_after_5`).
fn suffix_boundary_match(attr: &str, test_name: &str) -> bool {
    let attr_segs: Vec<&str> = attr.split("::").collect();
    let name_segs: Vec<&str> = test_name.split("::").collect();
    if attr_segs.len() > name_segs.len() || attr_segs.is_empty() {
        return false;
    }
    let start = name_segs.len() - attr_segs.len();
    name_segs[start..] == attr_segs[..]
}

/// Whether `test_name` satisfies stage 1 (exact) or stage 2 (`::`-boundary
/// suffix) of wiki/260 §4.6's match against a single declared `- test:`
/// value — used by `handoff_trace_ingest` to decide whether a *specific*
/// declared attribute was covered by this ingestion (§4.6's `missing_refs`:
/// stage 3's legacy convention is independent of any declared attribute, so
/// it does not participate in "was this declared test present" checks).
pub fn declared_attr_matches(attr: &str, test_name: &str) -> bool {
    attr == test_name || suffix_boundary_match(attr, test_name)
}

/// M1's legacy `stable_id` -> test-name-prefix convention, standalone (no
/// declared `test` attribute involved) — wiki/260 §4.6's stage 3.
pub fn legacy_prefix_match(stable_id: &str, test_name: &str) -> bool {
    let prefix = stable_id_to_test_name_prefix(stable_id);
    let bare = test_name.rsplit("::").next().unwrap_or(test_name);
    bare.starts_with(&prefix)
}

/// wiki/260 §4.6's 3-stage match, tried in priority order: (1) exact match
/// against one of `test_attrs` (an item's `- test: <value>` lines), (2) a
/// `::`-boundary suffix match against one of `test_attrs`, (3) M1's legacy
/// `stable_id` -> test-name-prefix convention (independent of `test_attrs` —
/// preserves `handoff_doc_req_test_sync`'s pre-M2 behavior verbatim for
/// items with no `test` attribute at all, and also applies when one is
/// present but stages 1-2 didn't match).
pub fn match_item_test(
    test_attrs: &[String],
    stable_id: &str,
    test_name: &str,
) -> Option<MatchKind> {
    if test_attrs.iter().any(|a| a == test_name) {
        return Some(MatchKind::ExactTestAttr);
    }
    if test_attrs
        .iter()
        .any(|a| suffix_boundary_match(a, test_name))
    {
        return Some(MatchKind::SuffixTestAttr);
    }
    if legacy_prefix_match(stable_id, test_name) {
        return Some(MatchKind::LegacyPrefix);
    }
    None
}

/// Derives a `CodeRef.path` for a matched test result on a layer-less item
/// (M1's `req_test_sync` convention, requirements-traceability P3 §6.1):
/// the test name's module path (everything before the last `::`), or the
/// full name when there is no `::` separator (no file/line is available
/// from either cargo JSON or JUnit XML, so the module path is the closest
/// available proxy).
pub fn test_name_module_path(test_name: &str) -> &str {
    match test_name.rsplit_once("::") {
        Some((module, _fn_name)) => module,
        None => test_name,
    }
}

/// wiki/260 §4.6's aggregation rule for one item matched by more than one
/// test in the same ingestion: any `Fail` wins outright; all-`Skipped` wins
/// next; otherwise (at least one `Pass`, and no `Fail`) `Pass` wins. `None`
/// for an empty slice (no test matched this item at all).
pub fn aggregate_outcomes(outcomes: &[TestOutcome]) -> Option<TestOutcome> {
    if outcomes.is_empty() {
        return None;
    }
    if outcomes.contains(&TestOutcome::Fail) {
        return Some(TestOutcome::Fail);
    }
    if outcomes.iter().all(|o| *o == TestOutcome::Skipped) {
        return Some(TestOutcome::Skipped);
    }
    Some(TestOutcome::Pass)
}

#[cfg(test)]
mod tests {
    use super::*;

    // -- parse_cargo_test_jsonl --

    #[test]
    fn parses_ok_failed_and_ignored_skips_other_lines() {
        let input = r#"
{"type":"suite","event":"started"}
{"type":"test","event":"started","name":"tests::a"}
{"type":"test","name":"tests::a","event":"ok"}
{"type":"test","name":"tests::b","event":"failed"}
{"type":"test","name":"tests::c","event":"ignored"}
not json at all
{"type":"test","name":"tests::d"}
"#;
        let results = parse_cargo_test_jsonl(input);
        assert_eq!(
            results,
            vec![
                ParsedTestResult {
                    name: "tests::a".to_string(),
                    outcome: TestOutcome::Pass
                },
                ParsedTestResult {
                    name: "tests::b".to_string(),
                    outcome: TestOutcome::Fail
                },
                ParsedTestResult {
                    name: "tests::c".to_string(),
                    outcome: TestOutcome::Skipped
                },
            ]
        );
    }

    #[test]
    fn empty_input_yields_no_results() {
        assert!(parse_cargo_test_jsonl("").is_empty());
    }

    // -- parse_junit_xml --

    #[test]
    fn junit_testcase_with_no_child_is_pass() {
        let xml = r#"<?xml version="1.0"?>
<testsuite name="s">
  <testcase classname="tests.e2e.lock" name="lock_after_5"/>
</testsuite>"#;
        let results = parse_junit_xml(xml).unwrap();
        assert_eq!(
            results,
            vec![ParsedTestResult {
                name: "tests::e2e::lock::lock_after_5".to_string(),
                outcome: TestOutcome::Pass,
            }]
        );
    }

    #[test]
    fn junit_testcase_with_failure_child_is_fail() {
        let xml = r#"<testsuite>
  <testcase classname="tests" name="b">
    <failure message="boom">stack trace</failure>
  </testcase>
</testsuite>"#;
        let results = parse_junit_xml(xml).unwrap();
        assert_eq!(results[0].outcome, TestOutcome::Fail);
    }

    #[test]
    fn junit_testcase_with_error_child_is_fail() {
        let xml = r#"<testsuite>
  <testcase classname="tests" name="c">
    <error message="panic"/>
  </testcase>
</testsuite>"#;
        let results = parse_junit_xml(xml).unwrap();
        assert_eq!(results[0].outcome, TestOutcome::Fail);
    }

    #[test]
    fn junit_testcase_with_skipped_child_is_skipped() {
        let xml = r#"<testsuite>
  <testcase classname="tests" name="d">
    <skipped/>
  </testcase>
</testsuite>"#;
        let results = parse_junit_xml(xml).unwrap();
        assert_eq!(results[0].outcome, TestOutcome::Skipped);
    }

    #[test]
    fn junit_testcase_without_classname_uses_name_alone() {
        let xml = r#"<testsuite><testcase name="bare_name"/></testsuite>"#;
        let results = parse_junit_xml(xml).unwrap();
        assert_eq!(results[0].name, "bare_name");
    }

    #[test]
    fn junit_nested_testsuites_are_all_collected() {
        let xml = r#"<testsuites>
  <testsuite name="a">
    <testcase classname="mod1" name="t1"/>
  </testsuite>
  <testsuite name="b">
    <testcase classname="mod2" name="t2">
      <failure/>
    </testcase>
  </testsuite>
</testsuites>"#;
        let results = parse_junit_xml(xml).unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].outcome, TestOutcome::Pass);
        assert_eq!(results[1].outcome, TestOutcome::Fail);
    }

    // -- stable_id_to_test_name_prefix (moved verbatim from docs_query.rs) --

    #[test]
    fn stable_id_prefix_lowercases_and_joins_with_underscores() {
        assert_eq!(
            stable_id_to_test_name_prefix("C01-2.1.1.1"),
            "test_c01_2_1_1_1"
        );
    }

    // -- match_item_test (3-stage priority) --

    #[test]
    fn exact_test_attr_match_wins_stage_1() {
        let attrs = vec!["tests::lock::lock_after_5".to_string()];
        assert_eq!(
            match_item_test(&attrs, "REQ-003", "tests::lock::lock_after_5"),
            Some(MatchKind::ExactTestAttr)
        );
    }

    #[test]
    fn suffix_boundary_match_wins_stage_2() {
        let attrs = vec!["lock::lock_after_5".to_string()];
        assert_eq!(
            match_item_test(&attrs, "REQ-003", "tests::e2e::lock::lock_after_5"),
            Some(MatchKind::SuffixTestAttr)
        );
    }

    #[test]
    fn suffix_match_requires_a_segment_boundary_not_a_raw_substring() {
        // "ck::lock_after_5" is a raw substring of the test name but not a
        // `::`-segment suffix (it would have to split "lock" mid-segment).
        let attrs = vec!["ck::lock_after_5".to_string()];
        assert_eq!(
            match_item_test(&attrs, "REQ-003", "tests::e2e::lock::lock_after_5"),
            None
        );
    }

    #[test]
    fn legacy_prefix_matches_when_no_test_attr_present() {
        assert_eq!(
            match_item_test(&[], "C01-2.1.1.1", "test_c01_2_1_1_1_extra"),
            Some(MatchKind::LegacyPrefix)
        );
    }

    #[test]
    fn legacy_prefix_also_applies_alongside_a_present_but_non_matching_test_attr() {
        let attrs = vec!["some::other::test".to_string()];
        assert_eq!(
            match_item_test(&attrs, "C01-2.1.1.1", "test_c01_2_1_1_1"),
            Some(MatchKind::LegacyPrefix)
        );
    }

    #[test]
    fn no_stage_matches_returns_none() {
        assert_eq!(
            match_item_test(&["foo::bar".to_string()], "REQ-999", "unrelated::test"),
            None
        );
    }

    // -- aggregate_outcomes --

    #[test]
    fn aggregate_any_fail_wins() {
        assert_eq!(
            aggregate_outcomes(&[TestOutcome::Pass, TestOutcome::Fail, TestOutcome::Skipped]),
            Some(TestOutcome::Fail)
        );
    }

    #[test]
    fn aggregate_all_skipped_is_skipped() {
        assert_eq!(
            aggregate_outcomes(&[TestOutcome::Skipped, TestOutcome::Skipped]),
            Some(TestOutcome::Skipped)
        );
    }

    #[test]
    fn aggregate_pass_and_skipped_with_no_fail_is_pass() {
        assert_eq!(
            aggregate_outcomes(&[TestOutcome::Skipped, TestOutcome::Pass]),
            Some(TestOutcome::Pass)
        );
    }

    #[test]
    fn aggregate_empty_is_none() {
        assert_eq!(aggregate_outcomes(&[]), None);
    }
}

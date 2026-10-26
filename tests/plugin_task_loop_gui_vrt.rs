//! Guards for the `gui-vrt` skill's MCP-driven `verify` / `ai-compare` flow
//! (REQ-GUI-VRT-001, REQ-GUI-VRT-002) and the `gui-visual-verifier` agent's
//! hand-off to it.
//!
//! The skill and the agent are Markdown, so the "implementation" is a contract
//! between prose and the Rust schema (`VrtEvidence`, `handoff_trace_test_run`).
//! These tests fail when the prose drifts from that contract, or when the
//! existing `capture-golden` / `compare` modes are disturbed.

use std::fs;
use std::path::{Path, PathBuf};

fn loop_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop")
}

fn read(rel: &str) -> String {
    fs::read_to_string(loop_dir().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

fn skill() -> String {
    read("skills/gui-vrt/SKILL.md")
}

fn agent() -> String {
    read("agents/gui-visual-verifier.md")
}

/// Text of the Markdown section whose heading line (any depth) contains
/// `needle`, up to the next heading of the same or a shallower depth.
fn section<'a>(text: &'a str, needle: &str) -> &'a str {
    let depth = |l: &str| l.chars().take_while(|c| *c == '#').count();
    let mut offset = 0usize;
    let mut found: Option<(usize, usize)> = None;
    for line in text.lines() {
        let d = depth(line);
        if d > 0 && line.as_bytes().get(d) == Some(&b' ') {
            if let Some((start, sd)) = found {
                if d <= sd {
                    return &text[start..offset];
                }
            } else if line.contains(needle) {
                found = Some((offset, d));
            }
        }
        offset += line.len() + 1;
    }
    let (start, _) = found.unwrap_or_else(|| panic!("no heading containing {needle:?}"));
    &text[start..]
}

fn pos(haystack: &str, needle: &str) -> usize {
    haystack
        .find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?}"))
}

#[test]
fn skill_frontmatter_is_intact() {
    let text = skill();
    let rest = text.strip_prefix("---\n").expect("frontmatter");
    let end = rest.find("\n---\n").expect("frontmatter closed");
    let fm = &rest[..end];
    assert!(fm.contains("name: gui-vrt"));
    assert!(fm.contains("description:"));
}

#[test]
fn verify_mode_runs_on_the_mcp_tool_chain_in_order() {
    let text = skill();
    let verify = section(&text, "Mode `verify`");

    // AC1/AC2/AC3: requirement extraction -> campaign record -> trace record.
    let list = pos(verify, "handoff_doc_req_list");
    let run = pos(verify, "handoff_trace_test_run");
    let record = pos(verify, "handoff_trace_record");
    assert!(
        list < run && run < record,
        "verify must call doc_req_list, then trace_test_run, then trace_record"
    );

    // mock-carrying requirements are selected by their parsed attributes.
    for attr in ["mock", "golden", "viewport", "capture_method"] {
        assert!(
            verify.contains(&format!("`{attr}`")),
            "verify must read the `{attr}` attribute from doc_req_list"
        );
    }
    // doc_req_list has no mock filter and is paginated: the skill must say so.
    let finding = section(&text, "Finding the requirements that have a mock");
    assert!(finding.contains("offset"), "must page through doc_req_list");
    assert!(finding.contains("total"), "must stop at total");
}

#[test]
fn verify_mode_no_longer_scans_documents_by_hand() {
    let text = skill();
    // Hand-grep of `- mock:` lines and the "future MCP extension" caveat were
    // the pre-P2 workaround; the parser now ships.
    assert!(!text.contains("Current limitation"));
    assert!(!text.contains("Until it does"));
    assert!(!text.contains("Grep  pattern: '^- mock:'"));
}

#[test]
fn verify_mode_resolves_capture_method_from_config_or_attribute() {
    let text = skill();
    let verify = section(&text, "Mode `verify`");
    assert!(verify.contains("default_capture_method"));
    assert!(verify.contains("[gui.capture_methods."));
    assert!(verify.contains("Playwright"));
}

#[test]
fn vrt_evidence_keys_match_the_server_schema_and_use_percent_units() {
    let rust =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/storage/test_runs.rs"))
            .unwrap();
    let text = skill();
    let recording = section(&text, "Recording results");

    // Every VrtEvidence field the skill names must exist in the Rust struct.
    for key in [
        "baseline_path",
        "actual_path",
        "diff_path",
        "similarity_pct",
        "threshold",
        "viewport",
        "tool",
    ] {
        assert!(
            rust.contains(&format!("pub {key}: Option<")),
            "VrtEvidence lost field {key}"
        );
        assert!(
            recording.contains(key),
            "recording section must show vrt.{key}"
        );
    }
    // vrt-compare.mjs prints a 0..1 ratio, the vrt object wants 0..100.
    assert!(recording.contains("* 100") || recording.contains("x 100"));
    assert!(recording.contains("min_similarity"));
    // The three evidence types the server renders verdicts for.
    for ty in ["golden", "vrt-actual", "vrt-diff"] {
        assert!(recording.contains(ty), "evidence type {ty} missing");
    }
}

#[test]
fn campaign_is_created_when_none_is_open() {
    let text = skill();
    let recording = section(&text, "Recording results");
    assert!(recording.contains(r#"action: "create""#));
    assert!(recording.contains("scope"));
    assert!(recording.contains("record_check"));
    assert!(recording.contains("test_run_id"));
}

#[test]
fn ai_compare_records_verdict_confidence_reason_and_both_images() {
    let text = skill();
    let ai = section(&text, "Mode `ai-compare`");
    // AC5/AC6/AC7
    assert!(ai.contains("trace_test_run"));
    assert!(ai.contains("record_check"));
    assert!(ai.contains("confidence"));
    assert!(ai.contains("reason"));
    assert!(ai.contains("vrt-actual"));
    assert!(ai.contains("baseline_path"));
    assert!(ai.contains("actual_path"));
    assert!(ai.contains("gui-visual-verifier"));
    // low confidence / ERROR must still not be recorded as pass.
    assert!(ai.contains("`low`"));
    assert!(ai.contains("ERROR"));
}

#[test]
fn existing_modes_are_untouched() {
    let text = skill();
    for heading in [
        "## 3. Mode `capture-golden`",
        "## 4. Mode `verify`",
        "## 5. Mode `compare`",
        "## 6. Mode `ai-compare`",
        "## 7. Rules",
    ] {
        assert!(text.contains(heading), "heading {heading:?} missing");
    }

    let golden = section(&text, "Mode `capture-golden`");
    assert!(golden.contains("Approval gate"));
    assert!(golden.contains("overwrite an existing version directory"));
    assert!(golden.contains("do **not**\n   call `trace_record` here"));
    assert!(golden.contains("meta.json"));
    // capture-golden must not be pulled onto the verify tool chain.
    assert!(!golden.contains("handoff_doc_req_list"));
    assert!(!golden.contains("record_check"));

    let compare = section(&text, "Mode `compare`");
    assert!(compare.contains("Do **not** record to the trace graph unless"));
    assert!(compare.contains("diff"));

    let rules = section(&text, "Rules");
    assert!(rules.contains("Never promote a golden without explicit user approval"));
    assert!(rules.contains("Do not weaken `min_similarity`"));
}

#[test]
fn agent_stays_read_only_and_emits_a_record_ready_payload() {
    let text = agent();
    // Still read-only: the hard rule and the forbidden-tool list survive.
    assert!(text.contains("Read-only (hard rule)"));
    for tool in ["handoff_trace_record", "handoff_trace_test_run"] {
        assert!(text.contains(tool), "{tool} must be named");
    }
    let rules = section(&text, "Read-only");
    assert!(rules.contains("Never call state-modifying handoff tools"));

    // REQ-GUI-VRT-002: verdict + confidence + reason + both image paths in a
    // shape the caller forwards verbatim.
    let record = section(&text, "Recording payload");
    for needle in [
        "result",
        "confidence",
        "reason",
        "note",
        "vrt-actual",
        "baseline_path",
        "actual_path",
        "tool",
        "gui-visual-verifier",
        "record_check",
    ] {
        assert!(record.contains(needle), "payload section lacks {needle:?}");
    }
    // ERROR maps to no payload; low confidence is flagged, not auto-passed.
    assert!(record.contains("ERROR"));
    assert!(record.contains("low"));
    // The caller (not the agent) converts to project-relative paths.
    assert!(record.contains("project-relative"));
}

#[test]
fn agent_output_schema_carries_the_payload() {
    let text = agent();
    assert!(
        text.contains(r#""record""#),
        "JSON output must include `record`"
    );
    assert!(
        text.contains("### Record"),
        "markdown return must include Record"
    );
}

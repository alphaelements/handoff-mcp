//! Guards for the brainstorm coordinator command and workflow skill shipped in
//! `plugin-task-loop/` (`commands/brainstorm-loop.md`,
//! `skills/brainstorm-execute/SKILL.md`, `workflows/brainstorm-execute.js`).
//!
//! The three files form one contract: the coordinator invokes the skill, the
//! skill documents the args of the workflow script. These tests fail when one
//! side drifts from the others.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn loop_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop")
}

fn read(rel: &str) -> String {
    fs::read_to_string(loop_dir().join(rel)).unwrap_or_else(|e| panic!("{rel}: {e}"))
}

/// Parse the leading `---` frontmatter block into key/value pairs (values may
/// be single- or double-quoted).
fn frontmatter(text: &str) -> BTreeMap<String, String> {
    let rest = text
        .strip_prefix("---\n")
        .expect("frontmatter must start the file");
    let end = rest.find("\n---\n").expect("frontmatter must be closed");
    let mut map = BTreeMap::new();
    for line in rest[..end].lines() {
        let (k, v) = line
            .split_once(':')
            .expect("frontmatter line must be key: value");
        let v = v.trim();
        let v = v
            .strip_prefix('\'')
            .and_then(|s| s.strip_suffix('\''))
            .or_else(|| v.strip_prefix('"').and_then(|s| s.strip_suffix('"')))
            .unwrap_or(v);
        map.insert(k.trim().to_string(), v.to_string());
    }
    map
}

#[test]
fn every_command_has_description_frontmatter() {
    let mut problems = Vec::new();
    for entry in fs::read_dir(loop_dir().join("commands")).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "md") {
            continue;
        }
        let name = path.file_name().unwrap().to_string_lossy().to_string();
        let text = fs::read_to_string(&path).unwrap();
        let Some(rest) = text.strip_prefix("---\n") else {
            problems.push(format!("{name}: no frontmatter"));
            continue;
        };
        if !rest.contains("\ndescription:") && !rest.starts_with("description:") {
            problems.push(format!("{name}: no description"));
        }
    }
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn brainstorm_loop_command_has_frontmatter_and_covers_every_phase() {
    let text = read("commands/brainstorm-loop.md");
    let fm = frontmatter(&text);
    assert!(fm.get("description").is_some_and(|d| !d.is_empty()));
    assert!(fm.get("argument-hint").is_some_and(|d| d.contains("topic")));

    // The four MCP tools the flow runs on, and the phases that use them.
    for tool in [
        "handoff_brainstorm_start",
        "handoff_brainstorm_turn",
        "handoff_brainstorm_status",
        "handoff_brainstorm_finalize",
    ] {
        assert!(text.contains(tool), "brainstorm-loop.md must use {tool}");
    }
    for phase in ["Phase 0", "Phase 1", "Phase 2", "Phase 3", "Phase 4"] {
        assert!(
            text.contains(phase),
            "brainstorm-loop.md must describe {phase}"
        );
    }
    // Required coverage topics.
    for topic in [
        "vision",
        "motivation",
        "target-users",
        "core-value",
        "scope",
    ] {
        assert!(text.contains(topic), "required topic {topic} must be named");
    }
    // Phase 2 goes through the skill, delegate mode exists, and the dialogue
    // consults project memory for the user's profile.
    assert!(text.contains("Skill(handoff-task-loop:brainstorm-execute)"));
    assert!(text.contains("required_covered"));
    assert!(text.contains("handoff_memory_query"));
    assert!(text.contains("Delegate mode"));
}

#[test]
fn brainstorm_execute_skill_has_frontmatter_matching_its_directory() {
    let text = read("skills/brainstorm-execute/SKILL.md");
    let fm = frontmatter(&text);
    assert_eq!(
        fm.get("name").map(String::as_str),
        Some("brainstorm-execute")
    );
    assert!(fm.get("description").is_some_and(|d| !d.is_empty()));
}

#[test]
fn brainstorm_execute_skill_launches_the_predefined_workflow() {
    let text = read("skills/brainstorm-execute/SKILL.md");
    assert!(text.contains("name: 'handoff-task-loop:brainstorm-execute'"));
    assert!(
        loop_dir().join("workflows/brainstorm-execute.js").is_file(),
        "the workflow the skill launches must exist"
    );
    // The agents the workflow dispatches to must exist as agent definitions.
    let script = read("workflows/brainstorm-execute.js");
    for agent in [
        "research-investigator",
        "brainstorm-regulatory-director",
        "brainstorm-regulatory-investigator",
        "brainstorm-adversarial-reviewer",
    ] {
        assert!(
            script.contains(&format!("'handoff-task-loop:{agent}'")),
            "{agent} used by workflow"
        );
        assert!(
            loop_dir().join(format!("agents/{agent}.md")).is_file(),
            "agent definition {agent}.md must exist"
        );
    }
}

#[test]
fn brainstorm_execute_skill_documents_every_workflow_arg_and_result_field() {
    let skill = read("skills/brainstorm-execute/SKILL.md");
    let script = read("workflows/brainstorm-execute.js");

    // `const { a, b, ... } = _args;` — the args the script reads.
    let start = script.find("const {\n").expect("args destructuring");
    let end = start
        + script[start..]
            .find("} = _args;")
            .expect("end of destructuring");
    let args: Vec<&str> = script[start + "const {".len()..end]
        .split(',')
        .map(str::trim)
        .filter(|a| !a.is_empty())
        .collect();
    assert!(args.len() >= 8, "parsed args: {args:?}");
    for arg in &args {
        assert!(
            skill.contains(arg),
            "SKILL.md must document workflow arg `{arg}`"
        );
    }

    for field in [
        "stream_a",
        "stream_b",
        "review",
        "warnings",
        "research_log",
        "failed_facets",
        "failed_areas",
    ] {
        assert!(
            skill.contains(field),
            "SKILL.md must document result field `{field}`"
        );
    }
    for verdict in ["PROCEED_WITH_CONDITIONS", "RECONSIDER"] {
        assert!(
            skill.contains(verdict),
            "SKILL.md must document review verdict {verdict}"
        );
    }
}

#[test]
fn skill_and_command_agree_on_report_section_layout() {
    // Research results land under "## 6. 調査結果" (preserved by finalize).
    let skill = read("skills/brainstorm-execute/SKILL.md");
    let command = read("commands/brainstorm-loop.md");
    for text in [&skill, &command] {
        assert!(
            text.contains("6.1") && text.contains("6.4"),
            "6.1-6.4 sections referenced"
        );
    }
}

// -- REQ-SL-008: brainstorm-loop -> spec-loop hand-off ----------------------

#[test]
fn brainstorm_loop_recommends_spec_loop_and_keeps_direct_requirement_loop_path() {
    let text = read("commands/brainstorm-loop.md");
    // The recommended flow and the direct path are both documented.
    assert!(
        text.contains("brainstorm -> spec-loop -> requirement-loop"),
        "recommended flow must be spelled out"
    );
    assert!(
        text.contains("brainstorm -> requirement-loop"),
        "direct path must be kept"
    );
    // Phase 4 (user message) and the closing handoff note both point at /spec-loop.
    let phase4 = &text[text.find("### Phase 4").expect("Phase 4 heading")..];
    let rules = phase4.find("## Rules").expect("Rules heading");
    let phase4 = &phase4[..rules];
    assert!(
        phase4.contains("Next: /spec-loop"),
        "finalize note: {phase4}"
    );
    assert!(phase4.contains("optional"), "spec-loop must be optional");
    // The requirement-loop hand-off survives, in the user message and the note.
    assert!(phase4.matches("/requirement-loop").count() >= 3, "{phase4}");
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/spec-loop")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// The `#### BS-<kind>-NNN <text>` ids a brainstorm report carries.
fn bs_ids(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|l| l.strip_prefix("#### "))
        .filter(|l| l.starts_with("BS-"))
        .map(|l| l.split_whitespace().next().unwrap().to_string())
        .collect()
}

fn scope_ids(text: &str) -> Vec<String> {
    bs_ids(text)
        .into_iter()
        .filter(|id| id.starts_with("BS-SCOPE-"))
        .collect()
}

#[test]
fn spec_loop_fixtures_gui_report_has_screens_flow_and_ux_policy() {
    let text = fixture("gui_project_brainstorm.md");
    assert!(
        (3..=5).contains(&scope_ids(&text).len()),
        "{:?}",
        bs_ids(&text)
    );
    for heading in [
        "## 8. GUI 設計概要",
        "### 8.1 画面一覧",
        "### 8.2 画面遷移図",
        "### 8.3 UX 方針",
        "## 9. 優先順位",
        "## 10. 要件定義への申し送り",
    ] {
        assert!(text.contains(heading), "missing {heading}");
    }
    let screens = text
        .lines()
        .filter(|l| l.starts_with('|') && !l.contains("---") && !l.contains("名前"))
        .count();
    assert!((3..=4).contains(&screens), "3-4 screen rows, got {screens}");
    assert!(bs_ids(&text).iter().any(|i| i.starts_with("BS-PRIORITY-")));
    assert!(bs_ids(&text).iter().any(|i| i.starts_with("BS-HINT-")));
}

#[test]
fn spec_loop_fixtures_cli_report_has_no_gui_and_has_cli_hints() {
    let text = fixture("cli_project_brainstorm.md");
    assert!(
        (3..=5).contains(&scope_ids(&text).len()),
        "{:?}",
        bs_ids(&text)
    );
    for absent in ["GUI 設計概要", "画面一覧", "画面遷移図", "UX 方針"] {
        assert!(
            !text.contains(absent),
            "CLI report must not contain {absent}"
        );
    }
    // Non-GUI numbering: priorities are section 8, hand-off section 9.
    assert!(text.contains("## 8. 優先順位") && text.contains("## 9. 要件定義への申し送り"));
    assert!(text.contains("--"), "command-line options in the hints");
    assert!(bs_ids(&text).iter().any(|i| i.starts_with("BS-HINT-")));
}

#[test]
fn spec_loop_fixtures_minimal_report_has_only_scope_items() {
    let text = fixture("minimal_brainstorm.md");
    assert!(!scope_ids(&text).is_empty());
    assert_eq!(scope_ids(&text), bs_ids(&text), "scope items only");
    assert!(!text.contains("GUI 設計概要"));
}

#[test]
fn spec_loop_fixtures_have_unique_well_formed_ids() {
    for name in [
        "gui_project_brainstorm.md",
        "cli_project_brainstorm.md",
        "minimal_brainstorm.md",
    ] {
        let text = fixture(name);
        assert!(text.starts_with("# ブレストレポート: "), "{name}");
        let ids = bs_ids(&text);
        let mut dedup = ids.clone();
        dedup.sort();
        dedup.dedup();
        assert_eq!(ids.len(), dedup.len(), "{name}: duplicate ids");
        for id in &ids {
            let n = id.rsplit('-').next().unwrap();
            assert!(
                n.len() == 3 && n.chars().all(|c| c.is_ascii_digit()),
                "{name}: {id}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// spec-loop (`commands/spec-loop.md` + `skills/spec-execute/SKILL.md`)
// ---------------------------------------------------------------------------

#[test]
fn spec_loop_command_has_frontmatter_and_covers_every_phase() {
    let text = read("commands/spec-loop.md");
    let fm = frontmatter(&text);
    assert!(fm.get("description").is_some_and(|d| !d.is_empty()));
    assert!(fm.contains_key("argument-hint"));
    for phase in ["Phase 0", "Phase 1", "Phase 2", "Phase 3", "Phase 4"] {
        assert!(
            text.contains(&format!("### {phase}")),
            "spec-loop.md must have a `### {phase}` section"
        );
    }
    // The coordinator hands the heavy lifting to the skill.
    assert!(text.contains("Skill(handoff-task-loop:spec-execute)"));
}

#[test]
fn spec_loop_defines_trigger_words() {
    // REQ-SL-001 AC2: the triggers appear in the skill description (what the
    // harness matches on) and in the command text.
    let skill = read("skills/spec-execute/SKILL.md");
    let command = read("commands/spec-loop.md");
    let description = frontmatter(&skill)
        .get("description")
        .cloned()
        .unwrap_or_default();
    for trigger in [
        "spec-loop",
        "要求仕様",
        "specification",
        "仕様策定",
        "spec phase",
    ] {
        assert!(
            description.contains(trigger),
            "skill description must carry trigger `{trigger}`"
        );
        assert!(
            command.contains(trigger),
            "spec-loop.md must list trigger `{trigger}`"
        );
    }
}

#[test]
fn spec_execute_skill_has_frontmatter_matching_its_directory() {
    let text = read("skills/spec-execute/SKILL.md");
    let fm = frontmatter(&text);
    assert_eq!(fm.get("name").map(String::as_str), Some("spec-execute"));
    assert!(fm.get("description").is_some_and(|d| !d.is_empty()));
    for phase in ["Phase 1", "Phase 2", "Phase 3", "Phase 4"] {
        assert!(
            text.contains(phase),
            "spec-execute SKILL.md must describe {phase}"
        );
    }
}

#[test]
fn spec_loop_phase0_imports_the_brainstorm_report() {
    // REQ-SL-002: automatic import, approval gate, manual fallback.
    let text = read("commands/spec-loop.md");
    for needle in [
        "handoff_doc_query",
        "brainstorm-report",
        "BS-SCOPE",
        "BS-PRIORITY",
        "BS-HINT",
        "screens",
        "screen_flow",
        "ux_policy",
        "AskUserQuestion",
        "manual",
    ] {
        assert!(
            text.contains(needle),
            "spec-loop.md Phase 0 must mention `{needle}`"
        );
    }
}

#[test]
fn spec_loop_phase4_saves_exports_and_gates_on_approval() {
    // REQ-SL-006: the final spec is saved, exported to PDF, and gated.
    let text = read("commands/spec-loop.md");
    for needle in [
        "handoff_doc_save",
        "handoff_doc_export",
        "Approve",
        "Revise",
        "Suspend",
        "Stop",
        "/requirement-loop",
    ] {
        assert!(
            text.contains(needle),
            "spec-loop.md Phase 4 must mention `{needle}`"
        );
    }
}

#[test]
fn spec_loop_picks_mermaid_diagram_types_per_phase() {
    // REQ-SL-010 AC2: the diagram type is chosen per phase.
    let skill = read("skills/spec-execute/SKILL.md");
    for needle in [
        "flowchart",
        "C4Context",
        "stateDiagram-v2",
        "sequenceDiagram",
    ] {
        assert!(
            skill.contains(needle),
            "spec-execute SKILL.md must name Mermaid type `{needle}`"
        );
    }
}

// -- REQ-SL-009: spec-loop -> requirement-loop connection --------------------

/// The `### Phase 0` section of requirement-loop.md (up to `### Phase 1`).
fn requirement_loop_phase0() -> String {
    let text = read("commands/requirement-loop.md");
    let start = text.find("### Phase 0").expect("Phase 0 heading");
    let end = text.find("### Phase 1").expect("Phase 1 heading");
    text[start..end].to_string()
}

#[test]
fn requirement_loop_phase0_loads_the_approved_spec_loop_output() {
    // REQ-SL-009 AC1/AC2: Phase 0 finds and imports the approved specification.
    let phase0 = requirement_loop_phase0();
    for needle in [
        "handoff_doc_list(tags=[\"spec-loop\", \"requirements-spec\"])",
        "approved",
        "handoff_doc_get",
        "architecture",
        "screens",
        "use cases",
        "AskUserQuestion",
    ] {
        assert!(
            phase0.contains(needle),
            "requirement-loop.md Phase 0 must mention `{needle}`"
        );
    }
}

#[test]
fn requirement_loop_phase0_keeps_the_paths_that_skip_spec_loop() {
    // REQ-SL-009 AC3: brainstorm -> requirement-loop and manual input still work.
    let phase0 = requirement_loop_phase0();
    // The brainstorm-report path is intact.
    for needle in [
        "brainstorm-report",
        "brainstorm_finalize",
        "handoff_doc_list(query=\"brainstorm\")",
        "No brainstorm report exists: continue as usual.",
        "GUI project flow",
    ] {
        assert!(
            phase0.contains(needle),
            "requirement-loop.md Phase 0 must still mention `{needle}`"
        );
    }
    // The spec-loop step is optional: absence falls through to the old paths.
    assert!(
        phase0.contains("No approved specification"),
        "Phase 0 must say what happens when no spec exists"
    );
    // Phase 0 still asks the user for the one-sentence purpose and actors.
    assert!(phase0.contains("one sentence"), "{phase0}");
    // The profile / gate structure of Phase 0 is untouched.
    for needle in ["Propose a profile", "Gate #0", "handoff_update_config"] {
        assert!(phase0.contains(needle), "Phase 0 lost `{needle}`");
    }
}

#[test]
fn requirement_loop_spec_import_matches_what_spec_loop_writes() {
    // The tags the consumer searches for are exactly the ones the producer saves.
    let producer = read("commands/spec-loop.md");
    let consumer = requirement_loop_phase0();
    for tag in ["spec-loop", "requirements-spec", "approved"] {
        assert!(
            producer.contains(tag),
            "spec-loop.md must write tag `{tag}`"
        );
        assert!(
            consumer.contains(tag),
            "requirement-loop.md must read tag `{tag}`"
        );
    }
}

// ---------------------------------------------------------------------------
// spec-loop Phase 1 / Phase 2 detail (REQ-SL-003, REQ-SL-004, REQ-SL-010)
// ---------------------------------------------------------------------------

/// Text of the `## <n>. ...` section of the spec-execute skill that starts
/// with `heading_prefix`, up to the next `## ` heading.
fn skill_section(heading_prefix: &str) -> String {
    let text = read("skills/spec-execute/SKILL.md");
    let start = text
        .find(&format!("\n## {heading_prefix}"))
        .unwrap_or_else(|| panic!("SKILL.md has no `## {heading_prefix}` section"));
    let body = &text[start + 1..];
    let end = body[3..].find("\n## ").map_or(body.len(), |i| i + 3);
    body[..end].to_string()
}

/// Contents of every fenced ```` ```mermaid ```` block in `text`.
fn mermaid_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        match (&mut current, line.trim_end()) {
            (None, l) if l.trim_start() == "```mermaid" => current = Some(String::new()),
            (Some(buf), l) if l.trim_start() == "```" => {
                blocks.push(std::mem::take(buf));
                current = None;
            }
            (Some(buf), _) => {
                buf.push_str(line);
                buf.push('\n');
            }
            (None, _) => {}
        }
    }
    blocks
}

#[test]
fn spec_execute_phase1_details_architecture_generation_and_gate() {
    // REQ-SL-003 AC1-AC3.
    let section = skill_section("2. Phase 1");
    for needle in [
        "BS-SCOPE",
        "subgraph",
        "C4Context",
        "flowchart TD",
        "external interface",
        "data flow",
        "AskUserQuestion",
        "handoff_doc_save",
        "doc_type=\"design\"",
        "references/diagram-templates.md",
        "regenerate",
        "mmdc",
    ] {
        assert!(
            section.contains(needle),
            "SKILL.md Phase 1 section must mention `{needle}`"
        );
    }
}

#[test]
fn spec_execute_phase2_details_mockups_transitions_and_skip() {
    // REQ-SL-004 AC1-AC4.
    let section = skill_section("3. Phase 2");
    for needle in [
        "screens",
        "screen_flow",
        "ux_policy",
        "stateDiagram-v2",
        "inline CSS",
        "AskUserQuestion",
        "handoff_doc_save",
        "Phase 2 skipped: no GUI",
        "references/mock-guidelines.md",
        "references/diagram-templates.md",
        "skip",
    ] {
        assert!(
            section.contains(needle),
            "SKILL.md Phase 2 section must mention `{needle}`"
        );
    }
}

#[test]
fn spec_execute_references_exist_and_are_linked_from_the_skill() {
    for rel in [
        "skills/spec-execute/references/diagram-templates.md",
        "skills/spec-execute/references/mock-guidelines.md",
    ] {
        assert!(loop_dir().join(rel).is_file(), "{rel} must exist");
    }
    let skill = read("skills/spec-execute/SKILL.md");
    for linked in [
        "references/diagram-templates.md",
        "references/mock-guidelines.md",
    ] {
        assert!(
            skill.contains(linked),
            "spec-execute SKILL.md must link `{linked}`"
        );
    }
}

#[test]
fn spec_execute_diagram_templates_cover_each_phase_type() {
    // REQ-SL-010 AC1/AC2: a template per phase-specific diagram type.
    let text = read("skills/spec-execute/references/diagram-templates.md");
    let blocks = mermaid_blocks(&text);
    for first_line in [
        "flowchart TD",
        "C4Context",
        "stateDiagram-v2",
        "sequenceDiagram",
    ] {
        assert!(
            blocks
                .iter()
                .any(|b| b.lines().next().map(str::trim) == Some(first_line)),
            "diagram-templates.md needs a ```mermaid block starting with `{first_line}`"
        );
    }
    // Japanese labels must be shown to work (htmlLabels:false is set by the
    // PDF pipeline, not by the diagram).
    assert!(
        blocks
            .iter()
            .any(|b| b.chars().any(|c| ('\u{3040}'..='\u{9fff}').contains(&c))),
        "diagram-templates.md should contain a Japanese-labelled example"
    );
}

#[test]
fn spec_execute_mock_guidelines_cover_self_contained_html() {
    let text = read("skills/spec-execute/references/mock-guidelines.md");
    for needle in [
        "<!DOCTYPE html>",
        "<style>",
        "lang=",
        "no framework",
        "href=",
        "escape",
    ] {
        assert!(
            text.contains(needle),
            "mock-guidelines.md must mention `{needle}`"
        );
    }
}

#[test]
fn spec_execute_diagram_templates_render_with_mmdc_when_installed() {
    // REQ-SL-010 AC3: every template is syntactically valid Mermaid. Skipped
    // (loudly) when no usable mmdc is on the machine.
    let bin = std::env::var("HANDOFF_MMDC_BIN").unwrap_or_else(|_| "mmdc".to_string());
    let dir = std::env::temp_dir().join(format!("spec-templates-{}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let config = dir.join("config.json");
    // Same switch the PDF pipeline applies (mermaid.rs MMDC_CONFIG_JSON).
    fs::write(
        &config,
        r#"{"htmlLabels":false,"flowchart":{"htmlLabels":false},"class":{"htmlLabels":false}}"#,
    )
    .unwrap();
    let probe = dir.join("probe.mmd");
    fs::write(&probe, "flowchart TD\n  A --> B\n").unwrap();
    let run = |input: &Path, output: &Path| {
        std::process::Command::new(&bin)
            .args(["-q", "-i"])
            .arg(input)
            .arg("-o")
            .arg(output)
            .arg("-c")
            .arg(&config)
            .output()
    };
    if !matches!(run(&probe, &dir.join("probe.svg")), Ok(o) if o.status.success()) {
        eprintln!("skipping: mmdc unavailable or unusable");
        let _ = fs::remove_dir_all(&dir);
        return;
    }
    let text = read("skills/spec-execute/references/diagram-templates.md");
    for (i, block) in mermaid_blocks(&text).iter().enumerate() {
        let input = dir.join(format!("t{i}.mmd"));
        fs::write(&input, block).unwrap();
        let out = run(&input, &dir.join(format!("t{i}.svg"))).unwrap();
        assert!(
            out.status.success(),
            "template block {i} failed mmdc:\n{block}\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

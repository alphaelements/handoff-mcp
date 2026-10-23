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

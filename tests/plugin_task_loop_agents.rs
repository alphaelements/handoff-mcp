//! Guards for the agent definitions shipped in `plugin-task-loop/agents/`.
//!
//! Every agent file must carry well-formed frontmatter (name matching the file
//! stem, a description, a known model, an effort, a tools list), and the
//! brainstorm agents must exist with the model/tool contract the brainstorm
//! workflow relies on.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn agents_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop/agents")
}

/// Parse the leading `---` frontmatter block into key/value pairs.
fn frontmatter(text: &str) -> Option<BTreeMap<String, String>> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let mut map = BTreeMap::new();
    for line in rest[..end].lines() {
        let (k, v) = line.split_once(':')?;
        map.insert(k.trim().to_string(), v.trim().to_string());
    }
    Some(map)
}

/// All agent definitions (underscore-prefixed files are shared protocol
/// fragments, not agents).
fn agent_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(agents_dir()).expect("agents dir") {
        let path = entry.unwrap().path();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        if path.extension().is_some_and(|e| e == "md") && !stem.starts_with('_') {
            out.push((stem, fs::read_to_string(&path).unwrap()));
        }
    }
    out.sort();
    out
}

#[test]
fn every_agent_has_well_formed_frontmatter() {
    let mut problems = Vec::new();
    for (stem, text) in agent_files() {
        let Some(fm) = frontmatter(&text) else {
            problems.push(format!("{stem}: missing or malformed frontmatter"));
            continue;
        };
        if fm.get("name") != Some(&stem) {
            problems.push(format!("{stem}: name {:?} != file stem", fm.get("name")));
        }
        if fm.get("description").is_none_or(|d| d.is_empty()) {
            problems.push(format!("{stem}: empty description"));
        }
        if !matches!(fm.get("model").map(String::as_str), Some("opus" | "sonnet")) {
            problems.push(format!("{stem}: unknown model {:?}", fm.get("model")));
        }
        if fm.get("tools").is_none_or(|v| v.is_empty()) {
            problems.push(format!("{stem}: missing tools"));
        }
    }
    assert!(
        problems.is_empty(),
        "agent frontmatter problems:\n{}",
        problems.join("\n")
    );
}

#[test]
fn brainstorm_agents_are_registered_with_expected_contract() {
    // (name, model, tools that must be present)
    let expected: [(&str, &str, &[&str]); 3] = [
        (
            "brainstorm-regulatory-director",
            "opus",
            &["Read", "Edit", "Write", "Bash", "Grep", "Glob", "TodoWrite"],
        ),
        (
            "brainstorm-regulatory-investigator",
            "sonnet",
            &[
                "Read",
                "Bash",
                "Grep",
                "Glob",
                "TodoWrite",
                "WebSearch",
                "WebFetch",
            ],
        ),
        (
            "brainstorm-adversarial-reviewer",
            "opus",
            &["Read", "Bash", "Grep", "Glob", "TodoWrite"],
        ),
    ];
    let files: BTreeMap<_, _> = agent_files().into_iter().collect();
    for (name, model, tools) in expected {
        let text = files
            .get(name)
            .unwrap_or_else(|| panic!("{name}: agent file missing"));
        let fm = frontmatter(text).unwrap_or_else(|| panic!("{name}: bad frontmatter"));
        assert_eq!(fm["model"], model, "{name}: model");
        assert_eq!(fm["effort"], "high", "{name}: effort");
        let have: Vec<&str> = fm["tools"].split(',').map(str::trim).collect();
        for t in tools {
            assert!(have.contains(t), "{name}: tool {t} missing from {have:?}");
        }
    }
}

#[test]
fn readme_lists_brainstorm_agents() {
    let readme = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop/README.md"),
    )
    .unwrap();
    for name in [
        "brainstorm-regulatory-director",
        "brainstorm-regulatory-investigator",
        "brainstorm-adversarial-reviewer",
    ] {
        assert!(readme.contains(name), "README.md does not mention {name}");
    }
}

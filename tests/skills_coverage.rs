//! Guards for the bundled user-facing skills (`skills/`, shipped to plugin
//! users as `plugin/skills/`).
//!
//! The skills drifted behind the code once already: whole tools
//! (`trace_test_run`, `trace_baseline`, `trace_delta`, `handoff_report`) and
//! several `[trace]` config keys shipped with no skill coverage, while the
//! skills cited internal design pages users cannot read. These tests make
//! that drift fail CI instead of being discovered by a user:
//!
//! 1. every MCP tool is named somewhere in the skills,
//! 2. every `[trace]` config key is documented in the config reference,
//! 3. no skill cites internal artifacts (design-wiki pages, internal
//!    requirement/task ids, milestone names, private repos/skills),
//! 4. every skill is structurally sound (frontmatter, Codex metadata,
//!    `references/` links resolve and nothing is orphaned, size budget).

use handoff_mcp::mcp::tools::all_tool_definitions;
use handoff_mcp::storage::config::{
    RequirementGateConfig, TraceConfig, TraceLintConfig, TraceLintRequireRule,
    TraceLintRequireWhen, TraceProfileConfig,
};
use handoff_mcp::storage::docs::layer::CustomLayerConfig;
use std::fs;
use std::path::{Path, PathBuf};

/// SKILL.md is loaded whole every time the skill triggers; anything past
/// this belongs in `references/` (read on demand).
const SKILL_MD_MAX_LINES: usize = 500;

fn skills_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("skills")
}

fn skill_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = fs::read_dir(skills_dir())
        .expect("skills/ must exist")
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

/// Every Markdown file under `skills/` (SKILL.md and references), as
/// `(path relative to skills/, contents)`.
fn skill_markdown() -> Vec<(String, String)> {
    fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
        for entry in fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "md") {
                out.push(path);
            }
        }
    }
    let mut paths = Vec::new();
    walk(&skills_dir(), &mut paths);
    paths.sort();
    paths
        .into_iter()
        .map(|p| {
            let rel = p
                .strip_prefix(skills_dir())
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (rel, fs::read_to_string(&p).unwrap())
        })
        .collect()
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

/// `needle` occurs in `hay` with no identifier character on either side.
fn contains_word(hay: &str, needle: &str) -> bool {
    hay.match_indices(needle).any(|(i, _)| {
        let before = hay[..i].chars().next_back();
        let after = hay[i + needle.len()..].chars().next();
        !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
    })
}

// ---------------------------------------------------------------------------
// 1. Tool coverage
// ---------------------------------------------------------------------------

#[test]
fn every_mcp_tool_is_documented_in_a_skill() {
    let corpus: String = skill_markdown()
        .into_iter()
        .map(|(_, body)| body)
        .collect::<Vec<_>>()
        .join("\n");
    let mut missing = Vec::new();
    for tool in all_tool_definitions() {
        let full = tool.name.as_str();
        let short = full.strip_prefix("handoff_").unwrap_or(full);
        // The short form (`trace_report`) is how skills usually refer to a
        // tool, but a one-word short form (`report`, `init`) is too generic
        // to count as a mention.
        let found =
            contains_word(&corpus, full) || (short.contains('_') && contains_word(&corpus, short));
        if !found {
            missing.push(full.to_string());
        }
    }
    assert!(
        missing.is_empty(),
        "MCP tools not mentioned in any skills/**/*.md (document them, e.g. in \
         skills/handoff-trace/references/tools.md): {missing:?}"
    );
}

/// Tools that no longer exist but are named on purpose (migration tables).
const REMOVED_TOOLS: &[&str] = &["handoff_doc_req_test_sync"];

/// `handoff_`-prefixed identifiers that are not tools (session fields).
const NOT_TOOLS: &[&str] = &["handoff_notes", "handoff_note"];

/// Every identifier in `text` starting at a word boundary with `prefix`,
/// extended over identifier characters (`handoff_trace_report`).
fn identifiers_with_prefix<'a>(text: &'a str, prefix: &str) -> Vec<(&'a str, &'a str)> {
    let mut out = Vec::new();
    for (i, _) in text.match_indices(prefix) {
        if text[..i].chars().next_back().is_some_and(is_word_char) {
            continue;
        }
        let len = text[i..]
            .char_indices()
            .find(|&(_, c)| !is_word_char(c))
            .map_or(text.len() - i, |(n, _)| n);
        out.push((&text[i..i + len], &text[i + len..]));
    }
    out
}

#[test]
fn every_tool_named_in_a_skill_exists() {
    let tools: Vec<String> = all_tool_definitions().into_iter().map(|t| t.name).collect();
    let exists = |name: &str| tools.iter().any(|t| t == name) || REMOVED_TOOLS.contains(&name);
    let mut unknown = Vec::new();
    for (path, body) in skill_markdown() {
        // `handoff_<tool>` anywhere.
        for (ident, _) in identifiers_with_prefix(&body, "handoff_") {
            // `handoff_trace_*` style family prefixes end in `_`.
            if ident.ends_with('_') || NOT_TOOLS.contains(&ident) {
                continue;
            }
            if !exists(ident) {
                unknown.push(format!("{path}: {ident}"));
            }
        }
        // Short form used as a call: `trace_report(` / `doc_save(`.
        for prefix in ["trace_", "doc_"] {
            for (ident, rest) in identifiers_with_prefix(&body, prefix) {
                if rest.starts_with('(') && !exists(&format!("handoff_{ident}")) {
                    unknown.push(format!("{path}: {ident}("));
                }
            }
        }
    }
    unknown.sort();
    unknown.dedup();
    assert!(
        unknown.is_empty(),
        "skills name tools that do not exist (typo, rename, or removal — add \
         deliberate mentions of removed tools to REMOVED_TOOLS):\n{}",
        unknown.join("\n")
    );
}

// ---------------------------------------------------------------------------
// 2. `[trace]` config key coverage
// ---------------------------------------------------------------------------

/// Every `[trace]` key, as the token the config reference must contain.
///
/// The exhaustive destructuring below (no `..`) is deliberate: adding a field
/// to any of these structs stops this test from compiling until the new key
/// is added to the list — and therefore to the reference.
fn trace_config_keys() -> Vec<&'static str> {
    let TraceConfig {
        layers: _,
        id_prefixes: _,
        layer: _,
        profile: _,
        done_guard: _,
        profiles: _,
        lint: _,
        auto_layer: _,
        plan_default_layer: _,
        requirement_gate: _,
    } = TraceConfig::default();
    let RequirementGateConfig {
        fail_on: _,
        required_checks: _,
        skip_checks: _,
    } = RequirementGateConfig::default();
    let CustomLayerConfig {
        id: _,
        display_name: _,
        side: _,
        level: _,
        pair: _,
        id_prefixes: _,
        verification: _,
    } = CustomLayerConfig::default();
    let TraceProfileConfig {
        extends: _,
        layers: _,
        implicit_acceptance: _,
        max_generated_per_call: _,
        default_needs: _,
    } = TraceProfileConfig::default();
    let TraceLintConfig {
        rules: _,
        require: _,
    } = TraceLintConfig::default();
    let TraceLintRequireRule {
        id: _,
        when: _,
        need: _,
        severity: _,
    } = TraceLintRequireRule::default();
    let TraceLintRequireWhen {
        layer: _,
        priority: _,
        method: _,
        doc: _,
        approval: _,
    } = TraceLintRequireWhen::default();

    vec![
        // [trace]
        "layers",
        "id_prefixes",
        "[[trace.layer]]",
        "profile",
        "done_guard",
        "[trace.profiles.",
        "[trace.lint.rules]",
        "[[trace.lint.require]]",
        "auto_layer",
        "plan_default_layer",
        "[trace.requirement_gate]",
        // [trace.requirement_gate]
        "fail_on",
        "required_checks",
        "skip_checks",
        // [[trace.layer]]
        "display_name",
        "side",
        "level",
        "pair",
        // [trace.profiles.<name>]
        "extends",
        "implicit_acceptance",
        "max_generated_per_call",
        "default_needs",
        // [[trace.lint.require]]
        "need",
        "severity",
        "when.layer",
        "when.priority",
        "when.method",
        "when.doc",
        "when.approval",
    ]
}

#[test]
fn every_trace_config_key_is_documented() {
    let path = skills_dir().join("handoff-trace/references/config.md");
    let body =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must exist: {e}", path.display()));
    let missing: Vec<_> = trace_config_keys()
        .into_iter()
        .filter(|k| !body.contains(k))
        .collect();
    assert!(
        missing.is_empty(),
        "[trace] config keys missing from {}: {missing:?}",
        path.display()
    );
}

// ---------------------------------------------------------------------------
// 3. No internal references
// ---------------------------------------------------------------------------

/// The prose of a Markdown file: fenced code blocks and inline code spans
/// removed. Example ids (`FR-001`) live in code; citations of internal
/// requirement ids appear in prose.
fn prose(markdown: &str) -> String {
    let mut out = String::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            out.push('\n');
            continue;
        }
        if in_fence {
            out.push('\n');
            continue;
        }
        let mut in_code = false;
        for c in line.chars() {
            if c == '`' {
                in_code = !in_code;
            } else if !in_code {
                out.push(c);
            }
        }
        out.push('\n');
    }
    out
}

/// Words in `text` matching `prefix` + 1..=max_digits ASCII digits as a
/// whole word (`t377`, `M2`, `E6`), with an optional `.n`/`-n` suffix chain.
fn find_coded_words(text: &str, prefix: &str, max_digits: usize) -> Vec<String> {
    let mut hits = Vec::new();
    for (i, _) in text.match_indices(prefix) {
        if text[..i].chars().next_back().is_some_and(is_word_char) {
            continue;
        }
        let rest = &text[i + prefix.len()..];
        let digits = rest.chars().take_while(|c| c.is_ascii_digit()).count();
        if digits == 0 || digits > max_digits {
            continue;
        }
        let after = rest[digits..].chars().next();
        if after.is_some_and(|c| c.is_ascii_alphabetic() || c == '_') {
            continue;
        }
        hits.push(format!("{prefix}{}", &rest[..digits]));
    }
    hits
}

fn internal_references(markdown: &str) -> Vec<String> {
    let mut hits = Vec::new();
    // Anywhere, code included: these never make sense to a plugin user.
    for needle in [
        "wiki/0",
        "wiki/1",
        "wiki/2",
        "wiki/3",
        "wiki/4",
        "wiki/5",
        "wiki/6",
        "wiki/7",
        "wiki/8",
        "wiki/9",
        "pochi",
        "aelm-referral",
        "/design-review",
        "plugin-task-loop/",
    ] {
        if markdown.contains(needle) {
            hits.push(needle.to_string());
        }
    }
    // Prose only.
    let text = prose(markdown);
    for prefix in ["FR-", "NFR-", "SPEC-TREE-", "REQ-VGAP-", "REQ-PLA-"] {
        for (i, _) in text.match_indices(prefix) {
            let boundary = !text[..i].chars().next_back().is_some_and(is_word_char);
            let digit = text[i + prefix.len()..]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_digit());
            if boundary && digit {
                hits.push(format!("{prefix}<n> (prose)"));
            }
        }
    }
    hits.extend(
        find_coded_words(&text, "t", 3)
            .into_iter()
            .filter(|w| w.len() == 4),
    );
    // Session-style task ids (`t-vskill`, `t-abc.3`): `t-` + a lowercase
    // letter, at a word start. Mid-word hyphens (`set-up`) never qualify.
    for (i, _) in text.match_indices("t-") {
        let boundary = !text[..i].chars().next_back().is_some_and(is_word_char);
        let next = text[i + 2..].chars().next();
        if boundary && next.is_some_and(|c| c.is_ascii_lowercase()) {
            hits.push("t-<id>".to_string());
        }
    }
    hits.extend(find_coded_words(&text, "M", 1));
    hits.extend(find_coded_words(&text, "E", 2));
    hits
}

#[test]
fn skills_cite_no_internal_artifacts() {
    let offenders: Vec<_> = skill_markdown()
        .into_iter()
        .filter_map(|(path, body)| {
            let hits = internal_references(&body);
            (!hits.is_empty()).then(|| format!("{path}: {hits:?}"))
        })
        .collect();
    assert!(
        offenders.is_empty(),
        "skills cite internal artifacts plugin users cannot see (design-wiki \
         pages, internal requirement/task ids, milestone names, private \
         repos/skills):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn internal_reference_detector_catches_known_shapes() {
    assert!(internal_references("see wiki/260-vmodel").contains(&"wiki/2".to_string()));
    assert!(!internal_references("(FR-1005)").is_empty());
    assert!(!internal_references("fixed in t377.5").is_empty());
    assert!(!internal_references("removed at the M3 release").is_empty());
    assert!(!internal_references("read-only (E6)").is_empty());
    assert!(!internal_references("tracked in t-vskill.3").is_empty());
    // Example ids in code, and ordinary words, are fine.
    assert!(internal_references("```\n### FR-001 Login\n```\nuse `FR-001`").is_empty());
    assert!(internal_references("run the E2E test on M-series Macs; t1 t12345").is_empty());
    assert!(internal_references("set-up, post-mortem, `t-vskill`").is_empty());
}

// ---------------------------------------------------------------------------
// 4. Structure
// ---------------------------------------------------------------------------

fn frontmatter_field<'a>(skill_md: &'a str, key: &str) -> Option<&'a str> {
    let body = skill_md.strip_prefix("---\n")?;
    let end = body.find("\n---")?;
    body[..end].lines().find_map(|l| {
        l.strip_prefix(key)
            .and_then(|r| r.strip_prefix(':'))
            .map(|v| v.trim().trim_matches('"'))
    })
}

#[test]
fn every_skill_is_structurally_sound() {
    let mut problems = Vec::new();
    for dir in skill_dirs() {
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        let skill_md_path = dir.join("SKILL.md");
        let Ok(skill_md) = fs::read_to_string(&skill_md_path) else {
            problems.push(format!("{name}: missing SKILL.md"));
            continue;
        };

        match frontmatter_field(&skill_md, "name") {
            Some(n) if n == name => {}
            other => problems.push(format!("{name}: frontmatter name {other:?} != dir name")),
        }
        if frontmatter_field(&skill_md, "description").is_none_or(str::is_empty) {
            problems.push(format!("{name}: frontmatter description missing"));
        }
        if !dir.join("agents/openai.yaml").is_file() {
            problems.push(format!(
                "{name}: missing agents/openai.yaml (Codex metadata)"
            ));
        }
        let lines = skill_md.lines().count();
        if lines > SKILL_MD_MAX_LINES {
            problems.push(format!(
                "{name}: SKILL.md is {lines} lines (> {SKILL_MD_MAX_LINES}); move detail to references/"
            ));
        }

        // Every Markdown link `](references/<file>.md...)` in SKILL.md
        // resolves, and every file in references/ is named in SKILL.md
        // (otherwise unreachable). Plain-text mentions are not links — they
        // may name another skill's reference file.
        for (i, _) in skill_md.match_indices("](references/") {
            let rest = &skill_md[i + 2..];
            if let Some(end) = rest.find(".md") {
                let rel = &rest[..end + 3];
                if !dir.join(rel).is_file() {
                    problems.push(format!("{name}: SKILL.md links missing {rel}"));
                }
            }
        }
        if let Ok(entries) = fs::read_dir(dir.join("references")) {
            for entry in entries {
                let file = entry.unwrap().file_name().to_string_lossy().into_owned();
                if !skill_md.contains(&format!("references/{file}")) {
                    problems.push(format!(
                        "{name}: references/{file} is not linked from SKILL.md"
                    ));
                }
            }
        }
    }
    assert!(
        problems.is_empty(),
        "skill structure problems:\n{}",
        problems.join("\n")
    );
}

// ---------------------------------------------------------------------------
// 5. Scheduled report generation guidance
// ---------------------------------------------------------------------------

#[test]
fn report_schedule_skill_documents_the_cloud_scheduling_recipe() {
    let path = skills_dir().join("handoff-report-schedule/SKILL.md");
    let body =
        fs::read_to_string(&path).unwrap_or_else(|e| panic!("{} must exist: {e}", path.display()));
    let missing: Vec<_> = [
        "/schedule",
        "CronCreate",
        "0 9 1 * *",
        "0 17 * * 5",
        "cloud agent",
        "does **not** run from a plain local CLI",
        "scope.period",
        "format: \"pdf\"",
    ]
    .into_iter()
    .filter(|needle| !body.contains(needle))
    .collect();
    assert!(
        missing.is_empty(),
        "{} is missing required guidance: {missing:?}",
        path.display()
    );
}

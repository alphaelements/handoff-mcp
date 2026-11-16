//! CLAUDE.md project sections and `[commands]` generation for
//! `handoff-mcp setup` (SPEC-ONB-001).
//!
//! `[commands]` in `.handoff/config.toml` is the source of truth for the
//! project's build, test and check commands. The `Build & Test` section this
//! module writes into `CLAUDE.md` is a copy of those values for people and
//! agents to read.
//!
//! Everything here only ever adds: a section that already exists is never
//! touched, a `[commands]` key that already has a value is never overwritten,
//! and a run with nothing to add writes no file.

use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item};

use crate::storage::config::{ensure_table_like, read_config, Config, COMMAND_KEYS};

/// Name of the config.toml section the commands live in.
const COMMANDS_SECTION: &str = "commands";

/// Marks a field the user still has to fill in. `setup --check` and
/// `handoff_doctor` look for this exact string.
pub const TODO_MARKER: &str = "TODO(handoff)";

/// One generated `CLAUDE.md` section.
#[derive(Debug)]
pub(crate) struct ScaffoldSection {
    /// Heading text, compared case-insensitively against existing headings.
    pub title: &'static str,
    template: &'static str,
}

/// The sections the task-loop agents expect besides `Session Handoff`, in the
/// order they are appended.
pub(crate) const SCAFFOLD_SECTIONS: [ScaffoldSection; 4] = [
    ScaffoldSection {
        title: "Build & Test",
        template: include_str!("../../templates/claude-md-build-test.md"),
    },
    ScaffoldSection {
        title: "Coding Rules",
        template: include_str!("../../templates/claude-md-coding-rules.md"),
    },
    ScaffoldSection {
        title: "Project Structure",
        template: include_str!("../../templates/claude-md-project-structure.md"),
    },
    ScaffoldSection {
        title: "Documentation Contract",
        template: include_str!("../../templates/claude-md-documentation-contract.md"),
    },
];

/// Heading written at the top of a `CLAUDE.md` that setup creates.
pub(crate) const NEW_CLAUDE_MD_TITLE: &str = "# CLAUDE.md\n";

/// What [`ensure_scaffold_sections`] did.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct SectionsOutcome {
    /// The file did not exist and was created.
    pub created: bool,
    /// Titles of the sections that were appended, in order.
    pub added: Vec<&'static str>,
}

/// What [`record_inferred_commands`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum CommandsOutcome {
    /// There is no `config.toml` (the project is not initialized).
    NoConfig,
    /// Every inferred key already has a value, or nothing was inferred.
    Unchanged,
    /// These `(key, command)` pairs were written.
    Added(Vec<(&'static str, String)>),
}

/// Normalizes a heading title for comparison: case-insensitive, with runs of
/// whitespace collapsed.
pub(crate) fn normalize_title(title: &str) -> String {
    title
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Normalized titles of the ATX headings (`#` to `######`) in `text`.
/// Headings inside fenced code blocks are not headings.
pub(crate) fn heading_titles(text: &str) -> Vec<String> {
    let mut titles = Vec::new();
    // The fence that opened the code block we are in: its character and length.
    let mut open_fence: Option<(char, usize)> = None;
    for line in text.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        let content = line[indent.min(line.len())..].trim_end();
        if indent > 3 {
            // Indented code, or a continuation line: never a fence or heading.
            continue;
        }
        if let Some((fence_char, fence_len)) = open_fence {
            let run = content.chars().take_while(|c| *c == fence_char).count();
            if run >= fence_len && content[run..].trim().is_empty() {
                open_fence = None;
            }
            continue;
        }
        if let Some(fence_char) = content.chars().next().filter(|c| matches!(c, '`' | '~')) {
            let run = content.chars().take_while(|c| *c == fence_char).count();
            if run >= 3 {
                open_fence = Some((fence_char, run));
                continue;
            }
        }
        if let Some(title) = atx_heading_title(content) {
            titles.push(normalize_title(title));
        }
    }
    titles
}

/// The title of an ATX heading line (already stripped of indentation and
/// trailing whitespace), or `None` when the line is not a heading.
fn atx_heading_title(line: &str) -> Option<&str> {
    let level = line.chars().take_while(|c| *c == '#').count();
    if !(1..=6).contains(&level) {
        return None;
    }
    let rest = &line[level..];
    if !(rest.is_empty() || rest.starts_with([' ', '\t'])) {
        return None;
    }
    let title = rest.trim();
    // An optional closing sequence: `## Title ##`.
    let without_closing = title.trim_end_matches('#');
    if without_closing.is_empty() {
        return Some("");
    }
    if without_closing.len() < title.len() && without_closing.ends_with([' ', '\t']) {
        return Some(without_closing.trim_end());
    }
    Some(title)
}

/// The scaffold sections `text` has no heading for.
pub(crate) fn missing_sections(text: &str) -> Vec<&'static ScaffoldSection> {
    let present = heading_titles(text);
    SCAFFOLD_SECTIONS
        .iter()
        .filter(|section| !present.contains(&normalize_title(section.title)))
        .collect()
}

/// Renders `section`, filling the `Build & Test` fields from `commands` and
/// leaving [`TODO_MARKER`] in every field that has no value.
pub(crate) fn render_section(
    section: &ScaffoldSection,
    commands: &BTreeMap<String, String>,
) -> String {
    // The template's own line endings depend on how the source tree was
    // checked out; normalize them so the caller decides what is written.
    let mut rendered = section.template.replace("\r\n", "\n");
    for key in COMMAND_KEYS {
        let placeholder = format!("{{{{{key}}}}}");
        if rendered.contains(&placeholder) {
            let field = command_field(key, commands.get(*key).map(String::as_str));
            rendered = rendered.replace(&placeholder, &field);
        }
    }
    rendered
}

/// How one command is shown in the `Build & Test` list.
fn command_field(key: &str, command: Option<&str>) -> String {
    let Some(command) = command.map(str::trim).filter(|c| !c.is_empty()) else {
        return TODO_MARKER.to_string();
    };
    if command.contains(['\n', '\r']) {
        // A list item cannot hold several lines without them being read as
        // Markdown of their own. The config stays the place to read it.
        return format!("(multi-line command; see `[commands] {key}` in `.handoff/config.toml`)");
    }
    code_span(command)
}

/// Wraps `text` in a Markdown code span whose delimiter is longer than any
/// backtick run inside it.
fn code_span(text: &str) -> String {
    let mut longest_run = 0;
    let mut run = 0;
    for c in text.chars() {
        run = if c == '`' { run + 1 } else { 0 };
        longest_run = longest_run.max(run);
    }
    let delimiter = "`".repeat(longest_run + 1);
    let padding = if text.starts_with('`') || text.ends_with('`') {
        " "
    } else {
        ""
    };
    format!("{delimiter}{padding}{text}{padding}{delimiter}")
}

/// Appends the scaffold sections `claude_md` lacks (creating the file when it
/// does not exist). Existing content is never rewritten: the new sections are
/// appended after it, byte for byte.
pub(crate) fn ensure_scaffold_sections(
    claude_md: &Path,
    commands: &BTreeMap<String, String>,
) -> Result<SectionsOutcome> {
    use std::io::Write;

    let existing = match std::fs::read(claude_md) {
        Ok(bytes) => Some(bytes),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", claude_md.display())),
    };
    // Headings are ASCII, so a lossy view is enough to find them even when the
    // file holds bytes that are not UTF-8. The bytes themselves are not reused.
    let text = String::from_utf8_lossy(existing.as_deref().unwrap_or_default());
    let missing = missing_sections(&text);
    if missing.is_empty() {
        return Ok(SectionsOutcome::default());
    }

    let line_ending = if text.contains("\r\n") { "\r\n" } else { "\n" };
    let mut addition = String::new();
    match &existing {
        None => addition.push_str(NEW_CLAUDE_MD_TITLE),
        Some(bytes) if !bytes.is_empty() && !bytes.ends_with(b"\n") => addition.push('\n'),
        Some(_) => {}
    }
    let starts_empty = existing.as_ref().is_some_and(Vec::is_empty);
    for (i, section) in missing.iter().enumerate() {
        if !(starts_empty && i == 0) {
            addition.push('\n');
        }
        addition.push_str(&render_section(section, commands));
    }
    if line_ending != "\n" {
        addition = addition.replace('\n', line_ending);
    }

    // Append in place rather than replace the file: the bytes already there
    // are never rewritten, and a CLAUDE.md that is a symlink (to AGENTS.md,
    // say) stays one.
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .create(true)
        .open(claude_md)
        .with_context(|| format!("failed to open {}", claude_md.display()))?;
    file.write_all(addition.as_bytes())
        .with_context(|| format!("failed to write {}", claude_md.display()))?;

    Ok(SectionsOutcome {
        created: existing.is_none(),
        added: missing.iter().map(|section| section.title).collect(),
    })
}

/// Titles of the scaffold sections the file at `claude_md` lacks. A file that
/// does not exist lacks all of them.
pub(crate) fn missing_section_titles(claude_md: &Path) -> Result<Vec<&'static str>> {
    let bytes = match std::fs::read(claude_md) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("failed to read {}", claude_md.display())),
    };
    Ok(missing_sections(&String::from_utf8_lossy(&bytes))
        .iter()
        .map(|section| section.title)
        .collect())
}

/// How many [`TODO_MARKER`]s the file at `claude_md` still holds (0 when it
/// cannot be read: there is then nothing to point the user at).
pub(crate) fn todo_marker_count(claude_md: &Path) -> usize {
    std::fs::read(claude_md)
        .map(|bytes| String::from_utf8_lossy(&bytes).matches(TODO_MARKER).count())
        .unwrap_or(0)
}

/// `config.toml` parsed for editing, or `None` when the file does not exist.
/// A file that is not a usable handoff config is an error: adding commands to
/// it would not make it readable.
fn open_config(config_path: &Path) -> Result<Option<DocumentMut>> {
    let raw = match std::fs::read_to_string(config_path) {
        Ok(raw) => raw,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("failed to read {}", config_path.display()))
        }
    };
    toml::from_str::<Config>(&raw)
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    let doc: DocumentMut = raw
        .parse()
        .with_context(|| format!("failed to parse {}", config_path.display()))?;
    Ok(Some(doc))
}

/// The inferred `(key, command)` pairs `doc` has no value for, in
/// [`COMMAND_KEYS`] order.
fn commands_to_add(
    doc: &DocumentMut,
    inferred: &BTreeMap<&'static str, String>,
) -> Vec<(&'static str, String)> {
    let existing = doc.get(COMMANDS_SECTION);
    COMMAND_KEYS
        .iter()
        .filter_map(|key| {
            let command = inferred.get(key)?;
            let has_value = existing
                .and_then(|commands| commands.get(key))
                .and_then(Item::as_str)
                .is_some_and(|value| !value.trim().is_empty());
            (!has_value).then(|| (*key, command.clone()))
        })
        .collect()
}

/// The `(key, command)` pairs [`record_inferred_commands`] would write:
/// inferred keys that have no value in `config_path` yet. `None` when there is
/// no `config.toml`.
pub(crate) fn pending_commands(
    config_path: &Path,
    inferred: &BTreeMap<&'static str, String>,
) -> Result<Option<Vec<(&'static str, String)>>> {
    Ok(open_config(config_path)?.map(|doc| commands_to_add(&doc, inferred)))
}

/// Writes inferred commands into `[commands]` for keys that have no value yet.
/// The rest of `config.toml` is left exactly as it is.
pub(crate) fn record_inferred_commands(
    config_path: &Path,
    inferred: &BTreeMap<&'static str, String>,
) -> Result<CommandsOutcome> {
    let Some(mut doc) = open_config(config_path)? else {
        return Ok(CommandsOutcome::NoConfig);
    };
    let additions = commands_to_add(&doc, inferred);
    if additions.is_empty() {
        return Ok(CommandsOutcome::Unchanged);
    }

    let commands = ensure_table_like(doc.as_table_mut(), COMMANDS_SECTION)?;
    for (key, command) in &additions {
        commands.insert(key, toml_edit::value(command.as_str()));
    }
    let rendered = doc.to_string();
    // Never leave behind a config.toml the rest of handoff-mcp cannot read.
    toml::from_str::<Config>(&rendered)
        .context("adding [commands] would make config.toml invalid; nothing was written")?;
    crate::storage::atomic_write(config_path, rendered.as_bytes())
        .with_context(|| format!("failed to write {}", config_path.display()))?;
    Ok(CommandsOutcome::Added(additions))
}

/// The values the `Build & Test` section shows: what `[commands]` holds, and
/// for keys it does not define, the inferred value.
pub(crate) fn effective_commands(
    config_path: &Path,
    inferred: &BTreeMap<&'static str, String>,
) -> BTreeMap<String, String> {
    // A missing or unreadable config only means there is nothing to copy from
    // it; the caller reports that problem when it records the commands.
    let configured = read_config(config_path).map(|c| c.commands).ok();
    COMMAND_KEYS
        .iter()
        .filter_map(|key| {
            configured
                .as_ref()
                .and_then(|commands| commands.get(key))
                .or_else(|| inferred.get(key).map(String::as_str))
                .map(|command| (key.to_string(), command.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    fn inferred(pairs: &[(&'static str, &str)]) -> BTreeMap<&'static str, String> {
        pairs.iter().map(|(k, v)| (*k, v.to_string())).collect()
    }

    fn section(title: &str) -> &'static ScaffoldSection {
        SCAFFOLD_SECTIONS.iter().find(|s| s.title == title).unwrap()
    }

    const ALL_TITLES: [&str; 4] = [
        "Build & Test",
        "Coding Rules",
        "Project Structure",
        "Documentation Contract",
    ];

    // --- headings ---

    #[test]
    fn heading_titles_reads_atx_headings_of_every_level() {
        let text = "# One\n## Two\n### Three ###\n#### Four\n##### Five\n###### Six\n";
        assert_eq!(
            heading_titles(text),
            vec!["one", "two", "three", "four", "five", "six"]
        );
    }

    #[test]
    fn heading_titles_normalizes_case_and_whitespace() {
        assert_eq!(
            heading_titles("##   BUILD  &   test  \n"),
            vec!["build & test"]
        );
        assert_eq!(
            heading_titles("   ## Coding Rules\r\n"),
            vec!["coding rules"]
        );
    }

    #[test]
    fn heading_titles_ignores_lines_that_are_not_headings() {
        let text = "#NoSpace\n    ## indented code\n####### seven\nplain ## text\n#\n";
        assert_eq!(heading_titles(text), vec![""]);
    }

    #[test]
    fn heading_titles_skips_fenced_code_blocks() {
        let text = "## Real\n```markdown\n## Build & Test\n```\n~~~~\n## Coding Rules\n```\n~~~\n## Still code\n~~~~\n## After\n";
        assert_eq!(heading_titles(text), vec!["real", "after"]);
    }

    #[test]
    fn heading_titles_treats_an_unclosed_fence_as_code_to_the_end() {
        assert_eq!(
            heading_titles("## Before\n```\n## Build & Test\n"),
            vec!["before"]
        );
    }

    #[test]
    fn missing_sections_matches_case_insensitively_at_any_level() {
        assert_eq!(
            missing_sections("# x\n")
                .iter()
                .map(|s| s.title)
                .collect::<Vec<_>>(),
            ALL_TITLES
        );
        let all = "# build & test\n### CODING RULES\n## Project structure\n#### Documentation Contract ##\n";
        assert!(missing_sections(all).is_empty());
        let partial = "## Coding Rules\n\n## Documentation Contract\n";
        assert_eq!(
            missing_sections(partial)
                .iter()
                .map(|s| s.title)
                .collect::<Vec<_>>(),
            vec!["Build & Test", "Project Structure"]
        );
    }

    #[test]
    fn missing_sections_needs_the_whole_title_and_a_real_heading() {
        let text = "## Build & Test Commands\n\nSee Coding Rules below.\n\n```\n## Project Structure\n```\n";
        assert_eq!(missing_sections(text).len(), 4);
    }

    // --- rendering ---

    #[test]
    fn build_and_test_marks_every_field_when_nothing_is_known() {
        let rendered = render_section(section("Build & Test"), &BTreeMap::new());
        assert!(rendered.starts_with("## Build & Test\n"), "{rendered}");
        for key in COMMAND_KEYS {
            assert!(
                rendered.contains(&format!("- {key}: {TODO_MARKER}\n")),
                "{key}: {rendered}"
            );
        }
        assert_eq!(rendered.matches(TODO_MARKER).count(), COMMAND_KEYS.len());
        assert!(!rendered.contains("{{"), "{rendered}");
    }

    #[test]
    fn build_and_test_shows_known_commands_and_marks_the_rest() {
        let rendered = render_section(
            section("Build & Test"),
            &values(&[("test", "npm run test"), ("smoke", "./scripts/smoke.sh")]),
        );
        assert!(rendered.contains("- test: `npm run test`\n"), "{rendered}");
        assert!(
            rendered.contains("- smoke: `./scripts/smoke.sh`\n"),
            "{rendered}"
        );
        assert!(rendered.contains(&format!("- build: {TODO_MARKER}\n")));
        assert_eq!(
            rendered.matches(TODO_MARKER).count(),
            COMMAND_KEYS.len() - 2
        );
    }

    #[test]
    fn build_and_test_has_no_marker_once_every_field_is_filled() {
        let all: Vec<(&str, &str)> = COMMAND_KEYS.iter().map(|k| (*k, "true")).collect();
        let rendered = render_section(section("Build & Test"), &values(&all));
        assert!(
            !rendered.contains(TODO_MARKER),
            "the section's own prose must not look unfilled: {rendered}"
        );
    }

    #[test]
    fn commands_with_backticks_stay_inside_one_code_span() {
        let rendered = render_section(
            section("Build & Test"),
            &values(&[("test", "echo `date` && make"), ("lint", "`x`")]),
        );
        assert!(
            rendered.contains("- test: ``echo `date` && make``\n"),
            "{rendered}"
        );
        assert!(rendered.contains("- lint: `` `x` ``\n"), "{rendered}");
    }

    #[test]
    fn multi_line_commands_point_at_the_config_instead_of_breaking_the_list() {
        let rendered = render_section(
            section("Build & Test"),
            &values(&[("test", "make a\n## Coding Rules\nmake b")]),
        );
        assert!(!rendered.contains("make a"), "{rendered}");
        assert!(
            rendered.contains(
                "- test: (multi-line command; see `[commands] test` in `.handoff/config.toml`)\n"
            ),
            "{rendered}"
        );
        assert_eq!(heading_titles(&rendered), vec!["build & test"]);
    }

    #[test]
    fn blank_command_values_count_as_unknown() {
        let rendered = render_section(section("Build & Test"), &values(&[("test", "   ")]));
        assert!(rendered.contains(&format!("- test: {TODO_MARKER}\n")));
    }

    /// On a checkout that converts line endings (git on Windows), the
    /// embedded templates arrive with CRLF. What is rendered must not depend
    /// on that: the caller chooses the line ending of the file it writes.
    #[test]
    fn a_template_checked_out_with_crlf_renders_with_plain_newlines() {
        let section = ScaffoldSection {
            title: "Build & Test",
            template: "## Build & Test\r\n\r\n- test: {{test}}\r\n- build: {{build}}\r\n",
        };
        let rendered = render_section(&section, &values(&[("test", "make test")]));
        assert_eq!(
            rendered,
            format!("## Build & Test\n\n- test: `make test`\n- build: {TODO_MARKER}\n")
        );
    }

    #[test]
    fn every_section_starts_with_its_own_heading_and_ends_with_a_newline() {
        for s in &SCAFFOLD_SECTIONS {
            let rendered = render_section(s, &BTreeMap::new());
            assert!(
                rendered.starts_with(&format!("## {}\n", s.title)),
                "{rendered}"
            );
            assert!(rendered.ends_with('\n') && !rendered.ends_with("\n\n"));
            assert_eq!(
                heading_titles(&rendered),
                vec![normalize_title(s.title)],
                "a section must not contain another heading"
            );
            assert!(
                rendered.contains(TODO_MARKER),
                "{}: a fresh section has something to fill in",
                s.title
            );
        }
    }

    // --- CLAUDE.md ---

    #[test]
    fn sections_are_appended_and_existing_bytes_are_untouched() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let original = "# My Project\n\nSome existing content.\n\n## Session Handoff\n\nfoo\n";
        std::fs::write(&path, original).unwrap();

        let outcome = ensure_scaffold_sections(&path, &BTreeMap::new()).unwrap();
        assert_eq!(
            outcome,
            SectionsOutcome {
                created: false,
                added: ALL_TITLES.to_vec()
            }
        );

        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(original), "{after}");
        let appended = &after[original.len()..];
        assert!(
            appended.starts_with("\n## Build & Test\n"),
            "one blank line separates the old content from the new: {appended:?}"
        );
        let positions: Vec<usize> = ALL_TITLES
            .iter()
            .map(|t| appended.find(&format!("\n## {t}\n")).expect(t))
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{positions:?}");
        assert!(missing_sections(&after).is_empty());
        assert!(!after.contains("\n\n\n"), "{after:?}");
    }

    #[test]
    fn content_without_a_trailing_newline_is_kept_as_a_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let original = "# P\n\nlast line without newline";
        std::fs::write(&path, original).unwrap();
        ensure_scaffold_sections(&path, &BTreeMap::new()).unwrap();
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(original));
        assert!(after[original.len()..].starts_with("\n\n## Build & Test\n"));
    }

    #[test]
    fn only_missing_sections_are_appended() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let original =
            "# P\n\n## coding rules\n\nTabs, not spaces.\n\n### Build & Test\n\nmake check\n";
        std::fs::write(&path, original).unwrap();

        let outcome = ensure_scaffold_sections(&path, &values(&[("test", "make test")])).unwrap();
        assert_eq!(
            outcome.added,
            vec!["Project Structure", "Documentation Contract"]
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(original));
        assert_eq!(after.matches("uild & ").count(), 1, "{after}");
        assert!(
            !after.contains("make test"),
            "an existing section is not refilled"
        );
    }

    #[test]
    fn a_missing_file_is_created_with_a_title_and_the_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let outcome = ensure_scaffold_sections(&path, &values(&[("build", "make")])).unwrap();
        assert_eq!(
            outcome,
            SectionsOutcome {
                created: true,
                added: ALL_TITLES.to_vec()
            }
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.starts_with("# CLAUDE.md\n\n## Build & Test\n"),
            "{after}"
        );
        assert!(after.contains("- build: `make`\n"));
    }

    #[cfg(unix)]
    #[test]
    fn a_second_run_writes_nothing() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        ensure_scaffold_sections(&path, &BTreeMap::new()).unwrap();
        let first = std::fs::read(&path).unwrap();

        // Read-only: any attempt to write would fail the call.
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
        let outcome = ensure_scaffold_sections(&path, &values(&[("test", "x")])).unwrap();
        assert_eq!(outcome, SectionsOutcome::default());
        assert_eq!(std::fs::read(&path).unwrap(), first);
    }

    #[test]
    fn crlf_files_get_crlf_sections() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let original = "# P\r\n\r\ntext\r\n";
        std::fs::write(&path, original).unwrap();
        let outcome = ensure_scaffold_sections(&path, &BTreeMap::new()).unwrap();
        assert_eq!(outcome.added, ALL_TITLES.to_vec());
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with(original));
        assert!(after.contains("\r\n\r\n## Build & Test\r\n"), "{after:?}");
        assert!(
            !after.replace("\r\n", "").contains('\n'),
            "no bare LF in a CRLF file: {after:?}"
        );
        assert!(missing_sections(&after).is_empty());
    }

    #[test]
    fn bytes_that_are_not_utf8_survive() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        let original: &[u8] = b"# P\n\ncaf\xe9 latin-1\n";
        std::fs::write(&path, original).unwrap();
        let outcome = ensure_scaffold_sections(&path, &BTreeMap::new()).unwrap();
        assert_eq!(outcome.added.len(), 4);
        let after = std::fs::read(&path).unwrap();
        assert!(after.starts_with(original));
    }

    #[test]
    fn an_empty_file_gets_the_sections_without_a_leading_blank_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("CLAUDE.md");
        std::fs::write(&path, "").unwrap();
        let outcome = ensure_scaffold_sections(&path, &BTreeMap::new()).unwrap();
        assert!(!outcome.created);
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.starts_with("## Build & Test\n"), "{after:?}");
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_claude_md_stays_a_symlink() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("AGENTS.md");
        std::fs::write(&target, "# Agents\n").unwrap();
        let link = dir.path().join("CLAUDE.md");
        std::os::unix::fs::symlink(&target, &link).unwrap();

        ensure_scaffold_sections(&link, &BTreeMap::new()).unwrap();
        assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
        let written = std::fs::read_to_string(&target).unwrap();
        assert!(written.starts_with("# Agents\n\n## Build & Test\n"));
    }

    // --- [commands] ---

    const BASE_CONFIG: &str = "# hand-written comment\n[project]\nname = \"p\"  # trailing comment\n\n[settings]\nmulti_session = true\n";

    fn config_file(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, content).unwrap();
        (dir, path)
    }

    #[test]
    fn no_config_file_means_nothing_is_recorded_or_created() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        let found = inferred(&[("test", "cargo test")]);
        assert_eq!(pending_commands(&path, &found).unwrap(), None);
        assert_eq!(
            record_inferred_commands(&path, &found).unwrap(),
            CommandsOutcome::NoConfig
        );
        assert!(!path.exists());
    }

    #[test]
    fn inferred_commands_are_added_and_the_rest_of_the_file_is_kept() {
        let (_dir, path) = config_file(BASE_CONFIG);
        let found = inferred(&[("test", "npm run test"), ("build", "npm run build")]);
        assert_eq!(
            pending_commands(&path, &found).unwrap(),
            Some(vec![
                ("build", "npm run build".to_string()),
                ("test", "npm run test".to_string())
            ]),
            "keys are listed in [commands] order"
        );

        let outcome = record_inferred_commands(&path, &found).unwrap();
        assert_eq!(
            outcome,
            CommandsOutcome::Added(vec![
                ("build", "npm run build".to_string()),
                ("test", "npm run test".to_string())
            ])
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(
            after.starts_with(BASE_CONFIG),
            "comments and existing sections survive: {after}"
        );
        let config: Config = toml::from_str(&after).unwrap();
        assert_eq!(config.commands.get("test"), Some("npm run test"));
        assert_eq!(config.commands.get("build"), Some("npm run build"));
        assert_eq!(config.commands.get("lint"), None);
    }

    #[test]
    fn existing_values_win_and_blank_ones_are_filled() {
        let (_dir, path) = config_file(
            "[project]\nname = \"p\"\n\n[commands]\ntest = \"make test\"  # mine\nbuild = \"\"\n",
        );
        let found = inferred(&[
            ("test", "npm run test"),
            ("build", "npm run build"),
            ("lint", "npm run lint"),
        ]);
        let outcome = record_inferred_commands(&path, &found).unwrap();
        assert_eq!(
            outcome,
            CommandsOutcome::Added(vec![
                ("build", "npm run build".to_string()),
                ("lint", "npm run lint".to_string())
            ])
        );
        let after = std::fs::read_to_string(&path).unwrap();
        assert!(after.contains("test = \"make test\"  # mine\n"), "{after}");
        let config: Config = toml::from_str(&after).unwrap();
        assert_eq!(config.commands.get("test"), Some("make test"));
        assert_eq!(config.commands.get("build"), Some("npm run build"));
        assert_eq!(config.commands.get("lint"), Some("npm run lint"));
    }

    #[cfg(unix)]
    #[test]
    fn nothing_to_add_leaves_the_file_unwritten_and_creates_no_section() {
        use std::os::unix::fs::PermissionsExt;
        let (dir, path) = config_file(BASE_CONFIG);
        // A read-only directory: an atomic (rename-based) write could not
        // even create its temp file.
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o555)).unwrap();

        let nothing = record_inferred_commands(&path, &BTreeMap::new());
        let (_dir2, valued) = config_file("[project]\nname = \"p\"\n[commands]\ntest = \"t\"\n");
        let all_valued = record_inferred_commands(&valued, &inferred(&[("test", "other")]));

        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(nothing.unwrap(), CommandsOutcome::Unchanged);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), BASE_CONFIG);
        assert!(!std::fs::read_to_string(&path)
            .unwrap()
            .contains("[commands]"));
        assert_eq!(all_valued.unwrap(), CommandsOutcome::Unchanged);
        assert_eq!(
            std::fs::read_to_string(&valued).unwrap(),
            "[project]\nname = \"p\"\n[commands]\ntest = \"t\"\n"
        );
    }

    #[test]
    fn a_config_that_cannot_take_commands_is_an_error_and_stays_as_it_was() {
        for broken in [
            "[project\nname = ",
            "commands = \"not a table\"\n[project]\nname = \"p\"\n",
            // Parses as TOML but is not a valid handoff config: writing into it
            // would bless a file no other tool can read.
            "[settings]\nmulti_session = true\n",
        ] {
            let (_dir, path) = config_file(broken);
            let found = inferred(&[("test", "cargo test")]);
            assert!(record_inferred_commands(&path, &found).is_err(), "{broken}");
            assert!(pending_commands(&path, &found).is_err(), "{broken}");
            assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        }
    }

    /// `handoff_update_config(updates: {"commands": {...}})` writes the section
    /// as an inline table; adding to it must keep what it holds.
    #[test]
    fn commands_written_as_an_inline_table_are_extended_not_replaced() {
        let (_dir, path) = config_file(
            "commands = { smoke = \"./smoke.sh\", test = \"\" }\n\n[project]\nname = \"p\"\n",
        );
        let found = inferred(&[("test", "cargo test"), ("build", "cargo build")]);
        let outcome = record_inferred_commands(&path, &found).unwrap();
        assert_eq!(
            outcome,
            CommandsOutcome::Added(vec![
                ("build", "cargo build".to_string()),
                ("test", "cargo test".to_string())
            ])
        );
        let config = read_config(&path).unwrap();
        assert_eq!(config.commands.get("smoke"), Some("./smoke.sh"));
        assert_eq!(config.commands.get("test"), Some("cargo test"));
        assert_eq!(config.commands.get("build"), Some("cargo build"));
    }

    #[test]
    fn recorded_commands_are_escaped_as_toml_strings() {
        let (_dir, path) = config_file(BASE_CONFIG);
        let command = r#"sh -c "echo \"hi\" | grep 'h'" && true"#;
        record_inferred_commands(&path, &inferred(&[("test", command)])).unwrap();
        let config: Config = toml::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(config.commands.get("test"), Some(command));
    }

    #[test]
    fn effective_commands_prefer_the_config_and_fall_back_to_inference() {
        let (_dir, path) = config_file(
            "[project]\nname = \"p\"\n[commands]\ntest = \"make test\"\nsmoke = \"./smoke.sh\"\nlint = \" \"\n",
        );
        let found = inferred(&[
            ("test", "npm run test"),
            ("build", "npm run build"),
            ("lint", "npm run lint"),
        ]);
        assert_eq!(
            effective_commands(&path, &found),
            values(&[
                ("build", "npm run build"),
                ("lint", "npm run lint"),
                ("smoke", "./smoke.sh"),
                ("test", "make test"),
            ])
        );
    }

    #[test]
    fn effective_commands_are_the_inferred_ones_without_a_usable_config() {
        let dir = tempfile::tempdir().unwrap();
        let found = inferred(&[("test", "cargo test")]);
        assert_eq!(
            effective_commands(&dir.path().join("config.toml"), &found),
            values(&[("test", "cargo test")])
        );
        let (_dir, broken) = config_file("[project\n");
        assert_eq!(
            effective_commands(&broken, &found),
            values(&[("test", "cargo test")])
        );
    }
}

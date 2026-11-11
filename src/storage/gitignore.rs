//! The recommended `.handoff/.gitignore` and the idempotent "ensure" that keeps
//! it up to date.
//!
//! `.handoff/` is meant to be tracked in Git (usually as its own repository),
//! but some of what the server writes there is machine-local: lock files, agent
//! records, timer state, injection caches and derived indexes. Tracking those
//! produces noisy diffs and merge conflicts, so every write path that creates
//! one of them calls [`ensure_gitignore_entry`] first. Only `.handoff/.gitignore`
//! is ever touched: the host project's repository, its `.gitignore` and any
//! already-tracked file are left alone (untracking is the user's `git rm
//! --cached`, documented in the README).
//!
//! [`RECOMMENDED_GITIGNORE_ENTRIES`] is the single source of truth. The
//! README's "Managing `.handoff/` in Git" code block must list exactly these
//! lines (a test pins that), and `handoff_init` writes them all.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use anyhow::{Context, Result};

/// Cross-process `flock` handles (`tasks/**/.lock`, `agents/*.lock`,
/// `test_runs/*.lock`, `brainstorm/*.lock`). `*` also matches the bare `.lock`.
pub(crate) const LOCK_FILES: &str = "*.lock";
/// Agent registry records and their lock files.
pub(crate) const AGENTS_DIR: &str = "/agents/";
/// Timer coordination state shared with the editor extension.
pub(crate) const TIMER_DIR: &str = "/timer/";
/// Per-session "already injected" sidecars of `handoff_memory_query`.
pub(crate) const MEMORY_INJECTED_DIR: &str = "/memory/injected/";
/// Per-session "already injected" sidecars of `handoff_doc_query`.
pub(crate) const DOCS_INJECTED_DIR: &str = "/docs/injected/";
/// Derived: task-id index rebuild cache.
pub(crate) const DOCS_TASK_IDS_REBUILD: &str = "/docs/_task_ids_rebuild.json";
/// Derived: requirements summary cache.
pub(crate) const DOCS_REQUIREMENTS_SUMMARY: &str = "/docs/_requirements_summary.json";
/// Derived: last `handoff_trace_report` output.
pub(crate) const DOCS_TRACE_REPORT: &str = "/docs/_trace_report.json";
/// Derived: latest-result cache of the test run records.
pub(crate) const RUNS_LATEST: &str = "/runs/_latest.json";
/// Derived: trace baseline index (the baseline records themselves are tracked).
pub(crate) const TRACE_BASELINES_INDEX: &str = "/trace/baselines/_index.json";
/// Generated PDFs and their Typst sources.
pub(crate) const EXPORTS_DIR: &str = "/exports/";
/// Transient mermaid render cache (under `exports/`; listed on its own for
/// setups that ignore it without ignoring the exports).
pub(crate) const MERMAID_CACHE_DIR: &str = ".mermaid-cache/";

/// Every line of the recommended `.handoff/.gitignore`, verbatim and in file
/// order. Paths are relative to `.handoff/`; a leading `/` anchors to it.
pub const RECOMMENDED_GITIGNORE_ENTRIES: &[&str] = &[
    LOCK_FILES,
    AGENTS_DIR,
    TIMER_DIR,
    MEMORY_INJECTED_DIR,
    DOCS_INJECTED_DIR,
    DOCS_TASK_IDS_REBUILD,
    DOCS_REQUIREMENTS_SUMMARY,
    DOCS_TRACE_REPORT,
    RUNS_LATEST,
    TRACE_BASELINES_INDEX,
    EXPORTS_DIR,
    MERMAID_CACHE_DIR,
];

/// UTF-8 byte order mark an editor may have put in front of the first line.
const UTF8_BOM: &[u8] = b"\xEF\xBB\xBF";

/// Paths whose failed ensure has already been reported, so a path that stays
/// unwritable warns once per process instead of on every write.
fn warned_paths() -> &'static Mutex<HashSet<PathBuf>> {
    static WARNED: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    WARNED.get_or_init(Mutex::default)
}

/// Where `.gitignore` must be written: the file itself, or, when it is a
/// symlink, the file it points to (a rename over the link would replace the
/// link with a regular file and detach the user's shared ignore list).
fn write_target(path: &Path) -> PathBuf {
    let is_symlink = std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink());
    if !is_symlink {
        return path.to_path_buf();
    }
    // A dangling link cannot be canonicalized; its target is then created in
    // place, which is what the user's link asks for.
    std::fs::canonicalize(path).unwrap_or_else(|_| match std::fs::read_link(path) {
        Ok(link) => path
            .parent()
            .map_or_else(|| link.clone(), |dir| dir.join(&link)),
        Err(_) => path.to_path_buf(),
    })
}

/// Whether some line of `content` (bytes: the file need not be UTF-8) is
/// `entry`, ignoring surrounding ASCII whitespace (so CRLF files match) and a
/// BOM on the first line.
fn has_entry(content: &[u8], entry: &str) -> bool {
    content.split(|&b| b == b'\n').enumerate().any(|(i, line)| {
        let line = if i == 0 {
            line.strip_prefix(UTF8_BOM).unwrap_or(line)
        } else {
            line
        };
        line.trim_ascii() == entry.as_bytes()
    })
}

/// [`try_ensure_gitignore_entries`] for write paths: a failure is reported once
/// per path as a warning on stderr and otherwise ignored.
///
/// Keeping a file out of Git is housekeeping; it must never be the reason a
/// task update, an agent registration, a memory query or an adopt fails. The
/// `Result` return is kept only so existing `?` call sites stay valid: this
/// function always returns `Ok`.
pub(crate) fn ensure_gitignore_entry(handoff: &Path, entry: &str) -> Result<()> {
    ensure_gitignore_entries(handoff, &[entry])
}

/// [`ensure_gitignore_entry`] for several entries with a single read and at
/// most one write. Best-effort, like it.
pub(crate) fn ensure_gitignore_entries(handoff: &Path, entries: &[&str]) -> Result<()> {
    if let Err(error) = try_ensure_gitignore_entries(handoff, entries) {
        let first_report = warned_paths()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(handoff.join(".gitignore"));
        if first_report {
            eprintln!(
                "warning: could not update {}: {error:#}; machine-local files may show up in Git",
                handoff.join(".gitignore").display()
            );
        }
    }
    Ok(())
}

/// Idempotently appends `entries` to `.handoff/.gitignore` (creating the file
/// if absent), each unless the file already contains that exact line. Existing
/// lines and their order are never changed, and a file that has every entry is
/// not rewritten at all. A read or write failure is reported to the caller;
/// write paths use the best-effort [`ensure_gitignore_entries`] instead.
///
/// The file is handled as bytes: a `.gitignore` in another encoding (a
/// Shift_JIS comment, say) is neither rejected nor altered, only appended to.
pub(crate) fn try_ensure_gitignore_entries(handoff: &Path, entries: &[&str]) -> Result<()> {
    let path = write_target(&handoff.join(".gitignore"));
    let existing = match std::fs::read(&path) {
        Ok(content) => content,
        // A missing file is the normal "nothing ignored yet" state; any other
        // error (unreadable, a directory) must not be papered over, or the
        // write below would clobber the user's file.
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(e) => return Err(e).with_context(|| format!("Failed to read {}", path.display())),
    };
    let missing: Vec<&str> = entries
        .iter()
        .copied()
        .filter(|entry| !has_entry(&existing, entry))
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    let mut new_content = existing;
    if !new_content.is_empty() && !new_content.ends_with(b"\n") {
        new_content.push(b'\n');
    }
    for entry in missing {
        new_content.extend_from_slice(entry.as_bytes());
        new_content.push(b'\n');
    }
    crate::storage::atomic_write(&path, &new_content)
        .with_context(|| format!("Failed to write {}", path.display()))
}

/// Ensures the whole recommended list; used by `handoff_init`. Best-effort.
pub(crate) fn ensure_recommended_gitignore(handoff: &Path) -> Result<()> {
    ensure_gitignore_entries(handoff, RECOMMENDED_GITIGNORE_ENTRIES)
}

/// Ensures the `*.lock` entry for a task directory (`.handoff/tasks/**`), whose
/// `.lock` file is created next to the task record.
///
/// A directory that is not below a `tasks/` directory has no `.handoff/` to
/// maintain a `.gitignore` for, so there is nothing to do for it.
pub(crate) fn ensure_task_lock_ignored(task_dir: &Path) -> Result<()> {
    let handoff = task_dir
        .ancestors()
        .find(|a| a.file_name().is_some_and(|n| n == "tasks"))
        .and_then(Path::parent);
    match handoff {
        Some(handoff) => ensure_gitignore_entry(handoff, LOCK_FILES),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(handoff: &Path) -> Vec<String> {
        std::fs::read_to_string(handoff.join(".gitignore"))
            .unwrap()
            .lines()
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn ensure_gitignore_entry_is_idempotent_and_preserves_existing_content() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path();
        std::fs::write(handoff.join(".gitignore"), "existing-line\n").unwrap();

        ensure_gitignore_entry(handoff, RUNS_LATEST).unwrap();
        let after_first = std::fs::read(handoff.join(".gitignore")).unwrap();
        ensure_gitignore_entry(handoff, RUNS_LATEST).unwrap();

        assert_eq!(
            std::fs::read(handoff.join(".gitignore")).unwrap(),
            after_first,
            "a second call must leave the bytes unchanged"
        );
        assert_eq!(lines(handoff), vec!["existing-line", RUNS_LATEST]);
    }

    #[test]
    fn ensure_does_not_glue_the_entry_to_an_unterminated_last_line() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join(".gitignore"), "my-notes/").unwrap();

        ensure_gitignore_entry(tmp.path(), AGENTS_DIR).unwrap();

        assert_eq!(lines(tmp.path()), vec!["my-notes/", AGENTS_DIR]);
    }

    #[test]
    fn ensure_entries_appends_only_the_missing_ones_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join(".gitignore"),
            format!("my-notes/\n{AGENTS_DIR}\n"),
        )
        .unwrap();

        ensure_gitignore_entries(tmp.path(), &[AGENTS_DIR, LOCK_FILES, TIMER_DIR]).unwrap();

        assert_eq!(
            lines(tmp.path()),
            vec!["my-notes/", AGENTS_DIR, LOCK_FILES, TIMER_DIR]
        );
    }

    #[test]
    fn strict_ensure_fails_on_an_unreadable_gitignore_instead_of_clobbering_it() {
        let tmp = tempfile::tempdir().unwrap();
        // A directory where the file should be: reading it fails with a
        // non-NotFound error on every platform and for every user.
        let path = tmp.path().join(".gitignore");
        std::fs::create_dir(&path).unwrap();

        assert!(try_ensure_gitignore_entries(tmp.path(), &[LOCK_FILES]).is_err());
        assert!(path.is_dir(), "the user's entry must be left as it was");
    }

    #[test]
    fn write_path_ensure_never_fails_on_an_unreadable_gitignore() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join(".gitignore")).unwrap();

        ensure_gitignore_entry(tmp.path(), LOCK_FILES).unwrap();
        ensure_gitignore_entries(tmp.path(), &[AGENTS_DIR, LOCK_FILES]).unwrap();
        ensure_recommended_gitignore(tmp.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn write_path_ensure_never_fails_when_the_handoff_dir_is_unwritable() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir(&handoff).unwrap();
        std::fs::set_permissions(&handoff, std::fs::Permissions::from_mode(0o555)).unwrap();
        // root (and some CI sandboxes) ignore directory permissions: then there
        // is no failure to observe.
        let can_write = std::fs::File::create(handoff.join("probe")).is_ok();

        let strict = try_ensure_gitignore_entries(&handoff, &[LOCK_FILES]);
        let best_effort = ensure_gitignore_entry(&handoff, LOCK_FILES);

        std::fs::set_permissions(&handoff, std::fs::Permissions::from_mode(0o755)).unwrap();
        best_effort.expect("a write path must not fail because .gitignore is unwritable");
        if !can_write {
            assert!(strict.is_err(), "the strict variant reports the failure");
            assert!(!handoff.join(".gitignore").exists());
        }
    }

    #[test]
    fn ensure_tolerates_a_non_utf8_gitignore_and_keeps_its_bytes() {
        let tmp = tempfile::tempdir().unwrap();
        // "# テスト" in Shift_JIS, then an entry.
        let original: Vec<u8> = [
            b"# ".as_slice(),
            &[0x83, 0x65, 0x83, 0x58, 0x83, 0x67],
            b"\n*.lock\n",
        ]
        .concat();
        let path = tmp.path().join(".gitignore");
        std::fs::write(&path, &original).unwrap();

        // Present entry: not rewritten at all.
        try_ensure_gitignore_entries(tmp.path(), &[LOCK_FILES]).unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), original);

        // Missing entry: appended after the untouched original bytes.
        try_ensure_gitignore_entries(tmp.path(), &[AGENTS_DIR]).unwrap();
        let expected = [original.as_slice(), b"/agents/\n"].concat();
        assert_eq!(std::fs::read(&path).unwrap(), expected);
    }

    #[test]
    fn ensure_recognizes_an_entry_on_a_bom_prefixed_first_line() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "\u{feff}*.lock\n";
        std::fs::write(tmp.path().join(".gitignore"), original).unwrap();

        try_ensure_gitignore_entries(tmp.path(), &[LOCK_FILES]).unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
            original,
            "an entry behind a BOM counts as present"
        );

        try_ensure_gitignore_entries(tmp.path(), &[AGENTS_DIR]).unwrap();
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
            format!("{original}{AGENTS_DIR}\n"),
            "the BOM stays where it was"
        );
    }

    #[test]
    fn ensure_treats_crlf_lines_as_present_entries() {
        let tmp = tempfile::tempdir().unwrap();
        let original = "*.lock\r\n/agents/\r\n";
        std::fs::write(tmp.path().join(".gitignore"), original).unwrap();

        try_ensure_gitignore_entries(tmp.path(), &[LOCK_FILES, AGENTS_DIR]).unwrap();

        assert_eq!(
            std::fs::read_to_string(tmp.path().join(".gitignore")).unwrap(),
            original
        );
    }

    #[cfg(unix)]
    #[test]
    fn ensure_keeps_a_symlinked_gitignore_a_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        std::fs::create_dir(&handoff).unwrap();
        let real = tmp.path().join("shared-gitignore");
        std::fs::write(&real, "mine\n").unwrap();
        std::os::unix::fs::symlink(&real, handoff.join(".gitignore")).unwrap();

        try_ensure_gitignore_entries(&handoff, &[LOCK_FILES]).unwrap();

        assert!(
            std::fs::symlink_metadata(handoff.join(".gitignore"))
                .unwrap()
                .file_type()
                .is_symlink(),
            "the symlink must not be replaced by a regular file"
        );
        assert_eq!(std::fs::read_to_string(&real).unwrap(), "mine\n*.lock\n");
    }

    #[test]
    fn ensure_recommended_writes_every_entry_once_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        ensure_recommended_gitignore(tmp.path()).unwrap();
        let first = std::fs::read(tmp.path().join(".gitignore")).unwrap();
        ensure_recommended_gitignore(tmp.path()).unwrap();

        assert_eq!(std::fs::read(tmp.path().join(".gitignore")).unwrap(), first);
        assert_eq!(lines(tmp.path()), RECOMMENDED_GITIGNORE_ENTRIES);
    }

    #[test]
    fn recommended_entries_are_unique_and_cover_the_machine_local_paths() {
        let mut sorted = RECOMMENDED_GITIGNORE_ENTRIES.to_vec();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(sorted.len(), RECOMMENDED_GITIGNORE_ENTRIES.len());
        for must in [
            "*.lock",
            "/agents/",
            "/timer/",
            "/memory/injected/",
            "/docs/injected/",
            "/exports/",
            ".mermaid-cache/",
        ] {
            assert!(
                RECOMMENDED_GITIGNORE_ENTRIES.contains(&must),
                "{must} missing"
            );
        }
    }

    #[test]
    fn task_lock_ensure_finds_the_handoff_dir_above_nested_task_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let handoff = tmp.path().join(".handoff");
        let task_dir = handoff.join("tasks").join("t1-parent").join("t1.2-child");
        std::fs::create_dir_all(&task_dir).unwrap();

        ensure_task_lock_ignored(&task_dir).unwrap();

        assert_eq!(lines(&handoff), vec![LOCK_FILES]);
    }

    #[test]
    fn task_lock_ensure_ignores_directories_outside_a_tasks_tree() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("elsewhere");
        std::fs::create_dir_all(&dir).unwrap();

        ensure_task_lock_ignored(&dir).unwrap();

        assert!(!tmp.path().join(".gitignore").exists());
        assert!(!dir.join(".gitignore").exists());
    }

    /// Returns the `.handoff/.gitignore` code block of the README's
    /// "Managing `.handoff/` in Git" section.
    fn readme_gitignore_block() -> String {
        let readme = include_str!("../../README.md");
        let mut in_section = false;
        let mut in_block = false;
        let mut block = Vec::new();
        for line in readme.lines() {
            if !in_section {
                in_section = line.starts_with('#') && line.contains("Managing `.handoff/` in Git");
            } else if !in_block {
                in_block = line.trim_end() == "```gitignore";
            } else if line.trim_end() == "```" {
                return block.join("\n");
            } else {
                block.push(line);
            }
        }
        panic!("README has no '```gitignore' block under 'Managing `.handoff/` in Git'");
    }

    fn entries_of(block: &str) -> Vec<&str> {
        block
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    }

    #[test]
    fn readme_block_lists_exactly_the_recommended_entries() {
        let block = readme_gitignore_block();
        let readme_entries = entries_of(&block);
        let missing: Vec<_> = RECOMMENDED_GITIGNORE_ENTRIES
            .iter()
            .filter(|e| !readme_entries.contains(e))
            .collect();
        let extra: Vec<_> = readme_entries
            .iter()
            .filter(|e| !RECOMMENDED_GITIGNORE_ENTRIES.contains(e))
            .collect();
        assert!(
            missing.is_empty() && extra.is_empty(),
            "README block drifted from RECOMMENDED_GITIGNORE_ENTRIES: missing from README \
             {missing:?}, not in the constant {extra:?}"
        );
        assert_eq!(readme_entries, RECOMMENDED_GITIGNORE_ENTRIES);
    }

    #[test]
    fn readme_block_check_reports_a_deleted_line() {
        let block = readme_gitignore_block();
        let without_agents: String = block
            .lines()
            .filter(|l| l.trim() != AGENTS_DIR)
            .collect::<Vec<_>>()
            .join("\n");
        assert_ne!(
            entries_of(&without_agents),
            RECOMMENDED_GITIGNORE_ENTRIES,
            "removing a line from the block must make the comparison fail"
        );
    }
}

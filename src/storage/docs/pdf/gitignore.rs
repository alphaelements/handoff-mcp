//! Detects whether `.handoff/exports/` is git-ignored (REQ-PDF-014 AC4).
//!
//! The export directory holds generated `.typ` / `.pdf` files that should not
//! be committed. The check is a plain reading of the two `.gitignore` files
//! that can cover it, not a `git check-ignore` run, so it also works when the
//! project is not a git checkout (or `git` is not installed).

use std::path::Path;

use super::export::EXPORTS_SUBDIR;
use super::types::{PdfWarning, PdfWarningCode};

/// A [`PdfWarningCode::ExportsNotGitignored`] warning when neither
/// `<handoff_dir>/.gitignore` nor `<project_root>/.gitignore` ignores
/// `<handoff_dir>/exports/`; `None` when one of them does.
///
/// Recognised entries (leading `/` and trailing `/` are optional):
/// * in `<handoff_dir>/.gitignore`: `exports`, `exports/*`, `exports/**`, `*`
/// * in `<project_root>/.gitignore`: `<handoff>` (e.g. `.handoff`),
///   `<handoff>/exports`, `<handoff>/exports/*`, `<handoff>/exports/**`,
///   `<handoff>/*`, or a bare `exports`
///
/// Negations (`!...`) and comments are skipped; a later negation that
/// re-includes the directory is not modelled.
pub fn exports_gitignore_warning(project_root: &Path, handoff_dir: &Path) -> Option<PdfWarning> {
    let mut covered = file_ignores(&handoff_dir.join(".gitignore"), &[EXPORTS_SUBDIR, "*"]);
    if !covered {
        if let Ok(rel) = handoff_dir.strip_prefix(project_root) {
            let rel = rel.to_string_lossy().replace('\\', "/");
            let rel = rel.trim_matches('/');
            if !rel.is_empty() {
                let nested = format!("{rel}/{EXPORTS_SUBDIR}");
                covered = file_ignores(
                    &project_root.join(".gitignore"),
                    &[rel, nested.as_str(), EXPORTS_SUBDIR],
                );
            }
        }
    }
    if covered {
        return None;
    }
    let handoff = handoff_dir_label(project_root, handoff_dir);
    Some(
        PdfWarning::new(
            PdfWarningCode::ExportsNotGitignored,
            format!(
                "{handoff}/{EXPORTS_SUBDIR}/ is not listed in a .gitignore, so generated PDFs \
                 may be committed; add '/{EXPORTS_SUBDIR}/' to {handoff}/.gitignore"
            ),
        )
        .with_detail(handoff_dir.join(EXPORTS_SUBDIR).display().to_string()),
    )
}

/// `.handoff` (relative to the project) when `handoff_dir` is below
/// `project_root`, else the full path.
fn handoff_dir_label(project_root: &Path, handoff_dir: &Path) -> String {
    handoff_dir
        .strip_prefix(project_root)
        .unwrap_or(handoff_dir)
        .display()
        .to_string()
}

/// `true` when some line of the `.gitignore` at `path` ignores one of
/// `targets` (project-relative, `/`-separated, no leading or trailing `/`).
fn file_ignores(path: &Path, targets: &[&str]) -> bool {
    // A missing or unreadable file lists nothing; reporting "not ignored" is
    // the conservative answer because the warning is advisory only.
    let content = std::fs::read_to_string(path).unwrap_or_default();
    content.lines().any(|line| line_ignores(line, targets))
}

fn line_ignores(line: &str, targets: &[&str]) -> bool {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
        return false;
    }
    let pattern = line.trim_start_matches('/').trim_end_matches('/');
    let pattern = pattern
        .strip_suffix("/**")
        .or_else(|| pattern.strip_suffix("/*"))
        .unwrap_or(pattern);
    targets.contains(&pattern)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let handoff = dir.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        (dir, handoff)
    }

    fn warns(dir: &tempfile::TempDir, handoff: &Path) -> bool {
        exports_gitignore_warning(dir.path(), handoff).is_some()
    }

    #[test]
    fn warns_when_no_gitignore_exists() {
        let (dir, handoff) = project();
        let w = exports_gitignore_warning(dir.path(), &handoff).expect("warning");
        assert_eq!(w.code, PdfWarningCode::ExportsNotGitignored);
        assert!(w.message.contains(".handoff/exports/"), "{}", w.message);
        assert_eq!(
            w.detail.as_deref(),
            Some(handoff.join("exports").to_str().unwrap())
        );
    }

    #[test]
    fn warns_when_gitignores_do_not_mention_exports() {
        let (dir, handoff) = project();
        std::fs::write(
            handoff.join(".gitignore"),
            "/memory/injected/\n# exports/\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\nnode_modules\n").unwrap();
        assert!(warns(&dir, &handoff));
    }

    #[test]
    fn quiet_when_handoff_gitignore_lists_exports() {
        for entry in [
            "exports/",
            "/exports/",
            "exports",
            "/exports",
            "exports/*",
            "/exports/**",
        ] {
            let (dir, handoff) = project();
            std::fs::write(handoff.join(".gitignore"), format!("/runs/\n{entry}\n")).unwrap();
            assert!(!warns(&dir, &handoff), "entry {entry:?} should count");
        }
    }

    #[test]
    fn quiet_when_project_gitignore_covers_the_exports_dir() {
        for entry in [
            ".handoff/",
            "/.handoff",
            ".handoff/exports/",
            "/.handoff/exports",
            ".handoff/exports/*",
            ".handoff/*",
            "exports/",
        ] {
            let (dir, handoff) = project();
            std::fs::write(dir.path().join(".gitignore"), format!("target/\n{entry}\n")).unwrap();
            assert!(!warns(&dir, &handoff), "entry {entry:?} should count");
        }
    }

    #[test]
    fn negations_comments_and_look_alikes_do_not_count() {
        let (dir, handoff) = project();
        std::fs::write(
            handoff.join(".gitignore"),
            "!exports/\n# exports/\nexports-old/\nexports/keep.txt\n",
        )
        .unwrap();
        std::fs::write(dir.path().join(".gitignore"), ".handoff-backup/\n").unwrap();
        assert!(warns(&dir, &handoff));
    }

    #[test]
    fn handoff_dir_outside_the_project_only_checks_its_own_gitignore() {
        let (dir, _) = project();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".gitignore"), ".handoff/\n").unwrap();
        let w = exports_gitignore_warning(dir.path(), elsewhere.path());
        assert!(w.is_some());
        std::fs::write(elsewhere.path().join(".gitignore"), "/exports/\n").unwrap();
        assert!(exports_gitignore_warning(dir.path(), elsewhere.path()).is_none());
    }
}

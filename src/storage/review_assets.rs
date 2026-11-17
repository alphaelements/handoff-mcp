//! Image paths of review board items (RB-010).
//!
//! An item embeds an image as a Markdown link `![alt](path)` that must resolve
//! from `review_board.md`, i.e. relative to `.handoff/`. Two path types are
//! supported:
//!
//! 1. the standard asset location `review_board_assets/<file>` (also accepted
//!    as `.handoff/review_board_assets/<file>`, which is rewritten to the form
//!    that resolves from the board);
//! 2. any other relative path, stored as given (`../design/mockup.png`
//!    resolves to `<project>/design/mockup.png`).
//!
//! The asset directory holds machine-local screenshots, so the first time an
//! item references it `review_board_assets/` is added to `.handoff/.gitignore`
//! (NFR-RB-SEC-001). Files are never copied or moved, and a project-relative
//! path never touches `.gitignore`.

use std::borrow::Cow;
use std::path::Path;

use anyhow::Result;

use super::gitignore::ensure_gitignore_entry;

/// Directory of review board images, relative to `.handoff/`.
pub const ASSETS_DIR: &str = "review_board_assets";

/// `.gitignore` line that keeps [`ASSETS_DIR`] out of Git.
pub(crate) const ASSETS_GITIGNORE_ENTRY: &str = "review_board_assets/";

/// Prefix a caller may put in front of an asset path.
const HANDOFF_PREFIX: &str = ".handoff/";

/// `path` as it must be written in the board: a `.handoff/review_board_assets/`
/// path loses its `.handoff/` prefix (it would not resolve from the board
/// otherwise); every other path is returned unchanged.
pub(crate) fn normalize_image_path(path: &str) -> Cow<'_, str> {
    match path.strip_prefix(HANDOFF_PREFIX) {
        Some(rest) if is_asset_path(rest) => Cow::Borrowed(rest),
        _ => Cow::Borrowed(path),
    }
}

/// Whether `path` (already normalized) points into [`ASSETS_DIR`].
pub(crate) fn is_asset_path(path: &str) -> bool {
    path.strip_prefix(ASSETS_DIR)
        .is_some_and(|rest| rest.starts_with('/'))
}

/// Ensures `.handoff/.gitignore` ignores [`ASSETS_DIR`] when at least one of
/// the (normalized) `paths` is inside it. Best-effort and idempotent, like
/// every other gitignore write.
pub(crate) fn ensure_assets_ignored<S: AsRef<str>>(handoff: &Path, paths: &[S]) -> Result<()> {
    if paths.iter().any(|p| is_asset_path(p.as_ref())) {
        ensure_gitignore_entry(handoff, ASSETS_GITIGNORE_ENTRY)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gitignore(handoff: &Path) -> Option<String> {
        std::fs::read_to_string(handoff.join(".gitignore")).ok()
    }

    #[test]
    fn handoff_prefixed_asset_path_is_rewritten_relative_to_the_board() {
        assert_eq!(
            normalize_image_path(".handoff/review_board_assets/a.png"),
            "review_board_assets/a.png"
        );
        assert_eq!(
            normalize_image_path("review_board_assets/a.png"),
            "review_board_assets/a.png"
        );
    }

    #[test]
    fn other_paths_are_kept_as_given() {
        for p in [
            "../design/mockup.png",
            ".handoff/other/x.png",
            "review_board_assets_old/x.png",
            "sub/review_board_assets/x.png",
        ] {
            assert_eq!(normalize_image_path(p), p);
        }
    }

    #[test]
    fn asset_path_detection() {
        assert!(is_asset_path("review_board_assets/a.png"));
        assert!(is_asset_path("review_board_assets/sub/a.png"));
        assert!(!is_asset_path("review_board_assets"));
        assert!(!is_asset_path("review_board_assets_x/a.png"));
        assert!(!is_asset_path("../review_board_assets/a.png"));
    }

    #[test]
    fn asset_path_creates_gitignore_with_the_entry() {
        let d = tempfile::tempdir().unwrap();
        ensure_assets_ignored(d.path(), &["review_board_assets/a.png"]).unwrap();
        assert_eq!(gitignore(d.path()).unwrap(), "review_board_assets/\n");
    }

    #[test]
    fn existing_entries_are_kept_and_the_entry_is_added_once() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join(".gitignore"), "*.lock\n/agents/").unwrap();
        for _ in 0..3 {
            ensure_assets_ignored(d.path(), &["review_board_assets/a.png"]).unwrap();
        }
        assert_eq!(
            gitignore(d.path()).unwrap(),
            "*.lock\n/agents/\nreview_board_assets/\n"
        );
    }

    #[test]
    fn project_relative_paths_do_not_touch_gitignore() {
        let d = tempfile::tempdir().unwrap();
        ensure_assets_ignored(d.path(), &["../design/mockup.png"]).unwrap();
        ensure_assets_ignored::<&str>(d.path(), &[]).unwrap();
        assert!(gitignore(d.path()).is_none());
    }

    #[test]
    fn mixed_paths_ignore_the_assets_dir() {
        let d = tempfile::tempdir().unwrap();
        ensure_assets_ignored(
            d.path(),
            &["../design/mockup.png", "review_board_assets/a.png"],
        )
        .unwrap();
        assert_eq!(gitignore(d.path()).unwrap(), "review_board_assets/\n");
    }
}

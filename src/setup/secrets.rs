//! The `secrets_untracked` check (SPEC-ONB-050): is a file that usually holds
//! secrets tracked by Git?
//!
//! Shared by the readiness diagnosis (`doctor`) and the pre-release check.
//! It matches the *names* of the files Git tracks against a list of patterns
//! and nothing more: no file is opened, and no process is started (the
//! tracked files come from `storage::git_index`, which reads the index file).

use std::path::Path;

use super::doctor::{Check, CheckStatus};
use crate::storage::config::SECRET_FILE_EXCLUSIONS;
use crate::storage::git_index::{tracked_files, TrackedFiles};

/// Id of the check.
pub const SECRETS_UNTRACKED: &str = "secrets_untracked";

/// How many matching paths the detail lists before summarizing.
const MATCHES_LISTED: usize = 20;

/// Said in every outcome, so a `pass` is never read as "no secrets in the
/// repository".
const CONTENTS_NOT_SCANNED: &str = "Only file names were matched; file contents were not scanned.";

/// Does `name` match the glob `pattern`? `*` stands for any run of
/// characters and `?` for exactly one; letters compare without regard to
/// ASCII case, since the file systems secrets leak from mostly ignore it.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().map(|c| c.to_ascii_lowercase()).collect();
    let name: Vec<char> = name.chars().map(|c| c.to_ascii_lowercase()).collect();
    let (mut p, mut n) = (0, 0);
    // Where to resume when a `*` has to swallow one more character.
    let mut backtrack: Option<(usize, usize)> = None;
    while n < name.len() {
        match pattern.get(p) {
            Some('*') => {
                backtrack = Some((p, n));
                p += 1;
            }
            Some(&c) if c == '?' || c == name[n] => {
                p += 1;
                n += 1;
            }
            _ => match backtrack {
                Some((star, matched)) => {
                    p = star + 1;
                    n = matched + 1;
                    backtrack = Some((star, matched + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|c| *c == '*')
}

/// Is the tracked file at `path` (relative, `/`-separated) one the patterns
/// call a secrets file? A pattern without `/` is matched against the file
/// name, so it applies in every directory; one with `/` is matched against
/// the whole path. The template files in [`SECRET_FILE_EXCLUSIONS`] never
/// count.
pub fn is_secret_file(path: &str, patterns: &[&str]) -> bool {
    let file_name = path.rsplit('/').next().unwrap_or(path);
    if SECRET_FILE_EXCLUSIONS
        .iter()
        .any(|excluded| excluded.eq_ignore_ascii_case(file_name))
    {
        return false;
    }
    patterns.iter().any(|pattern| {
        if pattern.contains('/') {
            glob_matches(pattern, path)
        } else {
            glob_matches(pattern, file_name)
        }
    })
}

/// Judges whether Git tracks a secrets file under `project_dir`.
pub fn check_secrets_untracked(project_dir: &Path, patterns: &[&str]) -> Check {
    judge(&tracked_files(project_dir), patterns)
}

fn judge(tracked: &TrackedFiles, patterns: &[&str]) -> Check {
    let skipped = |reason: String| {
        Check::new(
            SECRETS_UNTRACKED,
            CheckStatus::Skip,
            format!("not checked: {reason}. {CONTENTS_NOT_SCANNED}"),
        )
    };
    if patterns.is_empty() {
        return skipped(
            "`[onboarding] secret_file_patterns` is an empty list, so there is nothing to match"
                .to_string(),
        );
    }
    let files = match tracked {
        TrackedFiles::Files(files) => files,
        TrackedFiles::NotARepository => {
            return skipped("the project is not in a Git repository".to_string())
        }
        TrackedFiles::Unreadable(reason) => return skipped(reason.clone()),
    };

    let matches: Vec<&str> = files
        .iter()
        .map(String::as_str)
        .filter(|path| is_secret_file(path, patterns))
        .collect();
    if matches.is_empty() {
        return Check::new(
            SECRETS_UNTRACKED,
            CheckStatus::Pass,
            format!(
                "none of the {} file(s) Git tracks matches a secrets pattern ({}). \
                 {CONTENTS_NOT_SCANNED}",
                files.len(),
                patterns.join(", ")
            ),
        );
    }

    let mut listed: Vec<String> = matches
        .iter()
        .take(MATCHES_LISTED)
        .map(|path| format!("`{path}`"))
        .collect();
    if matches.len() > MATCHES_LISTED {
        listed.push(format!("and {} more", matches.len() - MATCHES_LISTED));
    }
    Check::failed(
        SECRETS_UNTRACKED,
        format!(
            "Git tracks {} file(s) whose name matches a secrets pattern: {}. \
             {CONTENTS_NOT_SCANNED}",
            matches.len(),
            listed.join(", ")
        ),
        "Stop tracking each file (`git rm --cached <path>`), add it to .gitignore, and replace \
         any secret that was already committed or pushed (removing the file does not remove it \
         from the history). If a listed file holds no secret, narrow \
         `[onboarding] secret_file_patterns`.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::config::DEFAULT_SECRET_FILE_PATTERNS;

    fn tracked(paths: &[&str]) -> TrackedFiles {
        TrackedFiles::Files(paths.iter().map(|p| p.to_string()).collect())
    }

    #[test]
    fn globs_match_whole_names_with_star_and_question_mark() {
        for (pattern, name, expected) in [
            (".env", ".env", true),
            (".env", ".envrc", false),
            (".env", "x.env", false),
            (".env.*", ".env.local", true),
            (".env.*", ".env.", true),
            (".env.*", ".env", false),
            ("*.pem", "server.pem", true),
            ("*.pem", ".pem", true),
            ("*.pem", "server.pem.bak", false),
            ("secrets.*", "secrets.yaml", true),
            ("secrets.*", "my-secrets.yaml", false),
            ("id_rsa", "id_rsa", true),
            ("id_rsa", "id_rsa.pub", false),
            ("a*b*c", "aXXbYYc", true),
            ("a*b*c", "abcbc", true),
            ("a*b*c", "aXXbYY", false),
            ("key-??.txt", "key-01.txt", true),
            ("key-??.txt", "key-1.txt", false),
            ("*", "anything", true),
            ("**", "", true),
            ("", "", true),
            ("", "x", false),
            // Case is ignored.
            ("*.PEM", "server.pem", true),
            (".env", ".ENV", true),
            // Not confused by characters beyond ASCII.
            ("*.key", "鍵.key", true),
            ("?.key", "鍵.key", true),
        ] {
            assert_eq!(
                glob_matches(pattern, name),
                expected,
                "{pattern:?} vs {name:?}"
            );
        }
    }

    #[test]
    fn the_default_patterns_find_the_usual_secrets_files_in_any_directory() {
        for path in [
            ".env",
            ".env.local",
            ".env.production",
            "apps/web/.env",
            "deploy/server.pem",
            "certs/private.key",
            ".ssh/id_rsa",
            "keys/id_ed25519",
            "config/credentials.json",
            "secrets.yml",
            "infra/secrets.tfvars",
            "Credentials.JSON",
        ] {
            assert!(
                is_secret_file(path, DEFAULT_SECRET_FILE_PATTERNS),
                "{path} should be reported"
            );
        }
        for path in [
            "README.md",
            "src/env.rs",
            ".envrc",
            "environment.ts",
            "id_rsa.pub",
            "docs/keys.md",
            "key.txt",
            "src/secrets/mod.rs",
            "pemfile",
        ] {
            assert!(
                !is_secret_file(path, DEFAULT_SECRET_FILE_PATTERNS),
                "{path} should not be reported"
            );
        }
    }

    // SPEC-ONB-050 AC3
    #[test]
    fn template_files_are_never_reported() {
        for path in [
            ".env.example",
            ".env.sample",
            ".env.template",
            "apps/web/.env.example",
            ".ENV.Example",
        ] {
            assert!(
                !is_secret_file(path, DEFAULT_SECRET_FILE_PATTERNS),
                "{path}"
            );
            // Not even when a configured pattern names them outright.
            assert!(!is_secret_file(path, &["*", ".env.example"]), "{path}");
        }
        // Only those exact names: a real file next to them still counts.
        assert!(is_secret_file(
            ".env.example.local",
            DEFAULT_SECRET_FILE_PATTERNS
        ));
    }

    #[test]
    fn a_pattern_with_a_slash_is_matched_against_the_whole_path() {
        let patterns = ["config/*.yaml", "token.txt"];
        assert!(is_secret_file("config/prod.yaml", &patterns));
        assert!(!is_secret_file("other/prod.yaml", &patterns));
        assert!(!is_secret_file("prod.yaml", &patterns));
        assert!(is_secret_file("deep/dir/token.txt", &patterns));
    }

    // SPEC-ONB-050 AC1
    #[test]
    fn a_tracked_secrets_file_fails_and_is_named() {
        let check = judge(
            &tracked(&[".env", "README.md", "deploy/server.pem"]),
            DEFAULT_SECRET_FILE_PATTERNS,
        );
        assert_eq!(check.id, SECRETS_UNTRACKED);
        assert_eq!(check.status, CheckStatus::Fail);
        assert!(check.detail.contains("`.env`"), "{}", check.detail);
        assert!(
            check.detail.contains("`deploy/server.pem`"),
            "{}",
            check.detail
        );
        assert!(!check.detail.contains("README.md"), "{}", check.detail);
        assert!(check.detail.contains("2 file(s)"), "{}", check.detail);
        assert!(
            check.detail.contains("contents were not scanned"),
            "{}",
            check.detail
        );
        let next_step = check.next_step.expect("a fail says what to do");
        assert!(next_step.contains("git rm --cached"), "{next_step}");
    }

    // SPEC-ONB-050 AC2 / AC3
    #[test]
    fn no_tracked_secrets_file_passes_and_says_contents_were_not_scanned() {
        for files in [vec![], vec!["README.md", ".env.example", ".gitignore"]] {
            let check = judge(&tracked(&files), DEFAULT_SECRET_FILE_PATTERNS);
            assert_eq!(check.status, CheckStatus::Pass, "{files:?}");
            assert!(
                check.detail.contains("contents were not scanned"),
                "{}",
                check.detail
            );
            assert_eq!(check.next_step, None);
        }
    }

    // SPEC-ONB-050 AC4, SPEC-ONB-092
    #[test]
    fn what_cannot_be_listed_is_skipped_with_the_reason_never_passed() {
        let outside = judge(&TrackedFiles::NotARepository, DEFAULT_SECRET_FILE_PATTERNS);
        assert_eq!(outside.status, CheckStatus::Skip);
        assert!(
            outside.detail.contains("not in a Git repository"),
            "{}",
            outside.detail
        );
        assert!(outside.detail.contains("contents were not scanned"));

        let unreadable = judge(
            &TrackedFiles::Unreadable("the repository uses a split index".to_string()),
            DEFAULT_SECRET_FILE_PATTERNS,
        );
        assert_eq!(unreadable.status, CheckStatus::Skip);
        assert!(
            unreadable.detail.contains("split index"),
            "{}",
            unreadable.detail
        );

        // No patterns: nothing was matched, which is not the same as clean.
        let no_patterns = judge(&tracked(&[".env"]), &[]);
        assert_eq!(no_patterns.status, CheckStatus::Skip);
        assert!(
            no_patterns.detail.contains("secret_file_patterns"),
            "{}",
            no_patterns.detail
        );
    }

    // SPEC-ONB-050 AC7
    #[test]
    fn configured_patterns_replace_the_defaults() {
        let files = tracked(&[".env", "terraform.tfstate"]);
        let custom = judge(&files, &["*.tfstate"]);
        assert_eq!(custom.status, CheckStatus::Fail);
        assert!(
            custom.detail.contains("`terraform.tfstate`"),
            "{}",
            custom.detail
        );
        assert!(
            !custom.detail.contains("`.env`"),
            "the default patterns no longer apply: {}",
            custom.detail
        );
        assert_eq!(
            judge(&tracked(&[".env"]), &["*.tfstate"]).status,
            CheckStatus::Pass
        );
    }

    #[test]
    fn a_long_list_of_matches_is_summarized() {
        let paths: Vec<String> = (0..25).map(|i| format!("dir/{i:02}.pem")).collect();
        let refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let check = judge(&tracked(&refs), DEFAULT_SECRET_FILE_PATTERNS);
        assert!(check.detail.contains("25 file(s)"), "{}", check.detail);
        assert!(check.detail.contains("`dir/19.pem`"), "{}", check.detail);
        assert!(!check.detail.contains("`dir/20.pem`"), "{}", check.detail);
        assert!(check.detail.contains("and 5 more"), "{}", check.detail);
    }
}

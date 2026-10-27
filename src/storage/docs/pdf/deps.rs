//! Third-party tool detection for PDF export (REQ-PDF-009).
//!
//! handoff-mcp never bundles Typst, Mermaid CLI or a headless Chrome
//! (REQ-PDF-009 AC4): it only *finds* what the user installed, checks the
//! version, and — when something is missing or too old — produces an error
//! that names the tool, the required version and how to install it
//! (REQ-PDF-009 AC3).
//!
//! | Tool                   | Binary                                  | License                      | Needed for              |
//! |------------------------|-----------------------------------------|------------------------------|-------------------------|
//! | Typst CLI              | `typst`                                 | Apache-2.0                   | every export (required) |
//! | Mermaid CLI            | `mmdc`                                  | MIT                          | Mermaid diagrams        |
//! | chrome-headless-shell  | `chrome-headless-shell` / Chrome family | Chromium (BSD-3-Clause) etc. | `mmdc` rendering        |
//!
//! Only Typst is required. Mermaid CLI and Chrome are optional: without them
//! Mermaid blocks degrade to code blocks with a warning (REQ-PDF-003 AC2), so
//! callers should use [`require`] for Typst and [`detect`] for the others.
//!
//! Each binary can be pinned with an environment variable
//! (`HANDOFF_TYPST_BIN`, `HANDOFF_MMDC_BIN`, `HANDOFF_CHROME_BIN`), which wins
//! over the `PATH` search.

use std::ffi::OsStr;
use std::fmt;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A third-party tool the PDF pipeline can call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tool {
    Typst,
    Mmdc,
    ChromeHeadlessShell,
}

/// A `major.minor.patch` version. Missing components parse as 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
}

impl Version {
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
        }
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

/// Static description of a [`Tool`].
#[derive(Debug)]
pub struct ToolSpec {
    pub display_name: &'static str,
    /// Executable names tried in order on `PATH`.
    pub binaries: &'static [&'static str],
    /// Environment variable that pins the executable path.
    pub env_var: &'static str,
    /// Arguments that make the tool print its version.
    pub version_args: &'static [&'static str],
    /// Minimum supported version; `None` = any version is accepted.
    pub min_version: Option<Version>,
    pub license: &'static str,
    /// `true`: the export cannot proceed without it. `false`: the pipeline
    /// degrades gracefully (with a warning) when it is absent.
    pub required: bool,
    /// Human-readable install instructions, one per line.
    pub install: &'static str,
}

const TYPST: ToolSpec = ToolSpec {
    display_name: "Typst CLI",
    binaries: &["typst"],
    env_var: "HANDOFF_TYPST_BIN",
    version_args: &["--version"],
    min_version: Some(Version::new(0, 12, 0)),
    license: "Apache-2.0",
    required: true,
    install: "  macOS:   brew install typst\n  Linux:   snap install typst, or download a release from https://github.com/typst/typst/releases\n  Windows: winget install --id Typst.Typst\n  Any OS:  cargo install --locked typst-cli",
};

const MMDC: ToolSpec = ToolSpec {
    display_name: "Mermaid CLI (mmdc)",
    binaries: &["mmdc"],
    env_var: "HANDOFF_MMDC_BIN",
    version_args: &["--version"],
    min_version: Some(Version::new(10, 0, 0)),
    license: "MIT",
    required: false,
    install: "  npm install -g @mermaid-js/mermaid-cli\n  (mmdc drives a headless Chrome: see chrome-headless-shell below)",
};

const CHROME: ToolSpec = ToolSpec {
    display_name: "chrome-headless-shell",
    binaries: &[
        "chrome-headless-shell",
        "google-chrome",
        "google-chrome-stable",
        "chromium",
        "chromium-browser",
    ],
    env_var: "HANDOFF_CHROME_BIN",
    version_args: &["--version"],
    min_version: None,
    license: "Chromium (BSD-3-Clause and bundled third-party licenses)",
    required: false,
    install: "  npx puppeteer browsers install chrome-headless-shell\n  or install Google Chrome / Chromium with your OS package manager",
};

impl Tool {
    pub const ALL: [Tool; 3] = [Tool::Typst, Tool::Mmdc, Tool::ChromeHeadlessShell];

    pub fn spec(self) -> &'static ToolSpec {
        match self {
            Tool::Typst => &TYPST,
            Tool::Mmdc => &MMDC,
            Tool::ChromeHeadlessShell => &CHROME,
        }
    }
}

/// Outcome of looking for one tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolStatus {
    /// Usable. `version` is `None` when the tool ran but its output held no
    /// parsable version; that cannot be checked against the minimum, so the
    /// tool is accepted rather than rejected on a guess.
    Found {
        path: PathBuf,
        version: Option<Version>,
    },
    /// Found, but older than the supported minimum.
    TooOld {
        path: PathBuf,
        version: Version,
        min: Version,
    },
    /// An executable was located but could not be run to read its version.
    Unusable {
        path: PathBuf,
        reason: String,
    },
    NotFound,
}

/// A tool that is installed and usable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolInstall {
    pub path: PathBuf,
    pub version: Option<Version>,
}

/// Why [`require`] failed. `Display` is the user-facing message: it names the
/// tool, the required version and the install instructions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DepError {
    pub tool: Tool,
    pub status: ToolStatus,
}

impl fmt::Display for DepError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let spec = self.tool.spec();
        let min = spec
            .min_version
            .map_or_else(|| "any version".to_string(), |v| format!("{v} or newer"));
        match &self.status {
            ToolStatus::NotFound => write!(
                f,
                "{} is not installed (required: {min}; binary: {}; override with ${}).",
                spec.display_name,
                spec.binaries.join(" | "),
                spec.env_var
            )?,
            ToolStatus::TooOld {
                path,
                version,
                min: required,
            } => write!(
                f,
                "{} {version} at {} is too old (required: {required} or newer).",
                spec.display_name,
                path.display()
            )?,
            ToolStatus::Unusable { path, reason } => write!(
                f,
                "{} at {} could not be run: {reason} (required: {min}).",
                spec.display_name,
                path.display()
            )?,
            ToolStatus::Found { .. } => write!(f, "{} is available.", spec.display_name)?,
        }
        write!(f, "\nInstall / upgrade:\n{}", spec.install)
    }
}

impl std::error::Error for DepError {}

/// Extract the first `X.Y[.Z]` version from a tool's `--version` output
/// (`typst 0.13.1 (abc)`, `11.16.0`, `Google Chrome 150.0.7871.46`,
/// `HeadlessChrome/120.0.1`).
pub fn parse_version(output: &str) -> Option<Version> {
    output
        .split(|c: char| c.is_whitespace() || c == '/' || c == '(' || c == ')')
        .find_map(|token| {
            let token = token.trim_start_matches('v');
            let mut parts = token.split('.');
            let major = parts.next()?.parse().ok()?;
            // A bare integer ("12") is not a version; require at least X.Y.
            let minor = parts.next()?.parse().ok()?;
            let patch = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            Some(Version::new(major, minor, patch))
        })
}

/// Search `path_var` (a `PATH`-style list) for the first executable called
/// one of `names`. Names are tried in order, directories in order within each
/// name, so an earlier name always wins over a later one.
pub fn find_in_path(names: &[&str], path_var: &OsStr) -> Option<PathBuf> {
    let suffixes: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let dirs: Vec<PathBuf> = std::env::split_paths(path_var).collect();
    for name in names {
        for dir in &dirs {
            for suffix in suffixes {
                let candidate = dir.join(format!("{name}{suffix}"));
                if is_executable(&candidate) {
                    return Some(candidate);
                }
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// Locate and version-check `tool` using the process environment
/// (`PATH` and the tool's override variable).
pub fn detect(tool: Tool) -> ToolStatus {
    let spec = tool.spec();
    let path_var = std::env::var_os("PATH").unwrap_or_default();
    detect_in(tool, &path_var, std::env::var_os(spec.env_var).as_deref())
}

/// [`detect`] with the environment passed in, for deterministic tests and for
/// callers that resolve configuration themselves. `override_path`, when set
/// and non-empty, is used instead of searching `path_var` (a bare name in it
/// is still looked up on `path_var`).
pub fn detect_in(tool: Tool, path_var: &OsStr, override_path: Option<&OsStr>) -> ToolStatus {
    let spec = tool.spec();
    let located = match override_path.filter(|p| !p.is_empty()) {
        Some(p) => {
            let p = Path::new(p);
            // `Path::components` drops a leading `./`, so test the raw text.
            if p.to_string_lossy().contains(std::path::is_separator) {
                if is_executable(p) {
                    Some(p.to_path_buf())
                } else {
                    return ToolStatus::Unusable {
                        path: p.to_path_buf(),
                        reason: format!(
                            "${} points to a file that is not executable",
                            spec.env_var
                        ),
                    };
                }
            } else {
                find_in_path(&[&p.to_string_lossy()], path_var)
            }
        }
        None => find_in_path(spec.binaries, path_var),
    };
    let Some(path) = located else {
        return ToolStatus::NotFound;
    };
    classify(spec, path)
}

fn classify(spec: &ToolSpec, path: PathBuf) -> ToolStatus {
    let output = match Command::new(&path).args(spec.version_args).output() {
        Ok(o) => o,
        Err(e) => {
            return ToolStatus::Unusable {
                path,
                reason: e.to_string(),
            }
        }
    };
    if !output.status.success() {
        return ToolStatus::Unusable {
            path,
            reason: format!("`--version` exited with {}", output.status),
        };
    }
    // Some tools print the version on stderr; read both.
    let mut text = String::from_utf8_lossy(&output.stdout).into_owned();
    text.push('\n');
    text.push_str(&String::from_utf8_lossy(&output.stderr));
    let version = parse_version(&text);
    match (version, spec.min_version) {
        (Some(version), Some(min)) if version < min => ToolStatus::TooOld { path, version, min },
        _ => ToolStatus::Found { path, version },
    }
}

/// [`detect`] `tool`, turning anything but a usable install into a
/// [`DepError`] whose message carries the install instructions.
pub fn require(tool: Tool) -> Result<ToolInstall, DepError> {
    into_result(tool, detect(tool))
}

fn into_result(tool: Tool, status: ToolStatus) -> Result<ToolInstall, DepError> {
    match status {
        ToolStatus::Found { path, version } => Ok(ToolInstall { path, version }),
        status => Err(DepError { tool, status }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    #[test]
    fn parse_version_handles_each_tools_output() {
        assert_eq!(
            parse_version("typst 0.13.1 (8ace67d9)"),
            Some(Version::new(0, 13, 1))
        );
        assert_eq!(parse_version("11.16.0\n"), Some(Version::new(11, 16, 0)));
        assert_eq!(
            parse_version("Google Chrome 150.0.7871.46 "),
            Some(Version::new(150, 0, 7871))
        );
        assert_eq!(
            parse_version("HeadlessChrome/120.0.6099.109"),
            Some(Version::new(120, 0, 6099))
        );
        assert_eq!(parse_version("typst v0.12"), Some(Version::new(0, 12, 0)));
    }

    #[test]
    fn parse_version_rejects_output_without_a_version() {
        assert_eq!(parse_version(""), None);
        assert_eq!(parse_version("no version here"), None);
        assert_eq!(parse_version("build 12"), None);
    }

    #[test]
    fn versions_order_numerically_not_lexically() {
        assert!(Version::new(0, 9, 0) < Version::new(0, 12, 0));
        assert!(Version::new(10, 0, 0) > Version::new(9, 99, 99));
        assert_eq!(Version::new(0, 12, 0).to_string(), "0.12.0");
    }

    #[test]
    fn license_and_requirement_table_matches_requirements() {
        assert_eq!(Tool::Typst.spec().license, "Apache-2.0");
        assert_eq!(Tool::Mmdc.spec().license, "MIT");
        assert!(Tool::ChromeHeadlessShell
            .spec()
            .license
            .contains("Chromium"));
        assert!(Tool::Typst.spec().required);
        assert!(!Tool::Mmdc.spec().required);
        assert!(Tool::Typst.spec().min_version.is_some());
        assert!(Tool::Mmdc.spec().min_version.is_some());
    }

    #[test]
    fn not_found_error_names_tool_version_and_install_steps() {
        let err = DepError {
            tool: Tool::Typst,
            status: ToolStatus::NotFound,
        };
        let msg = err.to_string();
        assert!(msg.contains("Typst CLI"), "{msg}");
        assert!(msg.contains("0.12.0 or newer"), "{msg}");
        assert!(msg.contains("brew install typst"), "{msg}");
        assert!(msg.contains("HANDOFF_TYPST_BIN"), "{msg}");
    }

    #[test]
    fn mmdc_error_carries_npm_install_command() {
        let msg = DepError {
            tool: Tool::Mmdc,
            status: ToolStatus::NotFound,
        }
        .to_string();
        assert!(
            msg.contains("npm install -g @mermaid-js/mermaid-cli"),
            "{msg}"
        );
    }

    #[test]
    fn too_old_error_shows_found_and_required_versions() {
        let msg = DepError {
            tool: Tool::Typst,
            status: ToolStatus::TooOld {
                path: PathBuf::from("/usr/bin/typst"),
                version: Version::new(0, 9, 0),
                min: Version::new(0, 12, 0),
            },
        }
        .to_string();
        assert!(msg.contains("0.9.0"), "{msg}");
        assert!(msg.contains("0.12.0"), "{msg}");
        assert!(msg.contains("/usr/bin/typst"), "{msg}");
        assert!(msg.contains("Install / upgrade"), "{msg}");
    }

    #[test]
    fn into_result_maps_statuses() {
        let found = ToolStatus::Found {
            path: PathBuf::from("/x/typst"),
            version: Some(Version::new(0, 13, 0)),
        };
        assert_eq!(
            into_result(Tool::Typst, found).unwrap().path,
            PathBuf::from("/x/typst")
        );
        let err = into_result(Tool::Typst, ToolStatus::NotFound).unwrap_err();
        assert_eq!(err.status, ToolStatus::NotFound);
    }

    #[test]
    fn find_in_path_returns_none_for_empty_path() {
        assert_eq!(find_in_path(&["typst"], OsStr::new("")), None);
    }

    #[cfg(unix)]
    mod unix {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        fn fake_tool(dir: &Path, name: &str, script_body: &str) -> PathBuf {
            let p = dir.join(name);
            std::fs::write(&p, format!("#!/bin/sh\n{script_body}\n")).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            p
        }

        fn path_of(dir: &Path) -> OsString {
            dir.as_os_str().to_owned()
        }

        #[test]
        fn detects_tool_on_path_and_reads_version() {
            let dir = tempfile::tempdir().unwrap();
            let bin = fake_tool(dir.path(), "typst", "echo 'typst 0.13.1 (abc)'");
            let status = detect_in(Tool::Typst, &path_of(dir.path()), None);
            assert_eq!(
                status,
                ToolStatus::Found {
                    path: bin,
                    version: Some(Version::new(0, 13, 1))
                }
            );
        }

        #[test]
        fn missing_tool_is_not_found() {
            let dir = tempfile::tempdir().unwrap();
            assert_eq!(
                detect_in(Tool::Typst, &path_of(dir.path()), None),
                ToolStatus::NotFound
            );
        }

        #[test]
        fn old_version_is_too_old() {
            let dir = tempfile::tempdir().unwrap();
            let bin = fake_tool(dir.path(), "typst", "echo 'typst 0.9.0'");
            assert_eq!(
                detect_in(Tool::Typst, &path_of(dir.path()), None),
                ToolStatus::TooOld {
                    path: bin,
                    version: Version::new(0, 9, 0),
                    min: Version::new(0, 12, 0)
                }
            );
        }

        #[test]
        fn minimum_version_itself_is_accepted() {
            let dir = tempfile::tempdir().unwrap();
            fake_tool(dir.path(), "typst", "echo 'typst 0.12.0'");
            assert!(matches!(
                detect_in(Tool::Typst, &path_of(dir.path()), None),
                ToolStatus::Found { .. }
            ));
        }

        #[test]
        fn unparsable_version_is_accepted_without_a_version() {
            let dir = tempfile::tempdir().unwrap();
            fake_tool(dir.path(), "typst", "echo 'dev build'");
            assert!(matches!(
                detect_in(Tool::Typst, &path_of(dir.path()), None),
                ToolStatus::Found { version: None, .. }
            ));
        }

        #[test]
        fn failing_version_command_is_unusable() {
            let dir = tempfile::tempdir().unwrap();
            fake_tool(dir.path(), "typst", "exit 3");
            assert!(matches!(
                detect_in(Tool::Typst, &path_of(dir.path()), None),
                ToolStatus::Unusable { .. }
            ));
        }

        #[test]
        fn version_on_stderr_is_read() {
            let dir = tempfile::tempdir().unwrap();
            fake_tool(dir.path(), "mmdc", "echo '11.4.2' >&2");
            assert!(matches!(
                detect_in(Tool::Mmdc, &path_of(dir.path()), None),
                ToolStatus::Found { version: Some(v), .. } if v == Version::new(11, 4, 2)
            ));
        }

        #[test]
        fn non_executable_file_is_not_found() {
            let dir = tempfile::tempdir().unwrap();
            std::fs::write(dir.path().join("typst"), "#!/bin/sh\necho 'typst 0.13.0'\n").unwrap();
            assert_eq!(
                detect_in(Tool::Typst, &path_of(dir.path()), None),
                ToolStatus::NotFound
            );
        }

        #[test]
        fn override_path_wins_over_path_search() {
            let on_path = tempfile::tempdir().unwrap();
            let pinned = tempfile::tempdir().unwrap();
            fake_tool(on_path.path(), "typst", "echo 'typst 0.12.0'");
            let pinned_bin = fake_tool(pinned.path(), "my-typst", "echo 'typst 0.14.0'");
            let status = detect_in(
                Tool::Typst,
                &path_of(on_path.path()),
                Some(pinned_bin.as_os_str()),
            );
            assert_eq!(
                status,
                ToolStatus::Found {
                    path: pinned_bin,
                    version: Some(Version::new(0, 14, 0))
                }
            );
        }

        #[test]
        fn override_pointing_at_missing_file_is_unusable_and_names_the_variable() {
            let dir = tempfile::tempdir().unwrap();
            let status = detect_in(
                Tool::Typst,
                &path_of(dir.path()),
                Some(dir.path().join("nope").as_os_str()),
            );
            match status {
                ToolStatus::Unusable { reason, .. } => {
                    assert!(reason.contains("HANDOFF_TYPST_BIN"), "{reason}")
                }
                other => panic!("expected Unusable, got {other:?}"),
            }
        }

        #[test]
        fn bare_name_override_is_resolved_on_path() {
            let dir = tempfile::tempdir().unwrap();
            let bin = fake_tool(dir.path(), "typst-nightly", "echo 'typst 0.15.0'");
            let status = detect_in(
                Tool::Typst,
                &path_of(dir.path()),
                Some(OsStr::new("typst-nightly")),
            );
            assert!(matches!(status, ToolStatus::Found { path, .. } if path == bin));
        }

        #[test]
        fn dot_slash_override_is_a_path_not_a_bare_name() {
            let on_path = tempfile::tempdir().unwrap();
            fake_tool(on_path.path(), "typst", "echo 'typst 0.13.0'");
            let status = detect_in(
                Tool::Typst,
                &path_of(on_path.path()),
                Some(OsStr::new("./definitely-not-here/typst")),
            );
            assert!(matches!(status, ToolStatus::Unusable { .. }), "{status:?}");
        }

        #[test]
        fn empty_override_is_ignored() {
            let dir = tempfile::tempdir().unwrap();
            fake_tool(dir.path(), "typst", "echo 'typst 0.13.0'");
            assert!(matches!(
                detect_in(Tool::Typst, &path_of(dir.path()), Some(OsStr::new(""))),
                ToolStatus::Found { .. }
            ));
        }

        #[test]
        fn chrome_prefers_headless_shell_over_full_chrome() {
            let dir = tempfile::tempdir().unwrap();
            fake_tool(
                dir.path(),
                "google-chrome",
                "echo 'Google Chrome 150.0.1.2'",
            );
            let shell = fake_tool(
                dir.path(),
                "chrome-headless-shell",
                "echo 'HeadlessChrome 131.0.1.2'",
            );
            let status = detect_in(Tool::ChromeHeadlessShell, &path_of(dir.path()), None);
            assert!(matches!(status, ToolStatus::Found { path, .. } if path == shell));
        }
    }
}

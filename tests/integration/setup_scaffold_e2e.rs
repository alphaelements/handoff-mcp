//! Real-binary E2E for `handoff-mcp setup`'s CLAUDE.md project sections and
//! `[commands]` generation (SPEC-ONB-001, SPEC-ONB-093).
//!
//! Every test runs the built `handoff-mcp` binary in a temporary project with
//! a temporary `HOME`, so neither this repository nor the developer's
//! `~/.claude/settings.json` is touched.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use handoff_mcp::storage::config::COMMAND_KEYS;

const TODO: &str = "TODO(handoff)";
const SCAFFOLD_TITLES: [&str; 4] = [
    "Build & Test",
    "Coding Rules",
    "Project Structure",
    "Documentation Contract",
];

struct Sandbox {
    _root: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

struct Output {
    stdout: String,
    stderr: String,
    code: i32,
}

impl Sandbox {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let home = root.path().join("home");
        let project = root.path().join("project");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Self {
            _root: root,
            home,
            project,
        }
    }

    fn with_files(files: &[(&str, &str)]) -> Self {
        let sandbox = Self::new();
        for (name, content) in files {
            sandbox.write(name, content);
        }
        sandbox
    }

    fn write(&self, name: &str, content: &str) {
        std::fs::write(self.project.join(name), content).unwrap();
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_handoff-mcp"));
        command
            .args(args)
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .env_remove("USERPROFILE")
            // Set inside Claude Code; would redirect `init` to another project.
            .env_remove("CLAUDE_PROJECT_DIR")
            .env_remove("GIT_DIR")
            .env_remove("GIT_WORK_TREE")
            .env_remove("GIT_INDEX_FILE");
        command
    }

    fn run(&self, args: &[&str]) -> Output {
        self.run_with_stdin(args, "")
    }

    fn run_with_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .command(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("failed to start handoff-mcp");
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        Output {
            stdout: String::from_utf8_lossy(&output.stdout).to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).to_string(),
            code: output.status.code().unwrap_or(-1),
        }
    }

    /// `handoff-mcp setup <args>`, asserting it succeeds.
    fn setup(&self, args: &[&str]) -> Output {
        let mut full = vec!["setup"];
        full.extend_from_slice(args);
        let output = self.run(&full);
        assert_eq!(
            output.code, 0,
            "setup {args:?} failed\nstdout: {}\nstderr: {}",
            output.stdout, output.stderr
        );
        output
    }

    /// Initializes `.handoff/` through the real CLI.
    fn init(&self) {
        let project = self.project.to_string_lossy().to_string();
        let output = self.run(&["init", "--project-name", "demo", "--project-dir", &project]);
        assert_eq!(output.code, 0, "init failed: {}", output.stdout);
        assert!(self.config_path().exists());
    }

    fn config_path(&self) -> PathBuf {
        self.project.join(".handoff").join("config.toml")
    }

    fn claude_md_path(&self) -> PathBuf {
        self.project.join("CLAUDE.md")
    }

    /// CLAUDE.md with `\n` line endings. (A binary built from a checkout that
    /// converts line endings embeds CRLF templates; what these tests check is
    /// the content.)
    fn claude_md(&self) -> String {
        std::fs::read_to_string(self.claude_md_path())
            .expect("CLAUDE.md")
            .replace("\r\n", "\n")
    }

    fn config_text(&self) -> String {
        std::fs::read_to_string(self.config_path()).expect("config.toml")
    }

    fn config(&self) -> toml::Value {
        toml::from_str(&self.config_text()).expect("config.toml parses")
    }

    fn config_command(&self, key: &str) -> Option<String> {
        self.config()
            .get("commands")
            .and_then(|commands| commands.get(key))
            .and_then(toml::Value::as_str)
            .map(String::from)
    }
}

/// The text of the `## <title>` section of a CLAUDE.md, up to the next `## `.
fn section<'a>(claude_md: &'a str, title: &str) -> &'a str {
    let heading = format!("## {title}\n");
    let start = claude_md
        .find(&heading)
        .unwrap_or_else(|| panic!("no `## {title}` section in:\n{claude_md}"));
    let body = &claude_md[start + heading.len()..];
    let end = body.find("\n## ").map_or(body.len(), |i| i + 1);
    &claude_md[start..start + heading.len() + end]
}

/// The value of the `- <key>: ...` field of the Build & Test section.
fn build_test_field(claude_md: &str, key: &str) -> String {
    let prefix = format!("- {key}: ");
    section(claude_md, "Build & Test")
        .lines()
        .find_map(|line| line.strip_prefix(&prefix))
        .unwrap_or_else(|| panic!("no `{key}` field in Build & Test:\n{claude_md}"))
        .to_string()
}

fn assert_all_build_test_fields_are_todo(claude_md: &str) {
    for key in COMMAND_KEYS {
        assert_eq!(build_test_field(claude_md, key), TODO, "{key}");
    }
}

fn heading_count(claude_md: &str, title: &str) -> usize {
    let wanted = title.to_lowercase();
    claude_md
        .lines()
        .filter(|line| line.trim_start_matches('#').trim().to_lowercase() == wanted)
        .filter(|line| line.starts_with('#'))
        .count()
}

fn modified(path: &Path) -> std::time::SystemTime {
    std::fs::metadata(path).unwrap().modified().unwrap()
}

// --- SPEC-ONB-001 AC1 ---

#[test]
fn ac1_sections_are_appended_and_the_existing_body_is_byte_identical() {
    let sandbox = Sandbox::new();
    let original =
        "# My App\n\nNotes that must survive.\n\n- item\n\n## Session Handoff\n\nCustom text.\n";
    sandbox.write("CLAUDE.md", original);

    sandbox.setup(&["-y"]);

    let after = std::fs::read(sandbox.claude_md_path()).unwrap();
    assert!(
        after.starts_with(original.as_bytes()),
        "the existing body changed:\n{}",
        String::from_utf8_lossy(&after)
    );
    let after = sandbox.claude_md();
    for title in SCAFFOLD_TITLES {
        assert_eq!(heading_count(&after, title), 1, "{title}");
        assert!(after[original.len()..].contains(&format!("## {title}\n")));
    }
    assert_eq!(heading_count(&after, "Session Handoff"), 1);
}

// --- SPEC-ONB-001 AC2 ---

#[test]
fn ac2_only_the_missing_sections_are_appended() {
    let sandbox = Sandbox::with_files(&[("Cargo.toml", "[package]\nname = \"demo\"\n")]);
    let original = "# App\n\n## Session Handoff\n\nx\n\n## BUILD & TEST\n\nmake everything\n\n### coding rules\n\nTabs.\n";
    sandbox.write("CLAUDE.md", original);

    let output = sandbox.setup(&["-y"]);

    let after = sandbox.claude_md();
    assert!(after.starts_with(original));
    assert_eq!(heading_count(&after, "Build & Test"), 1);
    assert_eq!(heading_count(&after, "Coding Rules"), 1);
    assert_eq!(heading_count(&after, "Project Structure"), 1);
    assert_eq!(heading_count(&after, "Documentation Contract"), 1);
    assert!(
        !after.contains("cargo test"),
        "the existing Build & Test section must not be refilled:\n{after}"
    );
    assert!(
        output
            .stdout
            .contains("Project Structure, Documentation Contract"),
        "{}",
        output.stdout
    );
}

// --- SPEC-ONB-001 AC3 ---

#[test]
fn ac3_package_json_test_script_lands_in_commands_and_in_build_and_test() {
    let sandbox = Sandbox::with_files(&[(
        "package.json",
        r#"{"name": "demo", "scripts": {"test": "vitest run", "build": "tsc -p ."}}"#,
    )]);
    sandbox.init();

    sandbox.setup(&["-y"]);

    let configured = sandbox
        .config_command("test")
        .expect("[commands] test is written");
    assert_eq!(configured, "npm run test");
    let claude_md = sandbox.claude_md();
    assert_eq!(
        build_test_field(&claude_md, "test"),
        format!("`{configured}`"),
        "Build & Test shows the same command as [commands] test"
    );
    assert_eq!(
        sandbox.config_command("build").as_deref(),
        Some("npm run build")
    );
    assert_eq!(build_test_field(&claude_md, "build"), "`npm run build`");
    for key in ["install", "lint", "format", "typecheck", "smoke", "audit"] {
        assert_eq!(sandbox.config_command(key), None, "{key}");
        assert_eq!(build_test_field(&claude_md, key), TODO, "{key}");
    }
}

#[test]
fn cargo_project_gets_cargo_commands_in_both_places() {
    let sandbox = Sandbox::with_files(&[(
        "Cargo.toml",
        "[package]\nname = \"demo\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )]);
    sandbox.init();

    sandbox.setup(&["-y"]);

    let claude_md = sandbox.claude_md();
    for (key, command) in [
        ("build", "cargo build"),
        ("test", "cargo test"),
        ("lint", "cargo clippy"),
        ("format", "cargo fmt"),
    ] {
        assert_eq!(sandbox.config_command(key).as_deref(), Some(command));
        assert_eq!(build_test_field(&claude_md, key), format!("`{command}`"));
    }
    for key in ["install", "typecheck", "smoke", "audit"] {
        assert_eq!(sandbox.config_command(key), None, "{key}");
        assert_eq!(build_test_field(&claude_md, key), TODO, "{key}");
    }
}

// --- SPEC-ONB-001 AC4 ---

#[test]
fn ac4_no_manifest_marks_every_field_and_creates_no_commands_section() {
    let sandbox = Sandbox::new();
    sandbox.init();
    let config_before = sandbox.config_text();

    sandbox.setup(&["-y"]);

    assert_all_build_test_fields_are_todo(&sandbox.claude_md());
    assert_eq!(
        sandbox.config_text(),
        config_before,
        "config.toml is untouched"
    );
    assert!(sandbox.config().get("commands").is_none());
}

// --- SPEC-ONB-001 AC5 ---

#[test]
fn ac5_a_second_run_changes_neither_claude_md_nor_config_toml() {
    for manifest in [
        Some((
            "package.json",
            r#"{"scripts": {"test": "jest", "lint": "eslint ."}}"#,
        )),
        Some(("Cargo.toml", "[package]\nname = \"demo\"\n")),
        None,
    ] {
        let sandbox = Sandbox::new();
        if let Some((name, content)) = manifest {
            sandbox.write(name, content);
        }
        sandbox.init();

        sandbox.setup(&["-y"]);
        let claude_md = std::fs::read(sandbox.claude_md_path()).unwrap();
        let config = std::fs::read(sandbox.config_path()).unwrap();
        let claude_md_time = modified(&sandbox.claude_md_path());
        let config_time = modified(&sandbox.config_path());
        let settings = std::fs::read(sandbox.home.join(".claude/settings.json")).unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        let second = sandbox.setup(&["-y"]);

        assert_eq!(
            std::fs::read(sandbox.claude_md_path()).unwrap(),
            claude_md,
            "{manifest:?}"
        );
        assert_eq!(
            std::fs::read(sandbox.config_path()).unwrap(),
            config,
            "{manifest:?}"
        );
        assert_eq!(
            modified(&sandbox.claude_md_path()),
            claude_md_time,
            "CLAUDE.md was rewritten ({manifest:?})"
        );
        assert_eq!(
            modified(&sandbox.config_path()),
            config_time,
            "config.toml was rewritten ({manifest:?})"
        );
        assert_eq!(
            std::fs::read(sandbox.home.join(".claude/settings.json")).unwrap(),
            settings
        );
        assert!(
            second
                .stdout
                .contains("CLAUDE.md: project sections already present"),
            "{}",
            second.stdout
        );
    }
}

// --- SPEC-ONB-001 AC6 ---

#[test]
fn ac6_a_missing_claude_md_is_created_with_the_title_and_five_sections() {
    let sandbox = Sandbox::new();
    assert!(!sandbox.claude_md_path().exists());

    sandbox.setup(&["-y"]);

    let claude_md = sandbox.claude_md();
    assert!(claude_md.starts_with("# CLAUDE.md\n"), "{claude_md}");
    let headings: Vec<&str> = claude_md
        .lines()
        .filter(|line| line.starts_with("## "))
        .collect();
    assert_eq!(
        headings,
        vec![
            "## Session Handoff",
            "## Build & Test",
            "## Coding Rules",
            "## Project Structure",
            "## Documentation Contract",
        ]
    );
    assert_eq!(claude_md.lines().filter(|l| l.starts_with("# ")).count(), 1);
}

// --- SPEC-ONB-093 AC1 ---

#[test]
fn spec093_ac1_makefile_only_project_succeeds_with_every_field_marked() {
    let sandbox = Sandbox::with_files(&[
        ("Makefile", "test:\n\t@echo ok\n"),
        ("build.gradle", "plugins { id 'java' }\n"),
    ]);
    sandbox.init();

    let output = sandbox.setup(&["-y"]);

    assert_eq!(output.code, 0);
    assert_all_build_test_fields_are_todo(&sandbox.claude_md());
    assert!(sandbox.config().get("commands").is_none());
    assert!(
        output.stdout.contains("[commands]: no commands detected"),
        "the user is told to write the commands by hand: {}",
        output.stdout
    );
    assert!(
        output.stdout.contains("[commands]") && output.stdout.contains("config.toml"),
        "{}",
        output.stdout
    );
}

/// SPEC-ONB-093: a hand-written `[commands]` is the source of truth. setup
/// copies it into Build & Test and never replaces it with a guess.
#[test]
fn hand_written_commands_are_kept_and_copied_into_build_and_test() {
    let sandbox = Sandbox::with_files(&[
        ("Makefile", "test:\n\t@echo ok\n"),
        ("package.json", r#"{"scripts": {"test": "jest"}}"#),
    ]);
    sandbox.init();
    let mut config = sandbox.config_text();
    config.push_str("\n[commands]\ntest = \"make test\"\nsmoke = \"make smoke | tee smoke.log\"\n");
    std::fs::write(sandbox.config_path(), &config).unwrap();

    sandbox.setup(&["-y"]);

    assert_eq!(sandbox.config_text(), config, "config.toml is untouched");
    let claude_md = sandbox.claude_md();
    assert_eq!(build_test_field(&claude_md, "test"), "`make test`");
    assert_eq!(
        build_test_field(&claude_md, "smoke"),
        "`make smoke | tee smoke.log`"
    );
    assert_eq!(build_test_field(&claude_md, "build"), TODO);
}

#[test]
fn unparsable_manifests_are_reported_and_do_not_fail_setup() {
    let sandbox = Sandbox::with_files(&[
        ("package.json", "{ this is not json"),
        ("pyproject.toml", "[tool.pytest.ini_options]\n"),
    ]);
    sandbox.init();

    let output = sandbox.setup(&["-y"]);

    assert!(
        output
            .stdout
            .contains("package.json: could not be parsed, ignored"),
        "{}",
        output.stdout
    );
    assert_eq!(sandbox.config_command("test").as_deref(), Some("pytest"));
    assert_eq!(build_test_field(&sandbox.claude_md(), "test"), "`pytest`");
}

// --- project state around setup ---

#[test]
fn an_uninitialized_project_gets_the_sections_but_no_handoff_directory() {
    let sandbox = Sandbox::with_files(&[("Cargo.toml", "[package]\nname = \"demo\"\n")]);

    let output = sandbox.setup(&["-y"]);

    assert!(!sandbox.project.join(".handoff").exists());
    assert_eq!(
        build_test_field(&sandbox.claude_md(), "test"),
        "`cargo test`"
    );
    assert!(
        output.stdout.contains("handoff_init"),
        "the user is told how to get [commands] written: {}",
        output.stdout
    );
    assert!(
        output.stdout.contains("test = \"cargo test\""),
        "the commands that are waiting to be recorded are named: {}",
        output.stdout
    );

    // Once the project is initialized, a re-run records the same commands.
    sandbox.init();
    let claude_md = sandbox.claude_md();
    sandbox.setup(&["-y"]);
    assert_eq!(
        sandbox.config_command("test").as_deref(),
        Some("cargo test")
    );
    assert_eq!(sandbox.claude_md(), claude_md);
}

#[test]
fn a_broken_config_toml_is_reported_left_alone_and_does_not_block_the_sections() {
    let sandbox = Sandbox::with_files(&[("Cargo.toml", "[package]\nname = \"demo\"\n")]);
    std::fs::create_dir_all(sandbox.project.join(".handoff")).unwrap();
    let broken = "[project\nname = ";
    std::fs::write(sandbox.config_path(), broken).unwrap();

    let output = sandbox.setup(&["-y"]);

    assert_eq!(sandbox.config_text(), broken);
    assert!(
        output.stderr.contains("[commands]") && output.stderr.contains("config.toml"),
        "stderr: {}",
        output.stderr
    );
    assert_eq!(
        build_test_field(&sandbox.claude_md(), "test"),
        "`cargo test`"
    );
}

// --- flags ---

#[test]
fn claude_md_scaffold_flag_adds_only_the_four_sections_without_prompting() {
    let sandbox = Sandbox::with_files(&[("package.json", r#"{"scripts": {"test": "jest"}}"#)]);
    sandbox.init();
    sandbox.write("CLAUDE.md", "# App\n");

    // No `-y`, and a stdin that would answer "no" to any prompt.
    let output = sandbox.run_with_stdin(&["setup", "--claude-md-scaffold"], "n\nn\nn\nn\n");

    assert_eq!(output.code, 0, "{}{}", output.stdout, output.stderr);
    let claude_md = sandbox.claude_md();
    assert!(claude_md.starts_with("# App\n"));
    for title in SCAFFOLD_TITLES {
        assert_eq!(heading_count(&claude_md, title), 1, "{title}");
    }
    assert_eq!(heading_count(&claude_md, "Session Handoff"), 0);
    assert_eq!(
        sandbox.config_command("test").as_deref(),
        Some("npm run test")
    );
    assert!(
        !sandbox.home.join(".claude").exists(),
        "hooks are not installed"
    );
    assert!(!sandbox.project.join(".mcp.json").exists());
    assert!(!output.stdout.contains("[Y/n]"), "{}", output.stdout);
}

#[test]
fn claude_md_scaffold_flag_creates_a_titled_file_when_there_is_none() {
    let sandbox = Sandbox::new();
    sandbox.setup(&["--claude-md-scaffold"]);
    let claude_md = sandbox.claude_md();
    assert!(claude_md.starts_with("# CLAUDE.md\n\n## Build & Test\n"));
    assert_eq!(heading_count(&claude_md, "Session Handoff"), 0);
}

#[test]
fn answering_no_keeps_claude_md_and_config_toml_as_they_were() {
    let sandbox = Sandbox::with_files(&[("Cargo.toml", "[package]\nname = \"demo\"\n")]);
    sandbox.init();
    sandbox.write("CLAUDE.md", "# App\n");
    let config = sandbox.config_text();

    let output = sandbox.run_with_stdin(&["setup"], "n\nn\nn\nn\n");

    assert_eq!(output.code, 0, "{}{}", output.stdout, output.stderr);
    assert_eq!(sandbox.claude_md(), "# App\n");
    assert_eq!(sandbox.config_text(), config);
    assert!(!sandbox.project.join(".mcp.json").exists());
    assert_eq!(
        output.stdout.matches("[Y/n]").count(),
        4,
        ".mcp.json, Session Handoff, [commands], project sections: {}",
        output.stdout
    );
}

#[test]
fn interactive_yes_records_commands_and_adds_sections() {
    let sandbox = Sandbox::with_files(&[("Cargo.toml", "[package]\nname = \"demo\"\n")]);
    sandbox.init();

    let output = sandbox.run_with_stdin(&["setup"], "y\ny\ny\ny\n");

    assert_eq!(output.code, 0, "{}{}", output.stdout, output.stderr);
    assert_eq!(
        sandbox.config_command("test").as_deref(),
        Some("cargo test")
    );
    assert_eq!(
        build_test_field(&sandbox.claude_md(), "test"),
        "`cargo test`"
    );
}

#[test]
fn unknown_and_conflicting_flags_are_rejected_before_anything_is_written() {
    for args in [
        vec!["setup", "--claude-md-scafold"],
        vec!["setup", "-y", "extra"],
        vec!["setup", "--claude-md-scaffold", "--check"],
        vec!["setup", "--claude-md-scaffold", "--uninstall"],
        vec!["setup", "--claude-md-scaffold", "--mcp-json"],
        vec!["setup", "--check", "--uninstall"],
    ] {
        let sandbox = Sandbox::with_files(&[("Cargo.toml", "[package]\nname = \"demo\"\n")]);
        let output = sandbox.run(&args);
        assert_eq!(output.code, 1, "{args:?}: {}", output.stdout);
        assert!(
            output.stderr.contains("Error:"),
            "{args:?}: {}",
            output.stderr
        );
        assert!(!sandbox.claude_md_path().exists(), "{args:?}");
        assert!(!sandbox.home.join(".claude").exists(), "{args:?}");
        assert!(!sandbox.project.join(".mcp.json").exists(), "{args:?}");
    }
}

#[test]
fn setup_help_prints_usage_and_changes_nothing() {
    for flag in ["--help", "-h"] {
        let sandbox = Sandbox::new();
        let output = sandbox.run(&["setup", flag]);
        assert_eq!(output.code, 0);
        assert!(
            output.stdout.contains("--claude-md-scaffold"),
            "{}",
            output.stdout
        );
        assert!(!sandbox.claude_md_path().exists());
        assert!(!sandbox.home.join(".claude").exists());
    }
}

#[test]
fn top_level_help_lists_the_scaffold_flag() {
    let sandbox = Sandbox::new();
    let output = sandbox.run(&["--help"]);
    assert!(
        output.stdout.contains("setup --claude-md-scaffold"),
        "{}",
        output.stdout
    );
}

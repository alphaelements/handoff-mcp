//! Command inference for `handoff-mcp setup` (SPEC-ONB-001, SPEC-ONB-093).
//!
//! Reads the project's build manifests and proposes values for the
//! `[commands]` section of `.handoff/config.toml`. A proposal is only made for
//! a key the manifest actually declares, so a wrong guess never reaches the
//! config: a key with no evidence stays unset and the user fills it in.
//!
//! Layouts this module does not know (a `Makefile`, a Gradle build, ...) are
//! not an error. They simply yield no proposal, and a manifest that cannot be
//! parsed is skipped with a note.

use std::collections::BTreeMap;
use std::path::Path;

use serde_json::Value as JsonValue;

/// What [`infer_commands`] found.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct Inference {
    /// Proposed command per `[commands]` key.
    pub commands: BTreeMap<&'static str, String>,
    /// One line per manifest that exists but could not be used.
    pub notes: Vec<String>,
}

/// `package.json` script names that map one-to-one onto a `[commands]` key.
const PACKAGE_JSON_SCRIPT_KEYS: &[&str] = &["build", "test", "lint", "format", "typecheck"];

/// The `test` script `npm init` writes. It always exits 1, so it says "no
/// tests yet", not "this is the test command".
const NPM_INIT_TEST_PLACEHOLDER: &str = "no test specified";

/// Proposes `[commands]` values from the manifests in `project_dir`.
///
/// Sources are consulted in a fixed order (`package.json`, `Cargo.toml`,
/// `pyproject.toml`) and the first one that declares a key wins it.
pub(crate) fn infer_commands(project_dir: &Path) -> Inference {
    let mut inference = Inference::default();
    let sources: [(&str, ManifestReader); 3] = [
        ("package.json", from_package_json),
        ("Cargo.toml", from_cargo_toml),
        ("pyproject.toml", from_pyproject_toml),
    ];
    for (file_name, read) in sources {
        let path = project_dir.join(file_name);
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                inference
                    .notes
                    .push(format!("{file_name}: could not be read, ignored ({e})"));
                continue;
            }
        };
        match read(&text, project_dir) {
            Ok(found) => {
                for (key, command) in found {
                    inference.commands.entry(key).or_insert(command);
                }
            }
            Err(reason) => inference.notes.push(format!(
                "{file_name}: could not be parsed, ignored ({reason})"
            )),
        }
    }
    inference
}

type ManifestReader = fn(&str, &Path) -> Result<Vec<(&'static str, String)>, String>;

fn from_package_json(
    text: &str,
    project_dir: &Path,
) -> Result<Vec<(&'static str, String)>, String> {
    let manifest: JsonValue = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let Some(scripts) = manifest.get("scripts").and_then(JsonValue::as_object) else {
        return Ok(Vec::new());
    };
    let runner = node_package_manager(&manifest, project_dir);
    let mut found = Vec::new();
    for key in PACKAGE_JSON_SCRIPT_KEYS {
        let Some(script) = scripts.get(*key).and_then(JsonValue::as_str) else {
            continue;
        };
        if script.trim().is_empty() {
            continue;
        }
        if *key == "test" && script.contains(NPM_INIT_TEST_PLACEHOLDER) {
            continue;
        }
        found.push((*key, format!("{runner} run {key}")));
    }
    Ok(found)
}

/// The package manager that runs this project's scripts: the manifest's own
/// `packageManager` field, else whichever lockfile is present, else `npm`.
fn node_package_manager(manifest: &JsonValue, project_dir: &Path) -> &'static str {
    const KNOWN: &[&str] = &["pnpm", "yarn", "bun", "npm"];
    if let Some(declared) = manifest.get("packageManager").and_then(JsonValue::as_str) {
        let name = declared.split('@').next().unwrap_or("");
        if let Some(known) = KNOWN.iter().find(|k| **k == name) {
            return known;
        }
    }
    const LOCKFILES: &[(&str, &str)] = &[
        ("pnpm-lock.yaml", "pnpm"),
        ("yarn.lock", "yarn"),
        ("bun.lockb", "bun"),
        ("bun.lock", "bun"),
    ];
    LOCKFILES
        .iter()
        .find(|(lockfile, _)| project_dir.join(lockfile).exists())
        .map_or("npm", |(_, manager)| manager)
}

fn from_cargo_toml(text: &str, _project_dir: &Path) -> Result<Vec<(&'static str, String)>, String> {
    let manifest: toml::Value = toml::from_str(text).map_err(|e| single_line(&e.to_string()))?;
    if manifest.get("package").is_none() && manifest.get("workspace").is_none() {
        return Ok(Vec::new());
    }
    Ok(vec![
        ("build", "cargo build".to_string()),
        ("test", "cargo test".to_string()),
        ("lint", "cargo clippy".to_string()),
        ("format", "cargo fmt".to_string()),
    ])
}

fn from_pyproject_toml(
    text: &str,
    project_dir: &Path,
) -> Result<Vec<(&'static str, String)>, String> {
    let manifest: toml::Value = toml::from_str(text).map_err(|e| single_line(&e.to_string()))?;
    let prefix = python_runner_prefix(project_dir);
    let mut found = Vec::new();
    if python_tool_declared(&manifest, "pytest") {
        found.push(("test", format!("{prefix}pytest")));
    }
    if python_tool_declared(&manifest, "ruff") {
        found.push(("lint", format!("{prefix}ruff check .")));
        found.push(("format", format!("{prefix}ruff format .")));
    }
    Ok(found)
}

/// `uv run ` / `poetry run ` when the project is locked by that tool, so the
/// command runs inside the project's environment; empty otherwise.
fn python_runner_prefix(project_dir: &Path) -> &'static str {
    if project_dir.join("uv.lock").exists() {
        "uv run "
    } else if project_dir.join("poetry.lock").exists() {
        "poetry run "
    } else {
        ""
    }
}

/// Does `pyproject.toml` configure `tool` (a `[tool.<tool>]` table) or list it
/// as a dependency in any of the standard places?
fn python_tool_declared(manifest: &toml::Value, tool: &str) -> bool {
    if manifest.get("tool").and_then(|t| t.get(tool)).is_some() {
        return true;
    }
    let mut requirement_lists: Vec<&toml::Value> = Vec::new();
    if let Some(project) = manifest.get("project") {
        requirement_lists.extend(project.get("dependencies"));
        if let Some(groups) = project
            .get("optional-dependencies")
            .and_then(toml::Value::as_table)
        {
            requirement_lists.extend(groups.values());
        }
    }
    if let Some(groups) = manifest
        .get("dependency-groups")
        .and_then(toml::Value::as_table)
    {
        requirement_lists.extend(groups.values());
    }
    requirement_lists.extend(
        manifest
            .get("tool")
            .and_then(|t| t.get("uv"))
            .and_then(|uv| uv.get("dev-dependencies")),
    );
    let in_requirement_list = requirement_lists
        .iter()
        .filter_map(|list| list.as_array())
        .flatten()
        .filter_map(toml::Value::as_str)
        .any(|requirement| requirement_name(requirement) == tool);
    if in_requirement_list {
        return true;
    }

    // Poetry keeps dependencies as table keys instead of requirement strings.
    let Some(poetry) = manifest.get("tool").and_then(|t| t.get("poetry")) else {
        return false;
    };
    let mut poetry_tables: Vec<&toml::Value> = Vec::new();
    poetry_tables.extend(poetry.get("dependencies"));
    poetry_tables.extend(poetry.get("dev-dependencies"));
    if let Some(groups) = poetry.get("group").and_then(toml::Value::as_table) {
        poetry_tables.extend(groups.values().filter_map(|g| g.get("dependencies")));
    }
    poetry_tables
        .iter()
        .filter_map(|table| table.as_table())
        .any(|table| table.keys().any(|name| requirement_name(name) == tool))
}

/// The distribution name of a PEP 508 requirement (`pytest>=8 ; python_version
/// > "3.9"` → `pytest`), normalized the way package indexes compare names.
fn requirement_name(requirement: &str) -> String {
    requirement
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
        .map(|c| match c {
            '_' | '.' => '-',
            other => other.to_ascii_lowercase(),
        })
        .collect()
}

/// TOML parse errors span several lines; notes are printed one per line.
fn single_line(message: &str) -> String {
    message.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (name, content) in files {
            std::fs::write(dir.path().join(name), content).unwrap();
        }
        dir
    }

    fn commands(dir: &tempfile::TempDir) -> BTreeMap<&'static str, String> {
        infer_commands(dir.path()).commands
    }

    #[test]
    fn package_json_scripts_map_to_run_commands_only_for_declared_keys() {
        let dir = project(&[(
            "package.json",
            r#"{"scripts": {"test": "vitest run", "build": "tsc -p .", "dev": "vite"}}"#,
        )]);
        let found = commands(&dir);
        assert_eq!(found.get("test").map(String::as_str), Some("npm run test"));
        assert_eq!(
            found.get("build").map(String::as_str),
            Some("npm run build")
        );
        assert_eq!(
            found.keys().copied().collect::<Vec<_>>(),
            vec!["build", "test"],
            "a script with no [commands] counterpart (dev) and undeclared keys propose nothing"
        );
    }

    #[test]
    fn package_json_covers_all_five_script_keys() {
        let dir = project(&[(
            "package.json",
            r#"{"scripts": {"build":"b","test":"t","lint":"l","format":"f","typecheck":"c"}}"#,
        )]);
        let found = commands(&dir);
        for key in ["build", "test", "lint", "format", "typecheck"] {
            assert_eq!(found.get(key), Some(&format!("npm run {key}")), "{key}");
        }
        assert_eq!(found.len(), 5, "install / smoke / audit are never inferred");
    }

    #[test]
    fn npm_init_placeholder_test_script_is_not_a_test_command() {
        let dir = project(&[(
            "package.json",
            r#"{"scripts": {"test": "echo \"Error: no test specified\" && exit 1"}}"#,
        )]);
        assert!(commands(&dir).is_empty());
    }

    #[test]
    fn empty_or_non_string_scripts_propose_nothing() {
        let dir = project(&[(
            "package.json",
            r#"{"scripts": {"test": "  ", "build": 3, "lint": null}}"#,
        )]);
        assert!(commands(&dir).is_empty());
        let no_scripts = project(&[("package.json", r#"{"name": "x"}"#)]);
        assert!(commands(&no_scripts).is_empty());
        let scripts_not_object = project(&[("package.json", r#"{"scripts": ["test"]}"#)]);
        assert!(commands(&scripts_not_object).is_empty());
    }

    #[test]
    fn package_manager_follows_the_lockfile() {
        for (lockfile, manager) in [
            ("pnpm-lock.yaml", "pnpm"),
            ("yarn.lock", "yarn"),
            ("bun.lockb", "bun"),
            ("bun.lock", "bun"),
        ] {
            let dir = project(&[
                ("package.json", r#"{"scripts": {"test": "t"}}"#),
                (lockfile, ""),
            ]);
            assert_eq!(
                commands(&dir).get("test"),
                Some(&format!("{manager} run test")),
                "{lockfile}"
            );
        }
    }

    #[test]
    fn package_manager_field_wins_over_the_lockfile() {
        let dir = project(&[
            (
                "package.json",
                r#"{"packageManager": "pnpm@9.1.0", "scripts": {"test": "t"}}"#,
            ),
            ("yarn.lock", ""),
        ]);
        assert_eq!(
            commands(&dir).get("test").map(String::as_str),
            Some("pnpm run test")
        );
        let unknown = project(&[(
            "package.json",
            r#"{"packageManager": "deno@2", "scripts": {"test": "t"}}"#,
        )]);
        assert_eq!(
            commands(&unknown).get("test").map(String::as_str),
            Some("npm run test"),
            "an unknown manager falls back instead of producing a command that cannot run"
        );
    }

    #[test]
    fn cargo_toml_proposes_build_test_lint_format() {
        let dir = project(&[(
            "Cargo.toml",
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n",
        )]);
        let found = commands(&dir);
        assert_eq!(found.get("build").map(String::as_str), Some("cargo build"));
        assert_eq!(found.get("test").map(String::as_str), Some("cargo test"));
        assert_eq!(found.get("lint").map(String::as_str), Some("cargo clippy"));
        assert_eq!(found.get("format").map(String::as_str), Some("cargo fmt"));
        assert_eq!(found.len(), 4);

        let workspace = project(&[("Cargo.toml", "[workspace]\nmembers = [\"a\"]\n")]);
        assert_eq!(commands(&workspace).len(), 4);
    }

    #[test]
    fn cargo_toml_without_package_or_workspace_proposes_nothing() {
        let dir = project(&[("Cargo.toml", "[profile.release]\nlto = true\n")]);
        assert!(commands(&dir).is_empty());
    }

    #[test]
    fn pyproject_proposes_only_the_tools_it_declares() {
        let pytest_only = project(&[(
            "pyproject.toml",
            "[tool.pytest.ini_options]\ntestpaths = [\"tests\"]\n",
        )]);
        let found = commands(&pytest_only);
        assert_eq!(found.get("test").map(String::as_str), Some("pytest"));
        assert_eq!(found.len(), 1);

        let ruff_only = project(&[("pyproject.toml", "[tool.ruff]\nline-length = 100\n")]);
        let found = commands(&ruff_only);
        assert_eq!(found.get("lint").map(String::as_str), Some("ruff check ."));
        assert_eq!(
            found.get("format").map(String::as_str),
            Some("ruff format .")
        );
        assert_eq!(found.len(), 2);

        let neither = project(&[("pyproject.toml", "[project]\nname = \"x\"\n")]);
        assert!(commands(&neither).is_empty());
    }

    #[test]
    fn pyproject_finds_tools_listed_as_dependencies() {
        for manifest in [
            "[project]\nname = \"x\"\ndependencies = [\"pytest>=8\"]\n",
            "[project]\nname = \"x\"\n[project.optional-dependencies]\ndev = [\"PyTest ; python_version > '3.9'\"]\n",
            "[dependency-groups]\ndev = [\"pytest\"]\n",
            "[tool.uv]\ndev-dependencies = [\"pytest==8.3\"]\n",
            "[tool.poetry.group.dev.dependencies]\npytest = \"^8\"\n",
            "[tool.poetry.dev-dependencies]\npytest = \"^8\"\n",
        ] {
            let dir = project(&[("pyproject.toml", manifest)]);
            assert_eq!(
                commands(&dir).get("test").map(String::as_str),
                Some("pytest"),
                "{manifest}"
            );
        }
        // A plugin is not the tool itself.
        let plugin_only = project(&[(
            "pyproject.toml",
            "[project]\nname = \"x\"\ndependencies = [\"pytest-cov\", \"ruff_helper\"]\n",
        )]);
        assert!(commands(&plugin_only).is_empty());
    }

    #[test]
    fn python_commands_run_through_the_locking_tool() {
        let uv = project(&[
            ("pyproject.toml", "[tool.pytest]\n[tool.ruff]\n"),
            ("uv.lock", ""),
        ]);
        let found = commands(&uv);
        assert_eq!(found.get("test").map(String::as_str), Some("uv run pytest"));
        assert_eq!(
            found.get("lint").map(String::as_str),
            Some("uv run ruff check .")
        );
        let poetry = project(&[("pyproject.toml", "[tool.pytest]\n"), ("poetry.lock", "")]);
        assert_eq!(
            commands(&poetry).get("test").map(String::as_str),
            Some("poetry run pytest")
        );
    }

    #[test]
    fn first_source_that_declares_a_key_wins_it() {
        let dir = project(&[
            ("package.json", r#"{"scripts": {"test": "vitest"}}"#),
            ("Cargo.toml", "[package]\nname = \"x\"\n"),
            ("pyproject.toml", "[tool.pytest]\n[tool.ruff]\n"),
        ]);
        let found = commands(&dir);
        assert_eq!(found.get("test").map(String::as_str), Some("npm run test"));
        assert_eq!(found.get("build").map(String::as_str), Some("cargo build"));
        assert_eq!(found.get("lint").map(String::as_str), Some("cargo clippy"));
    }

    #[test]
    fn no_manifest_and_unsupported_layouts_propose_nothing_without_notes() {
        let empty = project(&[]);
        assert_eq!(infer_commands(empty.path()), Inference::default());
        let makefile = project(&[
            ("Makefile", "test:\n\ttrue\n"),
            ("build.gradle", "plugins {}\n"),
        ]);
        assert_eq!(infer_commands(makefile.path()), Inference::default());
    }

    #[test]
    fn malformed_manifests_are_skipped_with_a_note_and_others_still_apply() {
        let dir = project(&[
            ("package.json", "{ not json"),
            ("Cargo.toml", "[package\nname = "),
            ("pyproject.toml", "[tool.pytest]\n"),
        ]);
        let inference = infer_commands(dir.path());
        assert_eq!(
            inference.commands.get("test").map(String::as_str),
            Some("pytest")
        );
        assert_eq!(inference.commands.len(), 1);
        assert_eq!(inference.notes.len(), 2, "{:?}", inference.notes);
        assert!(inference.notes[0].starts_with("package.json: could not be parsed"));
        assert!(inference.notes[1].starts_with("Cargo.toml: could not be parsed"));
        assert!(
            inference.notes.iter().all(|n| !n.contains('\n')),
            "notes are printed one per line: {:?}",
            inference.notes
        );
    }

    #[test]
    fn a_manifest_that_is_a_directory_is_skipped_with_a_note() {
        let dir = project(&[]);
        std::fs::create_dir(dir.path().join("package.json")).unwrap();
        let inference = infer_commands(dir.path());
        assert!(inference.commands.is_empty());
        assert_eq!(inference.notes.len(), 1);
        assert!(inference.notes[0].starts_with("package.json: could not be read"));
    }
}

//! The `handoff-getting-started` skill (SPEC-ONB-012): the shortest path from
//! an empty directory to one finished task.
//!
//! The skill is prose an agent follows. These tests keep it true:
//!
//! - AC1: the steps of the skill, carried out from an empty directory against
//!   the real `handoff-mcp` binary, end in one task that is `done`. The three
//!   loop commands are conversations with a model, so the test plays the
//!   coordinator's part with the handoff calls each loop ends in; what it
//!   checks at every step is that the server itself (`handoff_next_step`)
//!   recommends the operation the skill names next.
//! - AC2: the skill never asks the reader to choose a profile.
//! - AC3: every command, tool, argument and plugin the skill names exists in
//!   this commit: in the tool list, the command files, the plugin manifests,
//!   the help of the real binary, or in what the binary printed and wrote
//!   during the walk.
//!
//! Unix only: the walk runs a shell test command.
#![cfg(unix)]

#[path = "support/onboarding_e2e.rs"]
mod support;

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use handoff_mcp::mcp::tools::all_tool_definitions;
use handoff_mcp::storage::config::SESSION_LOOP_PROFILES;
use handoff_mcp::trace::profile::BUILTIN_PROFILE_NAMES;
use serde_json::{json, Value};
use support::{Sandbox, Server};

const SKILL_NAME: &str = "handoff-getting-started";

/// The operation each step is headed by, in the order SPEC-ONB-012 fixes.
/// Step 1 (installation) is headed by a word, the others by the operation.
const STEP_OPERATIONS: [&str; 7] = [
    "Install",
    "handoff_init",
    "handoff-mcp setup",
    "handoff_doctor",
    "/brainstorm-loop",
    "/requirement-loop",
    "/session-loop",
];

/// Slash commands that belong to Claude Code itself, not to this repository.
const CLAUDE_CODE_COMMANDS: [&str; 2] = ["/plugin", "/reload-plugins"];

/// The name the binary is installed under.
const BINARY: &str = "handoff-mcp";

fn repo() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

fn read(relative: &str) -> String {
    std::fs::read_to_string(repo().join(relative))
        .unwrap_or_else(|e| panic!("cannot read {relative}: {e}"))
}

fn skill() -> String {
    read(&format!("skills/{SKILL_NAME}/SKILL.md"))
}

/// The skill without its frontmatter.
fn skill_body() -> String {
    let text = skill();
    let rest = text
        .strip_prefix("---\n")
        .expect("the skill starts with frontmatter");
    let (_, body) = rest
        .split_once("\n---\n")
        .expect("the frontmatter is closed");
    body.to_string()
}

// --- reading the skill ---

struct Step {
    number: usize,
    /// The heading text after `## Step N: `.
    title: String,
    body: String,
}

/// The `## Step N: <title>` sections of the skill, in document order.
fn steps() -> Vec<Step> {
    let body = skill_body();
    let mut steps: Vec<Step> = Vec::new();
    let mut in_step = false;
    for line in body.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            in_step = false;
            if let Some(rest) = heading.strip_prefix("Step ") {
                let (number, title) = rest
                    .split_once(": ")
                    .unwrap_or_else(|| panic!("step heading without a title: {line}"));
                steps.push(Step {
                    number: number.parse().expect("step number"),
                    title: title.to_string(),
                    body: String::new(),
                });
                in_step = true;
            }
            continue;
        }
        if in_step {
            let step = steps.last_mut().expect("a step is open");
            step.body.push_str(line);
            step.body.push('\n');
        }
    }
    steps
}

/// Every code span of `markdown`: inline spans, and each line of a fenced
/// block without its trailing `# comment`.
fn code_spans(markdown: &str) -> Vec<String> {
    let mut spans = Vec::new();
    let mut in_fence = false;
    for line in markdown.lines() {
        if line.trim_start().starts_with("```") {
            in_fence = !in_fence;
            continue;
        }
        if in_fence {
            let code = line.split("  #").next().unwrap_or("").trim();
            if !code.is_empty() && !code.starts_with('#') {
                spans.push(code.to_string());
            }
            continue;
        }
        let mut rest = line;
        while let Some(start) = rest.find('`') {
            let after = &rest[start + 1..];
            let Some(end) = after.find('`') else { break };
            spans.push(after[..end].to_string());
            rest = &after[end + 1..];
        }
    }
    assert!(!in_fence, "a code fence is left open");
    spans
}

/// `text` without its code spans, for counting sentences.
fn without_code_spans(text: &str) -> String {
    let mut out = String::new();
    let mut in_code = false;
    for c in text.chars() {
        if c == '`' {
            in_code = !in_code;
            out.push('x');
        } else if !in_code {
            out.push(c);
        }
    }
    out
}

fn sentence_count(text: &str) -> usize {
    let plain = without_code_spans(text);
    let chars: Vec<char> = plain.trim().chars().collect();
    chars
        .iter()
        .enumerate()
        .filter(|(i, c)| {
            matches!(c, '.' | '!' | '?') && chars.get(i + 1).is_none_or(|next| next.is_whitespace())
        })
        .count()
}

// --- structure: the seven steps, in order, each saying what happens ---

#[test]
fn the_skill_has_the_seven_steps_in_the_specified_order() {
    let steps = steps();
    let numbers: Vec<usize> = steps.iter().map(|s| s.number).collect();
    assert_eq!(numbers, [1, 2, 3, 4, 5, 6, 7], "steps are numbered 1 to 7");

    for (step, operation) in steps.iter().zip(STEP_OPERATIONS) {
        let expected = if step.number == 1 {
            operation.to_string()
        } else {
            format!("`{operation}`")
        };
        assert_eq!(
            step.title, expected,
            "step {} is headed by its operation",
            step.number
        );
    }

    let init = &steps[1].body;
    assert!(
        init.contains("preset: \"beginner\""),
        "step 2 applies the beginner preset:\n{init}"
    );
}

/// `handoff-mcp setup` writes hooks into the user's own Claude Code settings,
/// which apply to every project. It is the only step that changes something
/// outside the project directory, so the skill has the agent say so and wait
/// before it runs the command. (Found in the real walk: the first version of
/// the skill only said "tell the user", and setup ran unannounced.)
#[test]
fn step_3_asks_before_it_changes_the_users_own_settings() {
    let steps = steps();
    let setup = &steps[2].body;
    let notice = setup.find("hooks").expect("step 3 names the hooks");
    let wait = setup
        .find("Wait for the user to agree")
        .expect("step 3 waits for the user");
    let run = setup
        .find("`handoff-mcp setup -y`")
        .expect("step 3 runs setup");
    assert!(
        notice < wait && wait < run,
        "the notice and the wait come before the command:\n{setup}"
    );
}

#[test]
fn every_step_says_what_happens_in_one_or_two_sentences() {
    for step in steps() {
        let what: Vec<&str> = step
            .body
            .lines()
            .filter_map(|line| line.strip_prefix("**What happens:** "))
            .collect();
        assert_eq!(
            what.len(),
            1,
            "step {} has exactly one `**What happens:**` line",
            step.number
        );
        let sentences = sentence_count(what[0]);
        assert!(
            (1..=2).contains(&sentences),
            "step {}: `What happens` is {sentences} sentences, not 1 or 2: {}",
            step.number,
            what[0]
        );
    }
}

#[test]
fn sentence_counting_ignores_dots_inside_code_spans() {
    assert_eq!(sentence_count("It writes `.handoff/config.toml`."), 1);
    assert_eq!(sentence_count("One. Two!"), 2);
    assert_eq!(sentence_count("No terminator"), 0);
}

// --- AC2: no profile to choose ---

#[test]
fn ac2_the_skill_never_asks_the_reader_to_choose_a_profile() {
    let body = skill_body();
    let lower = body.to_lowercase();
    assert!(
        !lower.contains("profile") && !body.contains("プロファイル"),
        "the skill must not talk about profiles: the preset has decided them"
    );

    let names: BTreeSet<&str> = BUILTIN_PROFILE_NAMES
        .iter()
        .chain(SESSION_LOOP_PROFILES)
        .copied()
        .collect();
    for span in code_spans(&body) {
        let words: Vec<&str> = span
            .split(|c: char| !(c.is_ascii_alphanumeric() || c == '_' || c == '-'))
            .collect();
        for name in &names {
            assert!(
                !words.contains(name),
                "`{span}` names the profile `{name}`: the skill presents no profile"
            );
        }
    }
}

// --- the walk: the steps of the skill against the real binary ---

/// What the walk observed.
struct Walk {
    /// Everything the binary printed or wrote, concatenated: the evidence
    /// that the plain names of the skill exist.
    corpus: String,
    /// `handoff_next_step`'s recommended command before each of steps 2 to 7.
    recommended: Vec<String>,
    /// The status of the task the walk worked on, as finally listed.
    final_status: String,
    /// `handoff_next_step`'s state after the task was finished.
    final_state: String,
}

fn next_step(server: &mut Server, corpus: &mut String) -> Value {
    let step = server.call("handoff_next_step", json!({}));
    corpus.push_str(&step.to_string());
    step
}

fn command_of(step: &Value) -> String {
    step["action"]["command"]
        .as_str()
        .expect("action.command")
        .to_string()
}

/// Runs `command` in the project the way an agent's shell would.
fn run_in_project(sandbox: &Sandbox, command: &str) -> i64 {
    let status = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(&sandbox.project)
        .env("HOME", &sandbox.home)
        .status()
        .expect("run the project command");
    i64::from(status.code().unwrap_or(-1))
}

fn files_below(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let path = entry.path();
        if path.is_dir() {
            files_below(&path, out);
        } else {
            out.push(path);
        }
    }
}

/// Carries out steps 2 to 7 of the skill from an empty directory.
fn walk() -> Walk {
    let sandbox = Sandbox::empty();
    let mut server = sandbox.server();
    let mut corpus = String::new();
    let mut recommended = Vec::new();

    // Step 2: `handoff_init` with the beginner preset.
    recommended.push(command_of(&next_step(&mut server, &mut corpus)));
    corpus.push_str(&server.ok(
        "handoff_init",
        json!({ "project_name": "demo", "preset": "beginner" }),
    ));

    // Step 3: `handoff-mcp setup` (`-y`: an agent cannot answer its prompts).
    recommended.push(command_of(&next_step(&mut server, &mut corpus)));
    let setup = sandbox.cli(&["setup", "-y"]);
    assert_eq!(
        setup.code, 0,
        "setup failed: {}{}",
        setup.stdout, setup.stderr
    );
    corpus.push_str(&setup.stdout);

    // Step 4: `handoff_doctor`. An empty directory is not ready: the skill
    // has the agent create the smallest thing that can be tested, record its
    // test command, fill in CLAUDE.md, run the commands and report them.
    let first = server.call("handoff_doctor", json!({}));
    corpus.push_str(&first.to_string());
    assert_eq!(first["ready"], json!(false), "an empty project: {first}");
    recommended.push(command_of(&next_step(&mut server, &mut corpus)));

    sandbox.write("hello.sh", "#!/bin/sh\necho \"Hello, world!\"\n");
    sandbox.write(
        "tests/run.sh",
        "#!/bin/sh\n[ \"$(sh hello.sh)\" = \"Hello, world!\" ]\n",
    );
    corpus.push_str(&server.ok(
        "handoff_update_config",
        json!({ "updates": { "commands.test": "sh tests/run.sh" } }),
    ));
    let claude_md = sandbox.project.join("CLAUDE.md");
    let sections = std::fs::read_to_string(&claude_md).expect("CLAUDE.md");
    corpus.push_str(&sections);
    std::fs::write(&claude_md, sections.replace("TODO(handoff)", "n/a")).unwrap();

    let asked = server.call("handoff_doctor", json!({}));
    corpus.push_str(&asked.to_string());
    let results: Vec<Value> = asked["commands_to_run"]
        .as_array()
        .expect("commands_to_run")
        .iter()
        .map(|c| {
            let command = c["command"].as_str().expect("command");
            json!({
                "id": c["id"], "command": command,
                "exit_code": run_in_project(&sandbox, command), "duration_ms": 1,
            })
        })
        .collect();
    assert_eq!(results.len(), 1, "the test command is handed out: {asked}");
    let judged = server.call("handoff_doctor", json!({ "results": results }));
    corpus.push_str(&judged.to_string());
    assert_eq!(judged["ready"], json!(true), "{judged}");
    let failed: Vec<&Value> = judged["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|c| c["status"] == json!("fail"))
        .collect();
    assert!(failed.is_empty(), "{failed:?}");

    // Step 5: `/brainstorm-loop` ends in a finalized brainstorm report.
    recommended.push(command_of(&next_step(&mut server, &mut corpus)));
    let started = server.call(
        "handoff_brainstorm_start",
        json!({ "topic": "A greeting script", "initial_context": "Print a greeting" }),
    );
    let finalized = server.call(
        "handoff_brainstorm_finalize",
        json!({
            "brainstorm_id": started["brainstorm_id"],
            "executive_summary": "A script that prints a greeting",
            "scope_in": ["greeting"],
        }),
    );
    assert_eq!(finalized["status"], json!("finalized"), "{finalized}");

    // Step 6: `/requirement-loop` ends in requirements and one task for each.
    recommended.push(command_of(&next_step(&mut server, &mut corpus)));
    server.ok(
        "handoff_doc_save",
        json!({
            "slug": "requirements", "title": "Requirements", "layer": "requirement",
            "body": "# Requirements\n\n### REQ-001 Greeting\n\nThe script prints a greeting.\n\n\
                     **Acceptance criteria:**\n- AC1: `sh hello.sh` prints `Hello, world!`\n",
        }),
    );
    let generated = server.call(
        "handoff_trace_tasks",
        json!({ "mode": "apply", "estimate_hours": 1 }),
    );
    let task_id = generated["created"]
        .as_array()
        .expect("created")
        .iter()
        .find(|t| t["role"] == json!("implements"))
        .unwrap_or_else(|| panic!("no task implements the requirement: {generated}"))["task_id"]
        .as_str()
        .expect("task_id")
        .to_string();

    // Step 7: `/session-loop` takes a ready task to `done`.
    recommended.push(command_of(&next_step(&mut server, &mut corpus)));
    server.ok(
        "handoff_save_context",
        json!({
            "summary": "Session 1: the greeting", "session_status": "active",
            "related_task_ids": [task_id],
        }),
    );
    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": task_id, "status": "in_progress" } }),
    );
    assert_eq!(run_in_project(&sandbox, "sh tests/run.sh"), 0);
    let task = server.call("handoff_get_task", json!({ "task_id": task_id }));
    for index in 0..task["done_criteria"].as_array().map_or(0, Vec::len) {
        server.ok(
            "handoff_check_criterion",
            json!({ "task_id": task_id, "criterion_index": index, "checked": true }),
        );
    }
    corpus.push_str(&server.ok(
        "handoff_update_task",
        json!({ "task": { "id": task_id, "status": "done" } }),
    ));
    server.ok(
        "handoff_save_context",
        json!({ "summary": "Session 1: the greeting is done", "session_status": "closed" }),
    );

    let listed = server.call("handoff_get_task", json!({ "task_id": task_id }));
    corpus.push_str(&listed.to_string());
    let after = next_step(&mut server, &mut corpus);

    // What the binary wrote counts as evidence too.
    let mut written = Vec::new();
    files_below(&sandbox.project, &mut written);
    for path in written {
        corpus.push_str(&path.to_string_lossy());
        corpus.push('\n');
        corpus.push_str(&std::fs::read_to_string(&path).unwrap_or_default());
    }

    Walk {
        corpus,
        recommended,
        final_status: listed["status"].as_str().expect("status").to_string(),
        final_state: after["state"].as_str().expect("state").to_string(),
    }
}

#[test]
fn ac1_from_an_empty_directory_the_steps_of_the_skill_end_in_one_task_done() {
    let walk = walk();

    assert_eq!(walk.final_status, "done");
    assert!(
        ![
            "not_initialized",
            "not_ready",
            "in_progress",
            "review_pending"
        ]
        .contains(&walk.final_state.as_str()),
        "nothing is left half-way: {}",
        walk.final_state
    );

    // Before each of steps 2 to 7 the server recommended the operation the
    // skill names for that step: the tool call, the command line (inside a
    // sentence), or exactly the slash command. Step 4 is the exception that
    // proves the rule: there the server names the first failed check, and
    // the skill tells the reader to do what the diagnosis says.
    let steps = steps();
    assert_eq!(walk.recommended.len(), steps.len() - 1);
    for (step, recommended) in steps.iter().skip(1).zip(&walk.recommended) {
        let operation = step.title.trim_matches('`');
        match step.number {
            2 => assert!(
                recommended.starts_with(&format!("{operation}(")),
                "before step 2 the server recommends `{recommended}`"
            ),
            3 => assert!(
                recommended.contains(&format!("`{operation} ")),
                "before step 3 the server recommends `{recommended}`"
            ),
            4 => {
                assert!(
                    recommended.contains("TODO(handoff)"),
                    "an empty project is first told to fill in CLAUDE.md: {recommended}"
                );
                assert!(
                    step.body.contains("`TODO(handoff)`") && step.body.contains("`next_step`"),
                    "step 4 explains the failed checks of an empty project"
                );
            }
            _ => assert_eq!(
                recommended, operation,
                "before step {} the server recommends another operation",
                step.number
            ),
        }
    }
}

// --- AC3: every name in the skill exists ---

fn json_file(relative: &str) -> Value {
    serde_json::from_str(&read(relative)).unwrap_or_else(|e| panic!("{relative}: {e}"))
}

/// The plugin names of the marketplace, each checked against the manifest of
/// the plugin directory it points to.
fn marketplace_plugins() -> (String, Vec<String>) {
    let marketplace = json_file(".claude-plugin/marketplace.json");
    let name = marketplace["name"].as_str().expect("name").to_string();
    let plugins = marketplace["plugins"]
        .as_array()
        .expect("plugins")
        .iter()
        .map(|plugin| {
            let plugin_name = plugin["name"].as_str().expect("plugin name");
            let source = plugin["source"].as_str().expect("plugin source");
            let manifest = json_file(&format!(
                "{}/.claude-plugin/plugin.json",
                source.trim_start_matches("./")
            ));
            assert_eq!(manifest["name"], json!(plugin_name), "{source}");
            plugin_name.to_string()
        })
        .collect();
    (name, plugins)
}

/// `owner/repo` of the repository in `package.json`.
fn repository_slug() -> String {
    let package = json_file("package.json");
    let url = package["repository"]["url"].as_str().expect("repository");
    url.trim_end_matches(".git")
        .rsplit('/')
        .take(2)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>()
        .join("/")
}

fn cargo_package_name() -> String {
    read("Cargo.toml")
        .lines()
        .find_map(|line| line.strip_prefix("name = "))
        .expect("package name")
        .trim_matches('"')
        .to_string()
}

/// The help text the real binary prints for `args`. It runs in an empty
/// directory with an empty `HOME`, so asking for help can touch nothing.
fn help(args: &[&str]) -> String {
    let nowhere = tempfile::tempdir().expect("tempdir");
    let output = Command::new(env!("CARGO_BIN_EXE_handoff-mcp"))
        .args(args)
        .arg("--help")
        .current_dir(nowhere.path())
        .env("HOME", nowhere.path())
        .env_remove("CLAUDE_PROJECT_DIR")
        .stdin(std::process::Stdio::null())
        .output()
        .expect("run handoff-mcp --help");
    format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

/// The argument names of a call written `tool(name: value, other: value)`:
/// the words before a `:` outside any quote or bracket.
fn argument_names(arguments: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut depth = 0;
    let mut in_string = false;
    let mut word = String::new();
    for c in arguments.chars() {
        match c {
            '"' => in_string = !in_string,
            _ if in_string => {}
            '[' | '{' | '(' => depth += 1,
            ']' | '}' | ')' => depth -= 1,
            ':' if depth == 0 => names.push(std::mem::take(&mut word)),
            c if c.is_ascii_alphanumeric() || c == '_' => word.push(c),
            _ => word.clear(),
        }
    }
    names
}

/// Why `span` does not name something that exists, or `None` when it does.
fn problem_with(span: &str, corpus: &str) -> Option<String> {
    let first_word = span.split_whitespace().next().unwrap_or("");

    // An MCP tool, optionally written as a call.
    if span.starts_with("handoff_") {
        let (name, arguments) = match span.split_once('(') {
            Some((name, rest)) => match rest.strip_suffix(')') {
                Some(arguments) => (name, arguments),
                None => return Some("the call is not closed with `)`".to_string()),
            },
            None => (span, ""),
        };
        let definitions = all_tool_definitions();
        let Some(tool) = definitions.iter().find(|tool| tool.name == name) else {
            return Some(format!("no MCP tool is called `{name}`"));
        };
        return argument_names(arguments).into_iter().find_map(|argument| {
            tool.input_schema["properties"]
                .get(&argument)
                .is_none()
                .then(|| format!("`{name}` has no argument `{argument}`"))
        });
    }

    // A slash command.
    if let Some(command) = first_word.strip_prefix('/') {
        let words: Vec<&str> = span.split_whitespace().collect();
        if CLAUDE_CODE_COMMANDS.contains(&first_word) {
            let (marketplace, plugins) = marketplace_plugins();
            return match words.as_slice() {
                ["/reload-plugins"] => None,
                ["/plugin", "marketplace", "add", source] => (*source != repository_slug())
                    .then(|| format!("the repository is `{}`", repository_slug())),
                ["/plugin", "install" | "enable", target] => {
                    let Some((plugin, from)) = target.split_once('@') else {
                        return Some("write the plugin as `<plugin>@<marketplace>`".to_string());
                    };
                    if from != marketplace {
                        Some(format!("the marketplace is `{marketplace}`"))
                    } else if !plugins.iter().any(|p| p == plugin) {
                        Some(format!("the marketplace has no plugin `{plugin}`"))
                    } else {
                        None
                    }
                }
                _ => Some("not a `/plugin` form this test knows".to_string()),
            };
        }
        // `/command`, or `/plugin:command` with the plugin that ships it.
        let (plugin, command) = match command.split_once(':') {
            Some((plugin, command)) => (Some(plugin), command),
            None => (None, command),
        };
        // The loop commands live in the task-loop plugin, the skills in the
        // base plugin (`plugin/skills/` is generated from `skills/`).
        let shipped_by = [
            (
                "plugin-task-loop",
                format!("plugin-task-loop/commands/{command}.md"),
            ),
            ("plugin", format!("skills/{command}/SKILL.md")),
        ]
        .into_iter()
        .find(|(_, file)| repo().join(file).is_file())
        .map(|(dir, _)| {
            json_file(&format!("{dir}/.claude-plugin/plugin.json"))["name"]
                .as_str()
                .expect("plugin name")
                .to_string()
        });
        return match (shipped_by, plugin) {
            (None, _) => Some(format!("no command file or skill is called `{command}`")),
            (Some(shipped_by), Some(plugin)) if shipped_by != plugin => Some(format!(
                "`{command}` is shipped by the plugin `{shipped_by}`, not `{plugin}`"
            )),
            _ => None,
        };
    }

    // The command line of the binary.
    if first_word == BINARY && span.contains(' ') {
        let words: Vec<&str> = span.split_whitespace().skip(1).collect();
        let subcommand = words.first().filter(|w| !w.starts_with('-')).copied();
        let top = help(&[]);
        if let Some(subcommand) = subcommand {
            let listed = top.lines().any(|line| {
                let line = line.trim_start();
                line.strip_prefix(subcommand)
                    .is_some_and(|rest| rest.starts_with(' '))
                    || line.starts_with(&format!("{BINARY} {subcommand}"))
            });
            if !listed {
                return Some(format!("`{BINARY} --help` lists no `{subcommand}`"));
            }
        }
        let own = subcommand.map_or_else(|| top.clone(), |s| help(&[s]));
        // The flags the help names, as whole words (`[--preset` is `--preset`).
        let known: BTreeSet<&str> = own
            .split_whitespace()
            .map(|word| word.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-')))
            .filter(|word| word.starts_with('-'))
            .collect();
        return words
            .iter()
            .filter(|word| word.starts_with('-'))
            .find(|flag| !known.contains(**flag))
            .map(|flag| format!("the help of `{span}` does not know `{flag}`"));
    }

    // The two ways to install the binary.
    let words: Vec<&str> = span.split_whitespace().collect();
    if let ["npm", "install", "-g", package] = words.as_slice() {
        let name = json_file("package.json")["name"].clone();
        return (name != json!(package)).then(|| format!("the npm package is {name}"));
    }
    if let ["cargo", "install", package] = words.as_slice() {
        return (*package != cargo_package_name())
            .then(|| format!("the crate is `{}`", cargo_package_name()));
    }

    // A plugin, the marketplace, or another skill, by its bare name.
    let (marketplace, plugins) = marketplace_plugins();
    if span == marketplace || plugins.iter().any(|plugin| plugin == span) {
        return None;
    }
    if repo().join(format!("skills/{span}/SKILL.md")).is_file() {
        return None;
    }

    // Anything else is a name the reader will meet on the way: it must occur
    // in what the binary printed or wrote during the walk.
    (!corpus.contains(span))
        .then(|| "the binary neither printed nor wrote this during the walk".to_string())
}

#[test]
fn ac3_every_name_in_the_skill_exists_in_this_commit() {
    let walk = walk();
    let spans: BTreeSet<String> = code_spans(&skill_body()).into_iter().collect();
    assert!(spans.len() >= STEP_OPERATIONS.len(), "{spans:?}");

    let problems: Vec<String> = spans
        .iter()
        .filter_map(|span| problem_with(span, &walk.corpus).map(|why| format!("`{span}`: {why}")))
        .collect();
    assert!(
        problems.is_empty(),
        "the skill names things this commit does not have:\n{}",
        problems.join("\n")
    );

    // The operations of the steps are among the checked names.
    for operation in &STEP_OPERATIONS[1..] {
        assert!(
            spans.iter().any(|span| span.starts_with(operation)),
            "`{operation}` is not named in a code span"
        );
    }
}

/// The checker must reject what does not exist, or it proves nothing.
#[test]
fn the_name_checker_rejects_names_that_do_not_exist() {
    let corpus = "CLAUDE.md `TODO(handoff)`";
    for (span, expected) in [
        ("handoff_no_such_tool", "no MCP tool"),
        ("handoff_init(colour: \"red\")", "no argument `colour`"),
        ("handoff_init(preset: \"beginner\"", "not closed"),
        ("/plugin install handoff-mcp", "<plugin>@<marketplace>"),
        ("handoff-mcp setup -m", "does not know `-m`"),
        ("/no-such-loop", "no command file or skill"),
        (
            "/handoff-task-loop:no-such-loop",
            "no command file or skill",
        ),
        (
            "/handoff-mcp:session-loop",
            "shipped by the plugin `handoff-task-loop`",
        ),
        (
            "/plugin install handoff-mcp@elsewhere",
            "the marketplace is",
        ),
        (
            "/plugin install no-such-plugin@handoff-mcp-marketplace",
            "no plugin",
        ),
        ("/plugin marketplace add someone/else", "the repository is"),
        ("handoff-mcp no-such-command", "lists no `no-such-command`"),
        (
            "handoff-mcp setup --no-such-flag",
            "does not know `--no-such-flag`",
        ),
        ("npm install -g some-other-package", "the npm package is"),
        ("cargo install some-other-crate", "the crate is"),
        ("NO_SUCH_NAME", "neither printed nor wrote"),
    ] {
        let problem =
            problem_with(span, corpus).unwrap_or_else(|| panic!("`{span}` must be rejected"));
        assert!(problem.contains(expected), "`{span}`: {problem}");
    }
    for span in [
        "handoff_doctor",
        "handoff_init(project_name: \"demo\", preset: \"beginner\")",
        "handoff_doctor(results: [{id: \"cmd_test\", exit_code: 0}])",
        "/session-loop",
        "/handoff-task-loop:session-loop",
        "/handoff-mcp:handoff-getting-started",
        "/reload-plugins",
        "/plugin install handoff-task-loop@handoff-mcp-marketplace",
        "handoff-mcp setup -y",
        "handoff-mcp init --project-name demo --preset beginner",
        "handoff-mcp next-step",
        "handoff-task-loop",
        "handoff",
        "CLAUDE.md",
        "TODO(handoff)",
    ] {
        assert_eq!(problem_with(span, corpus), None, "`{span}` exists");
    }
}

#[test]
fn argument_names_are_the_top_level_keys_of_a_call() {
    assert_eq!(
        argument_names("project_name: \"a: b\", preset: \"beginner\""),
        ["project_name", "preset"]
    );
    assert_eq!(
        argument_names("results: [{id: \"cmd_test\", exit_code: 0}]"),
        ["results"]
    );
    assert!(argument_names("").is_empty());
}

// --- packaging: the plugin ships it, the README points to it ---

#[test]
fn the_skill_is_shipped_in_the_plugin_and_listed_in_the_readmes() {
    assert_eq!(
        read(&format!("plugin/skills/{SKILL_NAME}/SKILL.md")),
        skill(),
        "plugin/skills/ is generated from skills/ (scripts/sync-plugin-skills.sh)"
    );

    let readme = read("README.md");
    assert!(
        readme.contains(&format!("| `{SKILL_NAME}` |")),
        "the README skills table lists the skill"
    );
    assert!(
        read("plugin/README.md").contains(SKILL_NAME),
        "the plugin README lists the skill"
    );
}

#[test]
fn the_readme_quick_start_points_to_the_skill_and_names_its_steps() {
    let readme = read("README.md");
    let quick_start = readme
        .split_once("\n## Quick Start\n")
        .expect("the README has a Quick Start")
        .1
        .split("\n## ")
        .next()
        .unwrap();
    // Quick Start opens with the first-project path; the session cycle that
    // follows it is for projects that already exist.
    let first_project = quick_start
        .split("\n### ")
        .nth(1)
        .filter(|section| section.starts_with("First project\n"))
        .expect("Quick Start opens with `### First project`");

    assert!(
        first_project.contains(&format!("`{SKILL_NAME}`"))
            && first_project.contains(&format!("`/{SKILL_NAME}`")),
        "Quick Start points to the skill and says how to start it:\n{first_project}"
    );
    // The same operations, in the same order as the skill.
    let mut from = 0;
    for operation in &STEP_OPERATIONS[1..] {
        let at = first_project[from..]
            .find(&format!("`{operation}"))
            .unwrap_or_else(|| panic!("Quick Start does not name `{operation}` in order"));
        from += at + operation.len();
    }
    // Every name it gives exists, by the same rules as the skill.
    let walk = walk();
    let problems: Vec<String> = code_spans(first_project)
        .into_iter()
        .filter_map(|span| problem_with(&span, &walk.corpus).map(|why| format!("`{span}`: {why}")))
        .collect();
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

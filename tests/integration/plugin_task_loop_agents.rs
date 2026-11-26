//! Guards for the agent definitions shipped in `plugin-task-loop/agents/`.
//!
//! Every agent file must carry well-formed frontmatter (name matching the file
//! stem, a description, a known model, an effort, a tools list), and the
//! brainstorm agents must exist with the model/tool contract the brainstorm
//! workflow relies on. The elite agents are the only ones allowed on `fable`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

fn agents_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop/agents")
}

/// Parse the leading `---` frontmatter block into key/value pairs.
fn frontmatter(text: &str) -> Option<BTreeMap<String, String>> {
    let rest = text.strip_prefix("---\n")?;
    let end = rest.find("\n---\n")?;
    let mut map = BTreeMap::new();
    for line in rest[..end].lines() {
        let (k, v) = line.split_once(':')?;
        map.insert(k.trim().to_string(), v.trim().to_string());
    }
    Some(map)
}

/// All agent definitions (underscore-prefixed files are shared protocol
/// fragments, not agents).
fn agent_files() -> Vec<(String, String)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(agents_dir()).expect("agents dir") {
        let path = entry.unwrap().path();
        let stem = path.file_stem().unwrap().to_string_lossy().to_string();
        if path.extension().is_some_and(|e| e == "md") && !stem.starts_with('_') {
            out.push((stem, fs::read_to_string(&path).unwrap()));
        }
    }
    out.sort();
    out
}

/// Name prefix of the single-agent definitions the `elite` profile launches.
const ELITE_AGENT_PREFIX: &str = "elite-";

/// May the agent in `<stem>.md` declare this model?
///
/// `opus` and `sonnet` are open to every agent. `fable` is the elite tier's
/// model and is accepted only on an `elite-` agent: it costs more per turn, so
/// moving one of the multi-agent pipeline's agents onto it must be a decision
/// that changes this rule, not a one-word frontmatter edit that passes unnoticed.
fn model_is_allowed(stem: &str, model: Option<&str>) -> bool {
    match model {
        Some("opus" | "sonnet") => true,
        Some("fable") => stem.starts_with(ELITE_AGENT_PREFIX),
        _ => false,
    }
}

#[test]
fn every_agent_has_well_formed_frontmatter() {
    let mut problems = Vec::new();
    for (stem, text) in agent_files() {
        let Some(fm) = frontmatter(&text) else {
            problems.push(format!("{stem}: missing or malformed frontmatter"));
            continue;
        };
        if fm.get("name") != Some(&stem) {
            problems.push(format!("{stem}: name {:?} != file stem", fm.get("name")));
        }
        if fm.get("description").is_none_or(|d| d.is_empty()) {
            problems.push(format!("{stem}: empty description"));
        }
        if !model_is_allowed(&stem, fm.get("model").map(String::as_str)) {
            problems.push(format!(
                "{stem}: model {:?} is unknown or not allowed for this agent",
                fm.get("model")
            ));
        }
        if fm.get("tools").is_none_or(|v| v.is_empty()) {
            problems.push(format!("{stem}: missing tools"));
        }
    }
    assert!(
        problems.is_empty(),
        "agent frontmatter problems:\n{}",
        problems.join("\n")
    );
}

#[test]
fn fable_is_a_known_model_only_for_elite_agents() {
    // The elite agents run on Fable.
    assert!(model_is_allowed("elite-developer", Some("fable")));
    // No other agent may be moved to it by an edit to its frontmatter alone.
    for stem in ["session-developer", "session-reviewer", "developer-elite"] {
        assert!(
            !model_is_allowed(stem, Some("fable")),
            "{stem} must not be allowed to declare fable"
        );
    }
    // The existing models stay valid for every agent, elite or not.
    for stem in ["session-developer", "elite-developer"] {
        for model in ["opus", "sonnet"] {
            assert!(model_is_allowed(stem, Some(model)), "{stem}: {model}");
        }
    }
    // A missing, empty, misspelled or differently-cased model is never allowed.
    for stem in ["session-developer", "elite-developer"] {
        for model in [
            None,
            Some(""),
            Some("Fable"),
            Some("fable-5"),
            Some("haiku"),
        ] {
            assert!(!model_is_allowed(stem, model), "{stem}: {model:?}");
        }
    }
}

#[test]
fn elite_developer_is_a_fable_agent_with_the_required_tools_listed_in_readme() {
    let files: BTreeMap<_, _> = agent_files().into_iter().collect();
    let text = files
        .get("elite-developer")
        .expect("elite-developer agent file missing");
    let fm = frontmatter(text).expect("elite-developer: bad frontmatter");
    assert_eq!(fm["model"], "fable", "elite-developer: model");
    let have: Vec<&str> = fm["tools"].split(',').map(str::trim).collect();
    let required = [
        "Read",
        "Edit",
        "Write",
        "Bash",
        "Grep",
        "Glob",
        "TodoWrite",
        "ToolSearch",
        "WebSearch",
        "WebFetch",
    ];
    for t in required.iter().chain(MCP_TOOL_PATTERNS.iter()) {
        assert!(
            have.contains(t),
            "elite-developer: tool {t} missing from {have:?}"
        );
    }
    assert_eq!(
        have.len(),
        required.len() + MCP_TOOL_PATTERNS.len(),
        "elite-developer: tools beyond the required set, or one listed twice: {have:?}"
    );
    // The workflow passes effort per profile; a pinned effort would override it.
    assert!(
        !fm.contains_key("effort"),
        "elite-developer: effort must not be pinned in the frontmatter"
    );
    let readme = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop/README.md"),
    )
    .unwrap();
    assert!(
        readme.contains("elite-developer (Fable"),
        "README.md does not list elite-developer with its model"
    );
}

#[test]
fn brainstorm_agents_are_registered_with_expected_contract() {
    // (name, model, tools that must be present)
    let expected: [(&str, &str, &[&str]); 3] = [
        (
            "brainstorm-regulatory-director",
            "opus",
            &["Read", "Edit", "Write", "Bash", "Grep", "Glob", "TodoWrite"],
        ),
        (
            "brainstorm-regulatory-investigator",
            "sonnet",
            &[
                "Read",
                "Bash",
                "Grep",
                "Glob",
                "TodoWrite",
                "WebSearch",
                "WebFetch",
            ],
        ),
        (
            "brainstorm-adversarial-reviewer",
            "opus",
            &["Read", "Bash", "Grep", "Glob", "TodoWrite"],
        ),
    ];
    let files: BTreeMap<_, _> = agent_files().into_iter().collect();
    for (name, model, tools) in expected {
        let text = files
            .get(name)
            .unwrap_or_else(|| panic!("{name}: agent file missing"));
        let fm = frontmatter(text).unwrap_or_else(|| panic!("{name}: bad frontmatter"));
        assert_eq!(fm["model"], model, "{name}: model");
        assert_eq!(fm["effort"], "high", "{name}: effort");
        let have: Vec<&str> = fm["tools"].split(',').map(str::trim).collect();
        for t in tools {
            assert!(have.contains(t), "{name}: tool {t} missing from {have:?}");
        }
    }
}

#[test]
fn readme_lists_brainstorm_agents() {
    let readme = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop/README.md"),
    )
    .unwrap();
    for name in [
        "brainstorm-regulatory-director",
        "brainstorm-regulatory-investigator",
        "brainstorm-adversarial-reviewer",
    ] {
        assert!(readme.contains(name), "README.md does not mention {name}");
    }
}

#[test]
fn test_spec_worker_is_registered_and_listed_in_readme() {
    let files: BTreeMap<_, _> = agent_files().into_iter().collect();
    let text = files
        .get("test-spec-worker")
        .expect("test-spec-worker agent file missing");
    let fm = frontmatter(text).expect("test-spec-worker: bad frontmatter");
    assert_eq!(fm["model"], "sonnet", "test-spec-worker: model");
    let have: Vec<&str> = fm["tools"].split(',').map(str::trim).collect();
    for t in ["Read", "Edit", "Write", "Grep", "Glob", "ToolSearch"] {
        assert!(have.contains(&t), "test-spec-worker: tool {t} missing");
    }
    let readme = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop/README.md"),
    )
    .unwrap();
    assert!(
        readme.contains("test-spec-worker"),
        "README.md does not mention test-spec-worker"
    );
}

// ---------------------------------------------------------------------------
// WS1 / t430: sub-agents (Agent tool and Workflow `agentType`) must be able to
// reach the handoff tools. An agent definition with a `tools:` allow-list
// receives neither ToolSearch nor MCP tools unless they are named in it, so
// the definitions list the MCP tools explicitly, carry a CLI fallback table
// and a "report failure, never skip silently" section.
// ---------------------------------------------------------------------------

/// MCP tool name forms a `tools:` allow-list needs: the standalone server
/// (`.mcp.json`) and the one bundled with the `handoff-mcp` plugin.
const MCP_TOOL_PATTERNS: [&str; 2] = ["mcp__handoff__*", "mcp__plugin_handoff-mcp_handoff__*"];

/// Agents that write handoff state, and the handoff tools each one calls
/// (every one must have a CLI equivalent row).
const HANDOFF_WRITER_AGENTS: [(&str, &[&str]); 3] = [
    (
        "session-closer",
        &[
            "handoff_get_task",
            "handoff_check_criterion",
            "handoff_update_task",
            "handoff_trace_update",
            "handoff_doc_verify",
            "handoff_log_time",
        ],
    ),
    (
        "session-spec-writer",
        &[
            "handoff_get_task",
            "handoff_get_config",
            "handoff_doc_query",
            "handoff_doc_list",
            "handoff_doc_save",
            "handoff_trace_scaffold",
            "handoff_trace_lint",
        ],
    ),
    (
        "session-spec-reconciler",
        &[
            "handoff_get_task",
            "handoff_doc_query",
            "handoff_doc_update_section",
            "handoff_trace_lint",
            "handoff_trace_suspect",
            "handoff_trace_update",
        ],
    ),
];

fn agent_text(name: &str) -> String {
    fs::read_to_string(agents_dir().join(format!("{name}.md")))
        .unwrap_or_else(|e| panic!("{name}: cannot read agent file: {e}"))
}

/// `(mcp_tool, group, action)` for every CLI-equivalent table row
/// (`| \`handoff_x\` | \`handoff-mcp <group> <action> ...\` | ... |`).
fn cli_table_rows(text: &str) -> Vec<(String, String, String)> {
    let mut rows = Vec::new();
    for line in text.lines().filter(|l| l.trim_start().starts_with('|')) {
        let cells: Vec<&str> = line.split('|').map(str::trim).collect();
        // cells[0] is the empty text before the leading pipe.
        let (Some(tool), Some(cli)) = (cells.get(1), cells.get(2)) else {
            continue;
        };
        let (Some(tool), Some(cli)) = (
            tool.strip_prefix('`').and_then(|t| t.strip_suffix('`')),
            cli.strip_prefix('`').and_then(|c| c.strip_suffix('`')),
        ) else {
            continue;
        };
        let Some(rest) = cli.strip_prefix("handoff-mcp ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        if let (Some(group), Some(action)) = (words.next(), words.next()) {
            rows.push((tool.to_string(), group.to_string(), action.to_string()));
        }
    }
    rows
}

#[test]
fn handoff_writer_agents_list_mcp_tools_in_both_name_forms() {
    for (name, _) in HANDOFF_WRITER_AGENTS {
        let text = agent_text(name);
        let fm = frontmatter(&text).unwrap_or_else(|| panic!("{name}: bad frontmatter"));
        let have: Vec<&str> = fm["tools"].split(',').map(str::trim).collect();
        assert!(have.contains(&"ToolSearch"), "{name}: ToolSearch dropped");
        assert!(have.contains(&"Bash"), "{name}: Bash is the CLI fallback");
        for pattern in MCP_TOOL_PATTERNS {
            assert!(
                have.contains(&pattern),
                "{name}: tools: must list {pattern} explicitly (an allow-list agent gets no MCP tools otherwise), got {have:?}"
            );
        }
    }
}

#[test]
fn handoff_writer_agents_document_cli_fallback_and_failure_reporting() {
    for (name, tools) in HANDOFF_WRITER_AGENTS {
        let text = agent_text(name);
        assert!(
            text.contains("CLI equivalents"),
            "{name}: missing the CLI equivalents section"
        );
        assert!(
            text.contains("Report failures, never skip silently"),
            "{name}: missing the failure-reporting section"
        );
        // JSON argument form: flat flags for nested fields are silently
        // dropped by some actions, so a table that writes nested arguments
        // (`task`, `ops`) must show `--task '{...}'` / `--ops '[...]'`.
        if tools.contains(&"handoff_update_task") {
            assert!(
                text.contains("--task '{"),
                "{name}: update_task row must use --task '{{...}}'"
            );
        }
        if tools.contains(&"handoff_trace_update") {
            assert!(
                text.contains("--ops '["),
                "{name}: trace_update row must use --ops '[...]'"
            );
            // trace update writes in a fixed category order, not input order,
            // and `failed.op_index` is the first op of the failing category:
            // "resend the failed op and the ops after it" silently drops
            // unwritten ops with a lower index. The retry set is exactly the
            // ops missing from `applied`.
            assert!(
                text.contains("not in `applied[].op_index`"),
                "{name}: must tell the agent to resend exactly the ops missing from `applied`"
            );
            assert!(
                !text.contains("ops after it"),
                "{name}: the 'failed op and the ops after it' retry rule drops unwritten ops"
            );
        }
        // A CLI call has no agent identity: session commands need an explicit id.
        assert!(
            text.contains("--session-id"),
            "{name}: must mention --session-id for session commands over the CLI"
        );
        // `trace update` exits 1 when an op failed: resend only the failed ops.
        assert!(
            text.contains("exit code 1") && text.contains("failed"),
            "{name}: must document the trace update exit code / failed ops"
        );
        let rows = cli_table_rows(&text);
        for tool in tools {
            assert!(
                rows.iter().any(|(t, _, _)| t == tool),
                "{name}: no CLI equivalent row for {tool}"
            );
        }
    }
}

#[test]
fn every_command_in_the_cli_tables_exists_in_the_binary() {
    // Probe the real binary in an empty directory: a command it does not know
    // fails with "Unknown command"; a known one fails later (no `.handoff/`).
    let empty = tempfile::TempDir::new().unwrap();
    let mut seen = 0;
    for (name, _) in HANDOFF_WRITER_AGENTS {
        for (tool, group, action) in cli_table_rows(&agent_text(name)) {
            let output = std::process::Command::new(env!("CARGO_BIN_EXE_handoff-mcp"))
                .args([&group, &action, "--project-dir"])
                .arg(empty.path())
                .output()
                .unwrap();
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            assert!(
                !stdout.contains("Unknown command") && !stderr.contains("Unknown command"),
                "{name}: `handoff-mcp {group} {action}` (for {tool}) is not a CLI command:\n{stdout}{stderr}"
            );
            seen += 1;
        }
    }
    assert!(
        seen >= 15,
        "expected the CLI tables to be parsed, saw {seen} rows"
    );
}

fn project_file(rel: &str) -> String {
    fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join(rel))
        .unwrap_or_else(|e| panic!("{rel}: {e}"))
}

#[test]
fn session_closer_has_a_log_time_step_with_the_agreed_rules() {
    let text = agent_text("session-closer");
    assert!(
        text.contains("### 3. Log actual time") || text.contains("Log actual time"),
        "session-closer: missing the 'Log actual time' procedure step"
    );
    for needle in [
        "handoff_log_time",
        "### Actual time",
        "hours_logged",
        "log_time",
        "never inflate",
    ] {
        assert!(
            text.to_lowercase().contains(&needle.to_lowercase()),
            "session-closer: log_time step must mention {needle}"
        );
    }
    // Missing time is not a failure, and bundled tasks have a documented rule.
    assert!(
        text.contains("not a failure"),
        "session-closer: a report without time must be documented as not a failure"
    );
    assert!(
        text.contains("first task"),
        "session-closer: bundled-task hours rule must be documented"
    );
}

#[test]
fn session_closer_log_time_cli_row_uses_task_log_time() {
    let rows = cli_table_rows(&agent_text("session-closer"));
    assert!(
        rows.iter()
            .any(|(t, g, a)| t == "handoff_log_time" && g == "task" && a == "log-time"),
        "session-closer: CLI table needs a `task log-time` row for handoff_log_time"
    );
}

#[test]
fn closer_schema_reports_hours_logged_per_task() {
    let js = project_file("plugin-task-loop/workflows/session-execute.js");
    let start = js.find("const CLOSER_SCHEMA").expect("CLOSER_SCHEMA");
    let end = js[start..]
        .find("const SPEC_WRITER_SCHEMA")
        .expect("next schema")
        + start;
    let schema = &js[start..end];
    assert!(
        schema.contains("hours_logged: { type: 'number' }"),
        "CLOSER_SCHEMA: hours_logged must be an optional number per task"
    );
    let required = schema.find("required: ['task_id', 'status_set']").is_some();
    assert!(required, "CLOSER_SCHEMA: hours_logged must stay optional");
    assert!(
        schema.contains("log_time"),
        "CLOSER_SCHEMA: ops_failed comment must name the log_time op"
    );
    // The closer prompt tells the closer to log the developer's reported time.
    let prompt = js
        .find("function buildCloserPrompt")
        .expect("closer prompt");
    let prompt_end = js[prompt..].find("function specScopeIsFull").unwrap() + prompt;
    assert!(
        js[prompt..prompt_end].contains("handoff_log_time"),
        "closer prompt must instruct the log_time step"
    );
}

#[test]
fn session_closer_documents_status_set_unchanged() {
    let text = agent_text("session-closer");
    assert!(
        text.contains("`unchanged`"),
        "session-closer: must document the `unchanged` status_set value"
    );
    assert!(
        !text.contains("status_set: \"\"") && !text.contains("empty `status_set`"),
        "session-closer: must not suggest an empty status_set"
    );
    let js = project_file("plugin-task-loop/workflows/session-execute.js");
    let start = js.find("const CLOSER_SCHEMA").expect("CLOSER_SCHEMA");
    let end = js[start..]
        .find("const SPEC_WRITER_SCHEMA")
        .expect("next schema")
        + start;
    assert!(
        js[start..end].contains("enum: ['done', 'review', 'in_progress', 'unchanged']"),
        "CLOSER_SCHEMA: status_set enum must include 'unchanged'"
    );
    let prompt = js
        .find("function buildCloserPrompt")
        .expect("closer prompt");
    let prompt_end = js[prompt..].find("function specScopeIsFull").unwrap() + prompt;
    assert!(
        js[prompt..prompt_end].contains("unchanged"),
        "closer prompt must tell the closer about status_set 'unchanged'"
    );
}

#[test]
fn developer_report_format_asks_for_actual_time() {
    let text = agent_text("session-developer");
    assert!(
        text.contains("### Actual time"),
        "session-developer: Return format needs an `### Actual time` section"
    );
}

/// Real binary: the CLI route the closer documents writes `time_log.jsonl`.
#[test]
fn cli_task_log_time_writes_the_time_log_on_disk() {
    let dir = tempfile::TempDir::new().unwrap();
    let run = |args: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_handoff-mcp"))
            .args(args)
            .arg("--project-dir")
            .arg(dir.path())
            .output()
            .unwrap()
    };
    assert!(run(&["init", "--project-name", "p"]).status.success());
    assert!(
        run(&["task", "update", "--task", r#"{"id":"t1","title":"x"}"#])
            .status
            .success()
    );
    let out = run(&["task", "log-time", "--task-id", "t1", "--hours", "0.2"]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let log = fs::read_to_string(dir.path().join(".handoff/time_log.jsonl")).unwrap();
    let entry: serde_json::Value = serde_json::from_str(log.lines().next().unwrap()).unwrap();
    assert_eq!(entry["task_id"], "t1");
    assert_eq!(entry["hours"], 0.2);
}

// ---------------------------------------------------------------------------
// t448 / D2: the Close stage reads the closer's claims back from disk with a
// read-only verifier agent; the comparison itself is deterministic JS.
// ---------------------------------------------------------------------------

#[test]
fn session_closer_verifier_is_a_read_only_agent_the_workflow_launches() {
    let text = agent_text("session-closer-verifier");
    let fm = frontmatter(&text).expect("session-closer-verifier: bad frontmatter");
    assert_eq!(fm["name"], "session-closer-verifier");
    assert_eq!(fm["model"], "sonnet");
    let have: Vec<&str> = fm["tools"].split(',').map(str::trim).collect();
    for t in ["Read", "Bash", "ToolSearch"] {
        assert!(have.contains(&t), "tools must include {t}: {have:?}");
    }
    // Read-only: no file writers and no blanket handoff MCP grant (which would
    // include the write tools); only `handoff_get_task`, in both name forms.
    for forbidden in [
        "Edit",
        "Write",
        "mcp__handoff__*",
        "mcp__plugin_handoff-mcp_handoff__*",
    ] {
        assert!(
            !have.contains(&forbidden),
            "{forbidden} must not be granted: {have:?}"
        );
    }
    for t in [
        "mcp__handoff__handoff_get_task",
        "mcp__plugin_handoff-mcp_handoff__handoff_get_task",
    ] {
        assert!(have.contains(&t), "tools must list {t}: {have:?}");
    }
    assert!(
        text.contains("handoff-mcp task get --task-id"),
        "must document the CLI fallback for handoff_get_task"
    );
    assert!(
        text.contains("checked_indices") && text.contains("never guess"),
        "must define the output and the never-guess rule"
    );
    let js = project_file("plugin-task-loop/workflows/session-execute.js");
    assert!(
        js.contains("agentType: 'handoff-task-loop:session-closer-verifier'"),
        "workflow must launch the verifier agent type"
    );
    assert!(
        js.contains("compareCloserClaimsWithDisk(claims, observed)"),
        "the comparison must run in the workflow via the pure function"
    );
    assert!(
        project_file("plugin-task-loop/README.md").contains("session-closer-verifier"),
        "README.md does not mention session-closer-verifier"
    );
}

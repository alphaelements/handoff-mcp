//! CLI subcommand routing layer.
//!
//! Translates `handoff-mcp <group> <action> [--key value ...]` into a
//! `serde_json::Value` and delegates to the existing MCP handler. All output
//! goes to stdout as JSON for programmatic consumption.

use serde_json::{json, Value};

use crate::mcp::handlers;

/// Entry point called from `main()` when `args[1]` matches a known CLI group.
/// Returns the exit code (0 = success, 1 = error).
pub fn run(args: &[String]) -> i32 {
    let result = dispatch(args);
    match result {
        Ok(output) => {
            println!("{output}");
            0
        }
        Err(e) => {
            let err = json!({ "error": format!("{e:#}") });
            println!(
                "{}",
                serde_json::to_string_pretty(&err).unwrap_or_else(|_| err.to_string())
            );
            1
        }
    }
}

fn dispatch(args: &[String]) -> anyhow::Result<String> {
    let group = args.first().map(String::as_str).unwrap_or("");
    let second = args.get(1).map(String::as_str).unwrap_or("");

    // If the second arg is a flag (starts with --), treat it as no action.
    let (action, flag_args) = if second.starts_with("--") || second.is_empty() {
        ("", if args.len() > 1 { &args[1..] } else { &[][..] })
    } else {
        (second, if args.len() > 2 { &args[2..] } else { &[][..] })
    };

    // --help anywhere in the flags → show group help.
    if flag_args.iter().any(|a| a == "--help" || a == "-h") {
        print_group_help(group);
        std::process::exit(0);
    }

    let tool_name = resolve_tool_name(group, action)?;
    let arguments = parse_flags(flag_args, &tool_name)?;

    // Delegate to the single dispatch table in handlers::handle_tool_call.
    // It returns a JsonRpcResponse wrapping the result; we extract the text.
    let response = handlers::handle_tool_call(&tool_name, &arguments);
    extract_tool_result(response)
}

/// Extract the content text from a JsonRpcResponse returned by the MCP handler
/// dispatch. Surfaces handler-level errors as `Err` so the CLI prints them with
/// exit code 1.
fn extract_tool_result(response: crate::mcp::types::JsonRpcResponse) -> anyhow::Result<String> {
    let result = response.result.ok_or_else(|| {
        let msg = response
            .error
            .map(|e| e.message)
            .unwrap_or_else(|| "Unknown error".to_string());
        anyhow::anyhow!("{msg}")
    })?;

    if result
        .get("isError")
        .and_then(|v| v.as_bool())
        .unwrap_or(false)
    {
        let text = result
            .get("content")
            .and_then(|c| c.as_array())
            .and_then(|arr| arr.first())
            .and_then(|item| item.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("Unknown error");
        anyhow::bail!("{text}");
    }

    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .and_then(|arr| arr.first())
        .and_then(|item| item.get("text"))
        .and_then(|t| t.as_str())
        .unwrap_or("")
        .to_string();

    Ok(text)
}

/// Map `(group, action)` to the internal MCP tool name.
fn resolve_tool_name(group: &str, action: &str) -> anyhow::Result<String> {
    let name = match (group, action) {
        // init (no action needed)
        ("init", "") => "handoff_init",

        // task
        ("task", "list") => "handoff_list_tasks",
        ("task", "get") => "handoff_get_task",
        ("task", "update" | "create") => "handoff_update_task",
        ("task", "check") => "handoff_check_criterion",
        ("task", "bulk-update") => "handoff_bulk_update_tasks",
        ("task", "log-time") => "handoff_log_time",
        ("task", "import") => "handoff_import_context",

        // session
        ("session", "load") => "handoff_load_context",
        ("session", "save") => "handoff_save_context",
        ("session", "list") => "handoff_list_sessions",
        ("session", "get") => "handoff_get_session",
        ("session", "update") => "handoff_update_session",

        // config
        ("config", "get") => "handoff_get_config",
        ("config", "update") => "handoff_update_config",

        // memory
        ("memory", "save") => "handoff_memory_save",
        ("memory", "query") => "handoff_memory_query",
        ("memory", "delete") => "handoff_memory_delete",
        ("memory", "cleanup") => "handoff_memory_cleanup",

        // referral
        ("referral", "send" | "refer") => "handoff_refer",
        ("referral", "list") => "handoff_list_referrals",
        ("referral", "get") => "handoff_get_referral",
        ("referral", "update") => "handoff_update_referral",

        // assignee
        ("assignee", "list") => "handoff_list_assignees",
        ("assignee", "add") => "handoff_add_assignee",
        ("assignee", "update") => "handoff_update_assignee",
        ("assignee", "remove") => "handoff_remove_assignee",

        // milestone
        ("milestone", "list") => "handoff_list_milestones",
        ("milestone", "add") => "handoff_add_milestone",
        ("milestone", "update") => "handoff_update_milestone",
        ("milestone", "remove") => "handoff_remove_milestone",

        // calendar / labels / project
        ("calendar", "update") => "handoff_update_calendar",
        ("labels", "update") => "handoff_update_labels",
        ("project", "start") => "handoff_start_project",

        // metrics / capacity / schedule
        ("metrics", "" | "get") => "handoff_get_metrics",
        ("capacity", "" | "get") => "handoff_get_capacity",
        ("schedule", "" | "auto") => "handoff_auto_schedule",

        // dashboard
        ("dashboard", "") => "handoff_dashboard",

        // timer
        ("timer", "start") => "handoff_timer_start",
        ("timer", "stop") => "handoff_timer_stop",
        ("timer", "get") => "handoff_timer_get_time",

        _ => {
            if action.is_empty() {
                anyhow::bail!(
                    "Unknown command: {group}\n\nRun `handoff-mcp --help` for available commands."
                );
            } else {
                anyhow::bail!(
                    "Unknown command: {group} {action}\n\nRun `handoff-mcp {group} --help` for available actions."
                );
            }
        }
    };
    Ok(name.to_string())
}

/// Parse `--key value` flags into a JSON object.
///
/// Conventions:
/// - `--key value` → `{"key": "value"}` (string)
/// - `--key 42` → `{"key": 42}` (number if parseable)
/// - `--key true/false` → `{"key": true}` (bool)
/// - `--key a,b,c` → `{"key": ["a","b","c"]}` (comma-separated → array)
/// - `--json-key '{"a":1}'` → parsed as JSON value (for nested objects)
/// - `--flag` (no value or next arg starts with `--`) → `{"flag": true}`
///
/// Dashes in key names are converted to underscores (e.g. `--project-dir` →
/// `project_dir`) to match the MCP parameter naming.
fn parse_flags(args: &[String], tool_name: &str) -> anyhow::Result<Value> {
    let mut map = serde_json::Map::new();
    let mut i = 0;

    while i < args.len() {
        let arg = &args[i];
        if !arg.starts_with("--") {
            anyhow::bail!("Unexpected positional argument: {arg}");
        }
        let key = arg[2..].replace('-', "_");

        let value = if i + 1 < args.len() && !args[i + 1].starts_with("--") {
            i += 1;
            parse_value(&args[i], &key)
        } else {
            Value::Bool(true)
        };

        // Nest into a `task` or `updates` object for tools that expect it.
        insert_value(&mut map, &key, value, tool_name);
        i += 1;
    }

    Ok(Value::Object(map))
}

/// Insert a value into the correct position in the JSON object.
///
/// Some MCP tools expect nested objects (e.g. `handoff_update_task` wants
/// `{"task": {"id": ..., "title": ...}}`). This function routes known fields
/// into the right nesting level so the CLI user writes flat flags:
/// `--id t1 --title "foo"` instead of `--task '{"id":"t1","title":"foo"}'`.
fn insert_value(
    map: &mut serde_json::Map<String, Value>,
    key: &str,
    value: Value,
    tool_name: &str,
) {
    match tool_name {
        "handoff_update_task" => {
            // Fields that go into the top-level `task` object.
            let task_fields = [
                "id",
                "title",
                "status",
                "notes",
                "priority",
                "labels",
                "links",
                "done_criteria",
                "assignee",
                "dependencies",
                "order",
            ];
            // Fields that go into `task.schedule`.
            let schedule_fields = [
                "start_date",
                "due_date",
                "estimate_hours",
                "actual_hours",
                "remaining_hours",
                "milestone",
                "pinned",
            ];

            if task_fields.contains(&key) {
                let task = map
                    .entry("task")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .expect("task must be object");
                task.insert(key.to_string(), value);
            } else if schedule_fields.contains(&key) {
                let task = map
                    .entry("task")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .expect("task must be object");
                let schedule = task
                    .entry("schedule")
                    .or_insert_with(|| json!({}))
                    .as_object_mut()
                    .expect("schedule must be object");
                schedule.insert(key.to_string(), value);
            } else {
                map.insert(key.to_string(), value);
            }
        }
        _ => {
            map.insert(key.to_string(), value);
        }
    }
}

/// Fields that are always strings even when they look like numbers. Handlers
/// call `.as_str()` on these, so coercing "42" to `Number(42)` would silently
/// break them.
const STRING_FIELDS: &[&str] = &[
    "task_id",
    "id",
    "session_id",
    "referral_id",
    "key",
    "name",
    "text",
    "kind",
    "summary",
    "status",
    "status_filter",
    "assignee_filter",
    "milestone_filter",
    "priority_filter",
    "label_filter",
    "assignee",
    "priority",
    "project_dir",
    "project_name",
    "description",
    "notes",
    "title",
    "display_name",
    "color",
    "target_project",
    "target_project_dir",
    "referral_type",
    "details",
    "merge_into",
    "tool_name",
    "start_date",
    "due_date",
    "date",
    "end_date",
    "schedule_mode",
    "session_status",
    "close_session_id",
    "pause_session_id",
    "move_to",
    "parent_id",
    "milestone",
];

/// Fields that are always numeric. Only these are coerced from string to number.
const NUMERIC_FIELDS: &[&str] = &[
    "hours",
    "estimate_hours",
    "actual_hours",
    "remaining_hours",
    "limit",
    "criterion_index",
    "order",
    "work_hours_per_day",
    "overwork_limit_percent",
    "max_utilization",
    "stale_days",
    "checklist_index",
];

/// Parse a CLI flag value into a JSON type, using the field name to decide
/// whether numeric coercion is appropriate.
fn parse_value(s: &str, key: &str) -> Value {
    // Try JSON parse first (handles objects, arrays, quoted strings).
    if let Ok(v) = serde_json::from_str::<Value>(s) {
        if v.is_object() || v.is_array() {
            return v;
        }
    }

    // Known string fields — never coerce to number/bool.
    if STRING_FIELDS.contains(&key) {
        return Value::String(s.to_string());
    }

    // Boolean.
    if s == "true" {
        return Value::Bool(true);
    }
    if s == "false" {
        return Value::Bool(false);
    }

    // Numeric coercion — only for known numeric fields.
    if NUMERIC_FIELDS.contains(&key) {
        if let Ok(n) = s.parse::<i64>() {
            return Value::Number(n.into());
        }
        if let Ok(f) = s.parse::<f64>() {
            if let Some(n) = serde_json::Number::from_f64(f) {
                return Value::Number(n);
            }
        }
    }

    // Comma-separated array (only if no spaces and contains comma).
    if s.contains(',') && !s.contains(' ') {
        let arr: Vec<Value> = s.split(',').map(|p| Value::String(p.to_string())).collect();
        return Value::Array(arr);
    }

    Value::String(s.to_string())
}

/// All known CLI groups (for help display).
pub const GROUPS: &[(&str, &str)] = &[
    ("init", "Initialize handoff tracking for a project"),
    (
        "task",
        "Task management (list, get, update, check, bulk-update, log-time, import)",
    ),
    (
        "session",
        "Session management (load, save, list, get, update)",
    ),
    ("config", "Configuration (get, update)"),
    ("memory", "Project memory (save, query, delete, cleanup)"),
    (
        "referral",
        "Cross-project referrals (send, list, get, update)",
    ),
    ("assignee", "Team members (list, add, update, remove)"),
    ("milestone", "Milestones (list, add, update, remove)"),
    ("calendar", "Calendar settings (update)"),
    ("labels", "Project labels (update)"),
    ("project", "Project lifecycle (start)"),
    ("metrics", "Project metrics"),
    ("capacity", "Work capacity"),
    ("schedule", "Auto-scheduler"),
    ("dashboard", "Cross-project dashboard"),
    ("timer", "Timer coordination (start, stop, get)"),
];

pub fn print_cli_help() {
    println!(
        "handoff-mcp v{version} — CLI API

USAGE:
    handoff-mcp <command> <action> [--key value ...]

COMMANDS:",
        version = env!("CARGO_PKG_VERSION")
    );
    for (name, desc) in GROUPS {
        println!("    {name:<16}{desc}");
    }
    println!(
        "
GLOBAL OPTIONS:
    --project-dir <path>    Project directory (default: current directory)
    --help                  Show help for a command

EXAMPLES:
    handoff-mcp memory save --text \"Always use atomic_write\" --kind lesson
    handoff-mcp memory query --text \"atomic\" --limit 5
    handoff-mcp task list --status-filter todo
    handoff-mcp session load
    handoff-mcp metrics"
    );
}

pub fn print_group_help(group: &str) {
    let actions: &[(&str, &str)] = match group {
        "init" => &[("", "Initialize handoff tracking (--project-name <name>)")],
        "task" => &[
            ("list", "List tasks (--status-filter, --assignee-filter, --milestone-filter, --priority-filter, --label-filter)"),
            ("get", "Get task detail (--task-id <id>)"),
            ("update", "Create or update a task (--id, --title, --status, --priority, --assignee, ...)"),
            ("check", "Toggle done_criteria item (--task-id, --criterion-index, --checked)"),
            ("bulk-update", "Bulk update tasks (--updates '[{...}]')"),
            ("log-time", "Log hours worked (--task-id, --hours)"),
            ("import", "Import context from document (--source '{...}')"),
        ],
        "session" => &[
            ("load", "Load context at session start (--session-id)"),
            ("save", "Save context at session end (--summary, --session-status, ...)"),
            ("list", "List sessions (--status-filter, --limit)"),
            ("get", "Get session detail (--session-id)"),
            ("update", "Update active session (--add-decision '{...}', ...)"),
        ],
        "config" => &[
            ("get", "Read project config"),
            ("update", "Update config (--updates '{...}')"),
        ],
        "memory" => &[
            ("save", "Save a memory (--text, --kind, --tags, --scope-paths, --force)"),
            ("query", "Query memories (--text, --limit, --session-id)"),
            ("delete", "Delete a memory (--id)"),
            ("cleanup", "Housekeep memory store (--apply-exact-merges, --stale-days)"),
        ],
        "referral" => &[
            ("send", "Send a referral to another project (--summary, --target-project, ...)"),
            ("list", "List incoming referrals (--status-filter)"),
            ("get", "Get referral detail (--referral-id)"),
            ("update", "Update referral status (--referral-id, --status)"),
        ],
        "assignee" => &[
            ("list", "List team members"),
            ("add", "Add assignee (--key, --display-name, ...)"),
            ("update", "Update assignee (--key, --display-name, ...)"),
            ("remove", "Remove assignee (--key)"),
        ],
        "milestone" => &[
            ("list", "List milestones"),
            ("add", "Add milestone (--name, --date, --description)"),
            ("update", "Update milestone (--name, --date, --description)"),
            ("remove", "Remove milestone (--name)"),
        ],
        "calendar" => &[
            ("update", "Update calendar settings (--work-hours-per-day, --closed-weekdays, ...)"),
        ],
        "labels" => &[
            ("update", "Set project labels (--labels a,b,c)"),
        ],
        "project" => &[
            ("start", "Set project start date (--start-date, --shift-dates)"),
        ],
        "metrics" => &[
            ("", "Get project metrics (--assignee)"),
        ],
        "capacity" => &[
            ("", "Get work capacity (--start-date, --end-date, --assignee)"),
        ],
        "schedule" => &[
            ("", "Run auto-scheduler (--dry-run, --assignee-filter, --start-date)"),
        ],
        "dashboard" => &[
            ("", "Show cross-project dashboard (--scan-dirs, --include-completed)"),
        ],
        "timer" => &[
            ("start", "Start timer for task (--task-id)"),
            ("stop", "Stop timer for task (--task-id)"),
            ("get", "Get timer state (--task-id)"),
        ],
        _ => {
            eprintln!("Unknown command group: {group}");
            eprintln!("Run `handoff-mcp --help` for available commands.");
            return;
        }
    };

    let desc = GROUPS
        .iter()
        .find(|(n, _)| *n == group)
        .map(|(_, d)| *d)
        .unwrap_or("");
    println!("handoff-mcp {group} — {desc}\n");
    println!("ACTIONS:");
    for (action, help) in actions {
        if action.is_empty() {
            println!("    (default)       {help}");
        } else {
            println!("    {action:<16}{help}");
        }
    }
    println!(
        "\nGLOBAL OPTIONS:\n    --project-dir <path>    Project directory (default: current directory)"
    );
}

/// Check if this group/action is a known CLI command.
pub fn is_cli_command(first_arg: &str) -> bool {
    GROUPS.iter().any(|(name, _)| *name == first_arg)
}

use serde_json::{json, Value};

use super::types::ToolDefinition;

pub fn all_tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "handoff_init".to_string(),
            description: "Initialize handoff tracking for a new project. Creates .handoff/ directory structure.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "project_name": {
                        "type": "string",
                        "description": "Project name"
                    },
                    "description": {
                        "type": "string",
                        "description": "Project description"
                    }
                },
                "required": ["project_name"]
            }),
        },
        ToolDefinition {
            name: "handoff_load_context".to_string(),
            description: "Load handoff context for the current project. Call at session start to resume work. Can also resume a paused session by ID.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session ID to activate and load. Searches open sessions first, then paused sessions. If omitted, activates all open sessions and returns the latest."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_save_context".to_string(),
            description: "Save current session state for the next session. Call at session end.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "summary": {
                        "type": "string",
                        "description": "One-line summary of this session"
                    },
                    "session_status": {
                        "type": "string",
                        "description": "Session status after save. 'closed' (default) = close the active session as history. 'active' = keep or create an active session (use at session start to establish a persistent session that survives interruptions).",
                        "enum": ["closed", "active"],
                        "default": "closed"
                    },
                    "close_session_id": {
                        "type": "string",
                        "description": "Session ID to close. If omitted (and no pause options set), active sessions are closed."
                    },
                    "pause_session_id": {
                        "type": "string",
                        "description": "Session ID to pause instead of close. The paused session can be resumed later via load_context with the same session_id. Use this when switching to different work temporarily."
                    },
                    "pause_active": {
                        "type": "boolean",
                        "description": "If true, pause all active sessions instead of closing them. Cannot be combined with close_session_id."
                    },
                    "pause_only": {
                        "type": "boolean",
                        "description": "If true, only pause sessions (via pause_session_id or pause_active) without creating a new session. Useful for session switching. When true, summary is optional."
                    },
                    "decisions": {
                        "type": "array",
                        "description": "Decisions made during this session",
                        "items": {
                            "type": "object",
                            "properties": {
                                "decision": { "type": "string", "description": "What was decided" },
                                "reason": { "type": "string", "description": "Why this decision was made" },
                                "confidence": {
                                    "type": "string",
                                    "description": "confirmed = verified by testing/evidence; estimated = reasoned but not verified; unverified = hypothesis needing validation",
                                    "enum": ["confirmed", "estimated", "unverified"]
                                }
                            },
                            "required": ["decision"]
                        }
                    },
                    "blockers": {
                        "type": "array",
                        "description": "Issues preventing progress. The next session should address these before starting new work.",
                        "items": { "type": "string" }
                    },
                    "checklist": {
                        "type": "array",
                        "description": "Verification items for the next session or user. Mark completed items as checked:true before saving.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "item": { "type": "string", "description": "What to verify or confirm" },
                                "checked": { "type": "boolean", "description": "true if already verified, false if pending" },
                                "owner": {
                                    "type": "string",
                                    "description": "user = requires human action; ai = the next AI session should handle this",
                                    "enum": ["user", "ai"]
                                }
                            },
                            "required": ["item"]
                        }
                    },
                    "handoff_notes": {
                        "type": "array",
                        "description": "Notes for the next session. Include at least one 'suggestion' with a concrete next action.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "note": { "type": "string", "description": "The note content. For suggestions: state what is ALREADY DONE, then describe the concrete next action." },
                                "category": {
                                    "type": "string",
                                    "description": "caution = risks/rules the next session must respect; context = background info for decisions; suggestion = concrete next action the next session should execute first (at least one required)",
                                    "enum": ["caution", "context", "suggestion"]
                                }
                            },
                            "required": ["note"]
                        }
                    },
                    "references": {
                        "type": "array",
                        "description": "Links to related docs, issues, MRs, or external resources for reference (not active work files — use context_pointers for those).",
                        "items": {
                            "type": "object",
                            "properties": {
                                "label": { "type": "string", "description": "Human-readable label for this reference" },
                                "uri": { "type": "string", "description": "Path, URL, or identifier" },
                                "type": {
                                    "type": "string",
                                    "description": "file = project file; issue = issue tracker; mr = merge/pull request; wiki = wiki page; doc = design document; url = external URL",
                                    "enum": ["file", "issue", "mr", "wiki", "doc", "url"]
                                },
                                "notes": { "type": "string", "description": "Additional context (e.g. 'see section 3 for root cause analysis')" }
                            },
                            "required": ["label", "uri"]
                        }
                    },
                    "context_pointers": {
                        "type": "array",
                        "description": "Files the next session should open first to resume work. Point to files that NEED WORK, not completed files. For completed files, use a 'context' handoff_note instead.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "path": { "type": "string", "description": "File path relative to project root" },
                                "reason": { "type": "string", "description": "Why the next session should read this (e.g. 'resume implementation here', 'needs review')" },
                                "lines": { "type": "string", "description": "Line range to focus on (e.g. '42-78')" }
                            },
                            "required": ["path"]
                        }
                    },
                    "environment": {
                        "type": "object",
                        "description": "Free-form environment state"
                    }
                },
                "required": ["summary"]
            }),
        },
        ToolDefinition {
            name: "handoff_list_tasks".to_string(),
            description: "List all tasks for the current project with optional status filter.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "status_filter": {
                        "type": "string",
                        "description": "Filter by status",
                        "enum": ["todo", "in_progress", "review", "done", "blocked", "skipped"]
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_get_task".to_string(),
            description: "Get full task details (notes, done_criteria, labels, links) by task ID. Use when list_tasks summary is not enough.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to retrieve (e.g. 't1', 't1.2')."
                    }
                },
                "required": ["task_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_check_criterion".to_string(),
            description: "Toggle a single done_criteria item by index. No need to resend the entire criteria list.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID containing the criterion."
                    },
                    "criterion_index": {
                        "type": "integer",
                        "description": "0-based index of the done_criteria item to toggle."
                    },
                    "checked": {
                        "type": "boolean",
                        "description": "true to mark as checked, false to uncheck."
                    }
                },
                "required": ["task_id", "criterion_index", "checked"]
            }),
        },
        ToolDefinition {
            name: "handoff_update_task".to_string(),
            description: "Add, update, or move a task. Manages the tasks/ directory structure.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "task": {
                        "type": "object",
                        "properties": {
                            "id": { "type": "string", "description": "Task ID. Omit for auto-generated ID. If provided and task exists, updates it. If provided and task does not exist, creates a new task with that ID (upsert)." },
                            "title": { "type": "string", "description": "Required for new tasks. Optional when updating (id present)." },
                            "status": {
                                "type": "string",
                                "enum": ["todo", "in_progress", "review", "done", "blocked", "skipped"]
                            },
                            "notes": { "type": "string" },
                            "priority": {
                                "type": "string",
                                "enum": ["low", "medium", "high"]
                            },
                            "labels": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "links": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "done_criteria": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "item": { "type": "string" },
                                        "checked": { "type": "boolean" }
                                    },
                                    "required": ["item"]
                                }
                            },
                            "schedule": {
                                "type": "object",
                                "description": "Schedule and effort tracking.",
                                "properties": {
                                    "start_date": { "type": "string", "description": "YYYY-MM-DD" },
                                    "due_date": { "type": "string", "description": "YYYY-MM-DD" },
                                    "estimate_hours": { "type": "number" },
                                    "actual_hours": { "type": "number" },
                                    "milestone": { "type": "string" }
                                }
                            },
                            "dependencies": {
                                "type": "array",
                                "description": "Task IDs this task depends on. Circular dependencies are rejected.",
                                "items": { "type": "string" }
                            },
                            "order": {
                                "type": "integer",
                                "description": "Display order among siblings. 0-based, lower = higher priority."
                            }
                        },
                    },
                    "parent_id": {
                        "type": "string",
                        "description": "Parent task ID for placement. Omit for auto-placement."
                    },
                    "move_to": {
                        "type": "string",
                        "description": "Move existing task subtree to a new parent."
                    }
                },
                "required": ["task"]
            }),
        },
        ToolDefinition {
            name: "handoff_get_config".to_string(),
            description: "Read the project's handoff configuration.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_update_config".to_string(),
            description: "Update the project's handoff configuration.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "updates": {
                        "type": "object",
                        "description": "Key-value pairs to update (dot-notation keys like 'settings.history_limit')"
                    }
                },
                "required": ["updates"]
            }),
        },
        ToolDefinition {
            name: "handoff_dashboard".to_string(),
            description: "Show handoff status across all projects in configured scan directories.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "scan_dirs": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Directories to scan. Defaults to config's dashboard.scan_dirs."
                    },
                    "include_completed": {
                        "type": "boolean",
                        "description": "Include completed tasks in summary"
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_import_context".to_string(),
            description: "Import existing handoff documents into .handoff/ management. AI reads the source material, structures it, and submits everything in one call. Supports nested task hierarchies via children field.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "source": {
                        "type": "object",
                        "description": "Metadata about the original document being imported",
                        "properties": {
                            "description": {
                                "type": "string",
                                "description": "What is being imported (e.g. 'Migration from tmp/260601-sprint-handoff.md')"
                            },
                            "format": {
                                "type": "string",
                                "enum": ["markdown", "json", "text", "other"],
                                "description": "Format of the source material. Defaults to 'other'."
                            }
                        },
                        "required": ["description"]
                    },
                    "tasks": {
                        "type": "array",
                        "description": "Tasks to import. Supports nested hierarchies via children field.",
                        "items": {
                            "$ref": "#/$defs/importTask"
                        }
                    },
                    "session": {
                        "type": "object",
                        "description": "Session context to save. Same fields as handoff_save_context.",
                        "properties": {
                            "summary": { "type": "string", "description": "One-line summary (required)" },
                            "decisions": {
                                "type": "array",
                                "description": "Decisions made during this session",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "decision": { "type": "string", "description": "What was decided" },
                                        "reason": { "type": "string", "description": "Why this decision was made" },
                                        "confidence": {
                                            "type": "string",
                                            "description": "confirmed = verified; estimated = reasoned but not verified; unverified = hypothesis",
                                            "enum": ["confirmed", "estimated", "unverified"]
                                        }
                                    },
                                    "required": ["decision"]
                                }
                            },
                            "blockers": {
                                "type": "array",
                                "description": "Issues preventing progress",
                                "items": { "type": "string" }
                            },
                            "checklist": {
                                "type": "array",
                                "description": "Verification items for the next session or user",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "item": { "type": "string", "description": "What to verify" },
                                        "checked": { "type": "boolean", "description": "true if verified, false if pending" },
                                        "owner": { "type": "string", "description": "user = human action; ai = next AI session", "enum": ["user", "ai"] }
                                    },
                                    "required": ["item"]
                                }
                            },
                            "handoff_notes": {
                                "type": "array",
                                "description": "Notes for the next session. Include at least one 'suggestion' with a concrete next action.",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "note": { "type": "string", "description": "The note content. For suggestions: state what is done, then the next action." },
                                        "category": { "type": "string", "description": "caution = risks/rules; context = background; suggestion = concrete next action (at least one required)", "enum": ["caution", "context", "suggestion"] }
                                    },
                                    "required": ["note"]
                                }
                            },
                            "references": {
                                "type": "array",
                                "description": "Links to related docs, issues, MRs (not active work files)",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "label": { "type": "string", "description": "Human-readable label" },
                                        "uri": { "type": "string", "description": "Path, URL, or identifier" },
                                        "type": { "type": "string", "description": "file/issue/mr/wiki/doc/url", "enum": ["file", "issue", "mr", "wiki", "doc", "url"] },
                                        "notes": { "type": "string", "description": "Additional context" }
                                    },
                                    "required": ["label", "uri"]
                                }
                            },
                            "context_pointers": {
                                "type": "array",
                                "description": "Files the next session should open first to resume work (not completed files)",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "path": { "type": "string", "description": "File path relative to project root" },
                                        "reason": { "type": "string", "description": "Why to read this file" },
                                        "lines": { "type": "string", "description": "Line range (e.g. '42-78')" }
                                    },
                                    "required": ["path"]
                                }
                            },
                            "environment": {
                                "type": "object",
                                "description": "Free-form environment state"
                            }
                        },
                        "required": ["summary"]
                    },
                    "raw_notes": {
                        "type": "string",
                        "description": "Free-form text that couldn't be structured. Saved as a handoff_note with category 'context'."
                    },
                    "skip_session_close": {
                        "type": "boolean",
                        "description": "If true, do not close active sessions before creating the import session. Default false."
                    }
                },
                "required": ["source"],
                "$defs": {
                    "importTask": {
                        "type": "object",
                        "properties": {
                            "title": { "type": "string" },
                            "status": {
                                "type": "string",
                                "enum": ["todo", "in_progress", "review", "done", "blocked", "skipped"]
                            },
                            "notes": { "type": "string" },
                            "priority": {
                                "type": "string",
                                "enum": ["low", "medium", "high"]
                            },
                            "labels": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "links": {
                                "type": "array",
                                "items": { "type": "string" }
                            },
                            "done_criteria": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "properties": {
                                        "item": { "type": "string" },
                                        "checked": { "type": "boolean" }
                                    },
                                    "required": ["item"]
                                }
                            },
                            "children": {
                                "type": "array",
                                "description": "Nested child tasks. Recursively supports the same structure.",
                                "items": {
                                    "$ref": "#/$defs/importTask"
                                }
                            }
                        },
                        "required": ["title"]
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_refer".to_string(),
            description: "Send a cross-project referral (improvement request, bug report, work request) to another project's .handoff/. The target project sees it on load_context.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Source project directory (sender). Defaults to current working directory."
                    },
                    "target_project": {
                        "type": "string",
                        "description": "Target project name (resolved via scan_dirs). Use this OR target_project_dir."
                    },
                    "target_project_dir": {
                        "type": "string",
                        "description": "Target project directory path (absolute). Takes precedence over target_project."
                    },
                    "referral_type": {
                        "type": "string",
                        "enum": ["improvement", "bug", "request", "info"],
                        "description": "Type of referral. Defaults to 'request'."
                    },
                    "summary": {
                        "type": "string",
                        "description": "One-line summary of the referral."
                    },
                    "details": {
                        "type": "string",
                        "description": "Detailed description of the referral."
                    },
                    "priority": {
                        "type": "string",
                        "enum": ["low", "medium", "high"],
                        "description": "Priority of the referral."
                    },
                    "tasks": {
                        "type": "array",
                        "description": "Suggested tasks for the target project.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "title": { "type": "string" },
                                "priority": { "type": "string", "enum": ["low", "medium", "high"] },
                                "done_criteria": {
                                    "type": "array",
                                    "items": {
                                        "type": "object",
                                        "properties": {
                                            "item": { "type": "string" },
                                            "checked": { "type": "boolean" }
                                        },
                                        "required": ["item"]
                                    }
                                }
                            },
                            "required": ["title"]
                        }
                    },
                    "context": {
                        "type": "object",
                        "description": "Additional context (branch, commit, references)."
                    }
                },
                "required": ["summary"]
            }),
        },
        ToolDefinition {
            name: "handoff_list_referrals".to_string(),
            description: "List incoming referrals from other projects with optional status filter.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "status_filter": {
                        "type": "string",
                        "enum": ["open", "acknowledged", "resolved"],
                        "description": "Filter by referral status."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_update_referral".to_string(),
            description: "Update the status of an incoming referral (open -> acknowledged -> resolved).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "referral_id": {
                        "type": "string",
                        "description": "ID of the referral to update."
                    },
                    "status": {
                        "type": "string",
                        "enum": ["open", "acknowledged", "resolved"],
                        "description": "New status for the referral."
                    }
                },
                "required": ["referral_id", "status"]
            }),
        },
    ]
}

pub fn all_resource_definitions() -> Vec<Value> {
    vec![
        json!({
            "uri": "handoff://sessions",
            "name": "Active Sessions",
            "description": "All active session files for the current project",
            "mimeType": "application/json"
        }),
        json!({
            "uri": "handoff://config",
            "name": "Project Configuration",
            "description": "Current project's config.toml content",
            "mimeType": "application/toml"
        }),
    ]
}

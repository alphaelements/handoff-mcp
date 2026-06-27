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
            description: "List all tasks for the current project with optional filters.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "status_filter": {
                        "type": "string",
                        "description": "Filter by status.",
                        "enum": ["todo", "in_progress", "review", "done", "blocked", "skipped"]
                    },
                    "assignee_filter": {
                        "type": "string",
                        "description": "Filter by assignee key."
                    },
                    "milestone_filter": {
                        "type": "string",
                        "description": "Filter by milestone name."
                    },
                    "priority_filter": {
                        "type": "string",
                        "description": "Filter by priority.",
                        "enum": ["low", "medium", "high"]
                    },
                    "label_filter": {
                        "type": "string",
                        "description": "Filter by label (task must contain this label)."
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
                                    "remaining_hours": { "type": "number", "description": "Hours remaining. Auto-decremented by handoff_log_time." },
                                    "milestone": { "type": "string" },
                                    "pinned": { "type": "boolean", "description": "If true, dates are locked and auto-scheduler skips this task." }
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
                            },
                            "assignee": {
                                "type": "string",
                                "description": "Assignee key (matches config.toml [assignees.<key>])."
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
            name: "handoff_get_referral".to_string(),
            description: "Get the full details of a single incoming referral by ID (summary, details, tasks with done_criteria, priority, context, status). Use this instead of reading .handoff/referrals/*.json directly.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "referral_id": {
                        "type": "string",
                        "description": "ID of the referral to retrieve (full id or a unique prefix)."
                    }
                },
                "required": ["referral_id"]
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
        ToolDefinition {
            name: "handoff_update_session".to_string(),
            description: "Incrementally update the active session. Toggle checklist items, add decisions, notes, or context pointers without resending everything. Use during work for progressive updates.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "checklist_index": {
                        "type": "integer",
                        "description": "0-based index of a checklist item to toggle."
                    },
                    "checklist_checked": {
                        "type": "boolean",
                        "description": "Set the checklist item to checked (true) or unchecked (false). Defaults to true."
                    },
                    "add_checklist_item": {
                        "type": "string",
                        "description": "Text of a new checklist item to add (unchecked)."
                    },
                    "checklist_owner": {
                        "type": "string",
                        "description": "Owner for the new checklist item: 'user' or 'ai'. Defaults to 'ai'.",
                        "enum": ["user", "ai"]
                    },
                    "add_decision": {
                        "type": "object",
                        "description": "A decision to append to the session.",
                        "properties": {
                            "decision": { "type": "string" },
                            "reason": { "type": "string" },
                            "confidence": { "type": "string", "enum": ["confirmed", "estimated", "unverified"] }
                        },
                        "required": ["decision"]
                    },
                    "add_handoff_note": {
                        "type": "object",
                        "description": "A handoff note to append to the session.",
                        "properties": {
                            "note": { "type": "string" },
                            "category": { "type": "string", "enum": ["caution", "context", "suggestion"] }
                        },
                        "required": ["note"]
                    },
                    "add_context_pointer": {
                        "type": "object",
                        "description": "A context pointer to append to the session.",
                        "properties": {
                            "path": { "type": "string" },
                            "reason": { "type": "string" },
                            "lines": { "type": "string" }
                        },
                        "required": ["path"]
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_log_time".to_string(),
            description: "Log hours worked on a task. Adds to actual_hours and deducts from remaining_hours atomically.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to log time against."
                    },
                    "hours": {
                        "type": "number",
                        "description": "Hours worked (e.g. 0.5 for 30 minutes)."
                    }
                },
                "required": ["task_id", "hours"]
            }),
        },
        ToolDefinition {
            name: "handoff_get_metrics".to_string(),
            description: "Get project metrics: completion %, effort tracking, overdue tasks, budget status, and milestone breakdown.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "assignee": {
                        "type": "string",
                        "description": "Filter metrics to a specific assignee."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_list_sessions".to_string(),
            description: "List all sessions (open, active, paused, closed) with summary info. Use handoff_get_session for full detail.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "status_filter": {
                        "type": "string",
                        "enum": ["open", "active", "paused", "closed"],
                        "description": "Filter sessions by status."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max sessions to return (default 20)."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_list_assignees".to_string(),
            description: "List all team members/assignees from config.toml with their task counts and effort stats.".to_string(),
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
            name: "handoff_bulk_update_tasks".to_string(),
            description: "Update multiple tasks in one call. Useful for applying auto-schedule results or bulk status/assignee changes.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "updates": {
                        "type": "array",
                        "description": "Array of task updates to apply.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_id": { "type": "string", "description": "Task ID to update." },
                                "status": { "type": "string", "enum": ["todo", "in_progress", "review", "done", "blocked", "skipped"] },
                                "priority": { "type": "string", "enum": ["low", "medium", "high"] },
                                "assignee": { "type": "string" },
                                "schedule": {
                                    "type": "object",
                                    "properties": {
                                        "start_date": { "type": "string" },
                                        "due_date": { "type": "string" },
                                        "estimate_hours": { "type": "number" },
                                        "actual_hours": { "type": "number" },
                                        "remaining_hours": { "type": "number" },
                                        "milestone": { "type": "string" },
                                        "pinned": { "type": "boolean" }
                                    }
                                }
                            },
                            "required": ["task_id"]
                        }
                    }
                },
                "required": ["updates"]
            }),
        },
        ToolDefinition {
            name: "handoff_get_session".to_string(),
            description: "Get full detail of a specific session by ID. Returns decisions, checklist, handoff notes, context pointers, etc.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session ID to retrieve."
                    }
                },
                "required": ["session_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_get_capacity".to_string(),
            description: "Get work capacity for a date range. Shows available hours per day based on calendar config, and allocated hours from scheduled tasks.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "start_date": {
                        "type": "string",
                        "description": "Start date (YYYY-MM-DD)."
                    },
                    "end_date": {
                        "type": "string",
                        "description": "End date (YYYY-MM-DD)."
                    },
                    "assignee": {
                        "type": "string",
                        "description": "Filter capacity to a specific assignee's calendar."
                    }
                },
                "required": ["start_date", "end_date"]
            }),
        },
        ToolDefinition {
            name: "handoff_auto_schedule".to_string(),
            description: "Run auto-scheduler to compute optimal task dates based on dependencies, estimates, and calendar capacity. Returns change diff; applies changes unless dry_run=true.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "dry_run": {
                        "type": "boolean",
                        "description": "If true (default), return computed spans without writing. If false, apply changes to task files."
                    },
                    "assignee_filter": {
                        "type": "string",
                        "description": "Only schedule tasks assigned to this assignee."
                    },
                    "start_date": {
                        "type": "string",
                        "description": "Anchor date YYYY-MM-DD for the earliest task. Defaults to today (UTC)."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_add_assignee".to_string(),
            description: "Add a team member to config.toml [assignees.<key>]. Fails if the key already exists.".to_string(),
            input_schema: assignee_write_schema(true),
        },
        ToolDefinition {
            name: "handoff_update_assignee".to_string(),
            description: "Update an existing [assignees.<key>] entry. Only provided fields change; pass null to clear a field.".to_string(),
            input_schema: assignee_write_schema(false),
        },
        ToolDefinition {
            name: "handoff_remove_assignee".to_string(),
            description: "Remove a team member from config.toml and unassign them from every task.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "key": { "type": "string", "description": "Assignee key to remove." }
                },
                "required": ["key"]
            }),
        },
        ToolDefinition {
            name: "handoff_list_milestones".to_string(),
            description: "List all milestones defined in config.toml [milestones.*].".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_add_milestone".to_string(),
            description: "Add a milestone to config.toml [milestones.<name>]. Fails if it already exists.".to_string(),
            input_schema: milestone_write_schema(),
        },
        ToolDefinition {
            name: "handoff_update_milestone".to_string(),
            description: "Update an existing [milestones.<name>] entry. Pass null to clear a field.".to_string(),
            input_schema: milestone_write_schema(),
        },
        ToolDefinition {
            name: "handoff_remove_milestone".to_string(),
            description: "Remove a milestone from config.toml.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "name": { "type": "string", "description": "Milestone name to remove." }
                },
                "required": ["name"]
            }),
        },
        ToolDefinition {
            name: "handoff_update_calendar".to_string(),
            description: "Patch the project [calendar] section (work hours, closed days, day_hours, schedule_mode). Only provided fields change.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "work_hours_per_day": { "type": "number", "description": "Default working hours per day." },
                    "closed_weekdays": { "type": "array", "description": "Non-working weekdays (0=Sun..6=Sat, or names like \"sat\").", "items": {} },
                    "closed_dates": { "type": "array", "description": "Non-working YYYY-MM-DD dates.", "items": { "type": "string" } },
                    "open_dates": { "type": "array", "description": "Working YYYY-MM-DD dates that override closed weekdays.", "items": { "type": "string" } },
                    "day_hours": { "type": "object", "description": "Per-weekday-name or per-date hour overrides, e.g. {\"fri\": 4, \"2026-07-01\": 0}.", "additionalProperties": { "type": "number" } },
                    "schedule_mode": { "type": "string", "description": "\"manual\" or \"auto\"." },
                    "overwork_limit_percent": { "type": "number" },
                    "max_utilization": { "type": "number" }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_update_labels".to_string(),
            description: "Set the project-level label vocabulary (top-level labels array in config.toml).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "labels": { "type": "array", "description": "Full replacement list of project labels.", "items": { "type": "string" } }
                },
                "required": ["labels"]
            }),
        },
        ToolDefinition {
            name: "handoff_start_project".to_string(),
            description: "Set the project started_at date and optionally shift all task dates so the earliest start aligns to it.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "start_date": { "type": "string", "description": "Project start date YYYY-MM-DD. Defaults to today (UTC)." },
                    "shift_dates": { "type": "boolean", "description": "If true, shift every task's start/due dates so the earliest start lands on start_date." }
                }
            }),
        },
        ToolDefinition {
            name: "memory_save".to_string(),
            description: "Save a long-lived project memory (lesson/rule/convention/gotcha) that future sessions should respect. Detects exact and near-duplicate memories: an exact match is reported (not rewritten), a near-duplicate is returned as a 'conflict' with both bodies for you to merge (call again with merge_into=<id> and absorb_ids=[…]) or save separately with force=true. Returns a JSON string.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "text": { "type": "string", "description": "The memory body (any language). Required, non-empty." },
                    "kind": { "type": "string", "description": "Memory kind.", "enum": ["lesson", "rule", "convention", "gotcha"], "default": "lesson" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional tags; also indexed for similarity." },
                    "scope_paths": { "type": "array", "items": { "type": "string" }, "description": "Path prefixes this memory applies to (e.g. 'src/storage/'). Boosts relevance when a query touches a matching file." },
                    "merge_into": { "type": "string", "description": "Commit an AI merge: overwrite this memory id with `text` and absorb `absorb_ids`." },
                    "absorb_ids": { "type": "array", "items": { "type": "string" }, "description": "Memory ids to delete and record as superseded when merging." },
                    "force": { "type": "boolean", "description": "Save even if a near-duplicate exists (skip the conflict response).", "default": false }
                },
                "required": ["text"]
            }),
        },
        ToolDefinition {
            name: "memory_query".to_string(),
            description: "Return the project memories most relevant to the given text/file (BM25 + scope-path boosting). Intended for automatic injection via hooks, but callable directly. Returns a JSON string {\"memories\":[{id,text,kind,score}],\"injected_count\"}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "session_id": { "type": "string", "description": "Hook session id. When given, memories already injected this session (same content hash) are filtered out; an edited memory is re-injected." },
                    "text": { "type": "string", "description": "The current prompt or context text to match against." },
                    "tool_name": { "type": "string", "description": "Name of the tool about to run (e.g. 'Edit'); added to the query." },
                    "file_paths": { "type": "array", "items": { "type": "string" }, "description": "File paths in play; basenames are added to the query and scope_paths are matched against these." },
                    "limit": { "type": "integer", "description": "Maximum memories to return.", "default": 5 },
                    "mark_injected": { "type": "boolean", "description": "Record returned memories in the session sidecar and bump their hit_count/last_referenced_at. Requires session_id.", "default": true }
                },
                "required": ["text"]
            }),
        },
        ToolDefinition {
            name: "memory_delete".to_string(),
            description: "Delete a project memory by id (full id or unique prefix). Use for AI-driven cleanup of stale memories. Returns a JSON string {\"status\":\"deleted\",\"id\"}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "id": { "type": "string", "description": "Memory id to delete (full id or unique prefix)." }
                },
                "required": ["id"]
            }),
        },
    ]
}

/// Shared input schema for add/update assignee. `key` is required either way.
fn assignee_write_schema(_is_add: bool) -> Value {
    json!({
        "type": "object",
        "properties": {
            "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
            "key": { "type": "string", "description": "Stable assignee key (used as [assignees.<key>])." },
            "display_name": { "type": "string", "description": "Human-readable name." },
            "color": { "type": "string", "description": "Display color (hex or name)." },
            "work_hours_per_day": { "type": "number", "description": "This member's daily working hours." },
            "closed_weekdays": { "type": "array", "description": "Non-working weekdays (0=Sun..6=Sat or names).", "items": {} },
            "closed_dates": { "type": "array", "description": "Non-working YYYY-MM-DD dates.", "items": { "type": "string" } },
            "open_dates": { "type": "array", "description": "Working YYYY-MM-DD override dates.", "items": { "type": "string" } },
            "day_hours": { "type": "object", "description": "Per-weekday/date hour overrides.", "additionalProperties": { "type": "number" } }
        },
        "required": ["key"]
    })
}

/// Shared input schema for add/update milestone. `name` is required.
fn milestone_write_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
            "name": { "type": "string", "description": "Milestone name (used as [milestones.<name>])." },
            "date": { "type": "string", "description": "Target date YYYY-MM-DD." },
            "color": { "type": "string", "description": "Display color." },
            "description": { "type": "string", "description": "Free-form description." }
        },
        "required": ["name"]
    })
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

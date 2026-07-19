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
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Target active session ID. When multiple active sessions exist, specifies which to update/close. If omitted, uses the latest active session. Lower priority than close_session_id / pause_session_id."
                    },
                    "timeline": {
                        "type": "string",
                        "description": "Session timeline/group label (e.g. 'feature-x', 'hotfix-y')."
                    },
                    "label": {
                        "type": "string",
                        "description": "Short human-readable session label for switching UI (e.g. 'WT2作業', 'API設計')."
                    },
                    "related_task_ids": {
                        "type": "array",
                        "description": "Task IDs this session is primarily working on.",
                        "items": { "type": "string" }
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
                    },
                    "include_children": {
                        "type": "boolean",
                        "description": "If true, recursively scan project_dir for child .handoff/ projects and include their tasks. Each task gets project_name, project_dir, and task_ref fields (task_ref is a composite '{project_name}-{hash}:{id}' identifier unique across projects). The original 'id' field is left unchanged so it stays usable with handoff_get_task/handoff_update_task/dependencies when paired with the task's own project_dir. Default: false."
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
            description: "Add, update, or move a task. Manages the tasks/ directory structure. Include task.schedule.estimate_hours (raw human-effort hours, > 0) when moving a leaf task to in_progress/review/done; it is rejected without one unless the task is a parent, todo, blocked, or skipped.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "task": {
                        "type": "object",
                        "description": "The task to add or update. schedule.estimate_hours is REQUIRED when a leaf task is in status in_progress/review/done. Omit it for parent tasks (any task with children) or status todo/blocked/skipped.",
                        "properties": {
                            "id": { "type": "string", "description": "Task ID. Omit for auto-generated ID. If provided and task exists, updates it. If provided and task does not exist, creates a new task with that ID (upsert)." },
                            "title": { "type": "string", "description": "Required for new tasks. Optional when updating (id present)." },
                            "status": {
                                "type": "string",
                                "enum": ["todo", "in_progress", "review", "done", "blocked", "skipped"]
                            },
                            "notes": { "type": "string" },
                            "notes_append": { "type": "string", "description": "Append text to existing notes with a timestamp heading. If both notes and notes_append are provided, notes (replace) takes precedence." },
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
                                "description": "Schedule and effort tracking. Supply estimate_hours when a leaf task enters in_progress/review/done.",
                                "properties": {
                                    "start_date": { "type": "string", "description": "YYYY-MM-DD" },
                                    "due_date": { "type": "string", "description": "YYYY-MM-DD" },
                                    "estimate_hours": { "type": "number", "description": "REQUIRED for leaf tasks in status in_progress/review/done; the call is rejected without it. Omit for parent tasks (any task with children) or status todo/blocked/skipped. Raw human-effort hours, > 0 — do not pre-multiply by settings.ai_estimate_multiplier, which is applied at aggregation time." },
                                    "actual_hours": { "type": "number", "description": "Hours actually spent. Prefer handoff_log_time, which adds to this and decrements remaining_hours atomically." },
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
                    "max_depth": {
                        "type": "integer",
                        "description": "Maximum directory depth for recursive scanning. Defaults to config's dashboard.max_depth (5)."
                    },
                    "exclude_patterns": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Directory names to skip during recursive scanning (exact match). Defaults to config's dashboard.exclude_patterns."
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
                    "session_id": {
                        "type": "string",
                        "description": "Target active session ID. When multiple active sessions exist, specifies which to update. If omitted and multiple exist, uses the latest."
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
                    "timeline": {
                        "type": "string",
                        "description": "Filter sessions by timeline label."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max sessions to return (default 20)."
                    },
                    "include_children": {
                        "type": "boolean",
                        "description": "If true, include a 'children' array on each session showing its forked child sessions."
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
            description: "Update multiple tasks in one call. Useful for applying auto-schedule results or bulk status/assignee changes. Enforces the same estimate rule as handoff_update_task: a leaf task in status in_progress/review/done must carry schedule.estimate_hours (> 0). Offending updates are rejected individually and reported in errors[]; the rest still apply.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "updates": {
                        "type": "array",
                        "description": "Array of task updates to apply. Each is validated on its own: if an update would leave a leaf task in status in_progress/review/done without schedule.estimate_hours, that update is rejected and listed in errors[] while the others still apply. Supply estimate_hours in the same update to move an estimateless task out of blocked/skipped or todo.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "task_id": { "type": "string", "description": "Task ID to update." },
                                "status": { "type": "string", "enum": ["todo", "in_progress", "review", "done", "blocked", "skipped"], "description": "Moving a leaf task into in_progress/review/done requires schedule.estimate_hours to be present or supplied in the same update. Parent tasks (any task with children) and the statuses todo/blocked/skipped are exempt." },
                                "priority": { "type": "string", "enum": ["low", "medium", "high"] },
                                "assignee": { "type": "string" },
                                "notes": { "type": "string", "description": "Replace task notes." },
                                "notes_append": { "type": "string", "description": "Append text to existing notes with a timestamp heading. If both notes and notes_append are provided, notes (replace) takes precedence." },
                                "schedule": {
                                    "type": "object",
                                    "description": "Schedule fields to merge. Omitted fields are preserved, not cleared.",
                                    "properties": {
                                        "start_date": { "type": "string", "description": "YYYY-MM-DD" },
                                        "due_date": { "type": "string", "description": "YYYY-MM-DD" },
                                        "estimate_hours": { "type": "number", "description": "REQUIRED for a leaf task in status in_progress/review/done; the update is rejected without it. Omit for parent tasks (any task with children) or status todo/blocked/skipped. Raw human-effort hours, > 0 — do not pre-multiply by settings.ai_estimate_multiplier, which is applied at aggregation time." },
                                        "actual_hours": { "type": "number", "description": "Hours actually spent. Prefer handoff_log_time, which adds to this and decrements remaining_hours atomically." },
                                        "remaining_hours": { "type": "number", "description": "Hours remaining. Auto-decremented by handoff_log_time." },
                                        "milestone": { "type": "string" },
                                        "pinned": { "type": "boolean", "description": "If true, dates are locked and auto-scheduler skips this task." }
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
            name: "handoff_memory_save".to_string(),
            description: "Save a long-lived project memory (lesson/rule/convention/gotcha) that future sessions should respect. Detects exact and near-duplicate memories: an exact match is reported (not rewritten), a near-duplicate is returned as a 'conflict' with both bodies for you to merge (call again with merge_into=<id> and absorb_ids=[…]) or save separately with force=true. Returns a JSON string.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "text": { "type": "string", "description": "The memory body (any language). Required, non-empty." },
                    "kind": { "type": "string", "description": "Memory kind.", "enum": ["lesson", "rule", "convention", "gotcha"], "default": "lesson" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Optional tags; also indexed for similarity." },
                    "keywords": { "type": "array", "items": { "type": "string" }, "description": "Subject keywords — nouns, technical terms, proper nouns that identify what this memory is about. These are weighted higher than body text in BM25 relevance scoring. Distinct from tags (classification labels)." },
                    "scope_paths": { "type": "array", "items": { "type": "string" }, "description": "Path prefixes this memory applies to (e.g. 'src/storage/'). Boosts relevance when a query touches a matching file." },
                    "merge_into": { "type": "string", "description": "Commit an AI merge: overwrite this memory id with `text` and absorb `absorb_ids`." },
                    "absorb_ids": { "type": "array", "items": { "type": "string" }, "description": "Memory ids to delete and record as superseded when merging." },
                    "force": { "type": "boolean", "description": "Save even if a near-duplicate exists (skip the conflict response).", "default": false }
                },
                "required": ["text"]
            }),
        },
        ToolDefinition {
            name: "handoff_memory_query".to_string(),
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
            name: "handoff_memory_delete".to_string(),
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
        ToolDefinition {
            name: "handoff_memory_cleanup".to_string(),
            description: "Housekeep the project memory store (intended for SessionStart). Silently merges exact duplicates (lossless), then returns recommendations the AI should act on: near-duplicate clusters (merge with memory_save merge_into=…) and stale memories (consider memory_delete). Also garbage-collects old per-session injection sidecars. Returns a JSON string {\"auto_merged_exact\":n,\"cleanup_recommendations\":{\"similar_clusters\":[…],\"stale\":[…]},\"injected_sidecars_removed\":k}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "apply_exact_merges": { "type": "boolean", "description": "Auto-merge exact-duplicate memories (same content hash). Lossless and safe.", "default": true },
                    "stale_days": { "type": "integer", "description": "Flag memories not referenced for this many days as stale recommendations.", "default": 60 }
                }
            }),
        },
        // ---- Session fork/merge tools ----
        ToolDefinition {
            name: "handoff_fork_session".to_string(),
            description: "Fork a new session from an existing one. Inherits decisions, context_pointers, references, and handoff_notes by default. The forked session becomes active with parent_session_id set.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "source_session_id": {
                        "type": "string",
                        "description": "Session ID to fork from (active, paused, or closed)."
                    },
                    "summary": {
                        "type": "string",
                        "description": "Summary for the new forked session."
                    },
                    "label": {
                        "type": "string",
                        "description": "Short human-readable label for the forked session."
                    },
                    "timeline": {
                        "type": "string",
                        "description": "Timeline label. Defaults to the source session's timeline."
                    },
                    "inherit": {
                        "type": "array",
                        "description": "Fields to inherit from the source. Default: [\"decisions\", \"context_pointers\", \"references\", \"handoff_notes\", \"environment\"]. Available: decisions, context_pointers, references, handoff_notes, environment, blockers, checklist.",
                        "items": { "type": "string" }
                    },
                    "related_task_ids": {
                        "type": "array",
                        "description": "Task IDs the forked session will work on.",
                        "items": { "type": "string" }
                    }
                },
                "required": ["source_session_id", "summary"]
            }),
        },
        ToolDefinition {
            name: "handoff_merge_sessions".to_string(),
            description: "Merge multiple sessions into one. Combines decisions, notes, references, and context_pointers. Detects duplicate decisions as conflicts. Source sessions (except the target) are closed by default.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "source_session_ids": {
                        "type": "array",
                        "description": "Session IDs to merge (must include at least 2).",
                        "items": { "type": "string" }
                    },
                    "target_session_id": {
                        "type": "string",
                        "description": "Which source session becomes the merge target (must be one of source_session_ids)."
                    },
                    "close_sources": {
                        "type": "boolean",
                        "description": "Close non-target source sessions after merge. Default: true."
                    }
                },
                "required": ["source_session_ids", "target_session_id"]
            }),
        },
        // ---- Timer coordination tools ----
        ToolDefinition {
            name: "handoff_timer_start".to_string(),
            description: "Start a timer for a task. If VSCode extension is running (authority alive), delegates to the extension via a request file. Otherwise starts an MCP fallback timer.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task ID to start timing (e.g. 't1', 't1.2')." },
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." }
                },
                "required": ["task_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_timer_stop".to_string(),
            description: "Stop the timer for a task. If VSCode extension is the authority, delegates the stop command. If MCP is the authority (fallback), stops the internal timer and adds elapsed time to the task's actual_hours (with optimistic locking).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task ID to stop timing." },
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." }
                },
                "required": ["task_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_timer_get_time".to_string(),
            description: "Get the current timer state for a task. Returns elapsed time, timer state (tracking/paused/stopped), authority info, and projected total hours. Reads from .handoff/timer/state.json.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": { "type": "string", "description": "Task ID to query timer for." },
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." }
                },
                "required": ["task_id"]
            }),
        },
        // ---- Document management tools (P1-6a, v5 rearchitecture: wiki/130-document-management.md §3.1) ----
        ToolDefinition {
            name: "handoff_doc_save".to_string(),
            description: "Save a complete document. The body MUST be a whole, human-readable Markdown document starting with a level-1 heading (e.g. `# Authentication Spec`) — group related content into ONE document (for example, all ADRs belong in a single `# Architecture Decision Records` document with each ADR as an `## ADR-001: ...` section, NOT as separate documents). MCP handles section-level indexing internally, so do not pre-split content yourself. To append a new section to an existing document (e.g. adding ADR-003) without rewriting the whole body, pass `append_body` with just the new section instead of `body` — `body` and `append_body` are mutually exclusive, and `append_body` requires an existing `doc_id` (there is nothing to append to when creating a new document). The body (or, for append_body, the resulting combined body) is stored verbatim at _doc.<slug>.md and split in-memory into a `sections` byte-offset index (no per-section files), syncing the bidirectional task<->doc link when task_ids is given. Omit doc_id to create a new document (slug is then required and must be unique); pass an existing doc_id to update it (slug is taken from the existing document — it cannot be renamed via doc_save). Returns a JSON string {doc_id,slug,title,doc_type,section_count,content_hash,warnings:[…]} — warnings lists any task_ids that could not be resolved, and includes a soft notice when the saved body does not start with a level-1 heading (the save is never rejected for this).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Existing document id to update. Omit to create a new document. Required when append_body is given." },
                    "slug": { "type": "string", "description": "Human-readable file-naming slug ([a-z0-9-], max 60 chars), used to name _doc.<slug>.json/.md. Required when creating; ignored on update (the existing document's slug is kept)." },
                    "title": { "type": "string", "description": "Document title. Required when creating; optional on update or append (defaults to the existing title)." },
                    "body": { "type": "string", "description": "Full Markdown document, starting with a level-1 heading. Mutually exclusive with append_body; one of the two is required." },
                    "append_body": { "type": "string", "description": "New section(s) to append to an existing document's body (e.g. `## ADR-003: ...`). Joined onto the existing body with `separator` before the usual split/save. Requires doc_id. Mutually exclusive with body; one of the two is required. Use the same line-ending style as the existing document." },
                    "separator": { "type": "string", "description": "append_body only: text inserted between the existing body and append_body.", "default": "\n\n" },
                    "doc_type": { "type": "string", "description": "Document type.", "enum": ["spec", "design", "adr", "guide", "note"], "default": "note" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags for filtering/search." },
                    "scope_paths": { "type": "array", "items": { "type": "string" }, "description": "Path prefixes this document applies to; boosts relevance in doc_list(query=...) when a file path matches." },
                    "parent_id": { "type": "string", "description": "Parent document id (family tree)." },
                    "task_ids": { "type": "array", "items": { "type": "string" }, "description": "Task ids to link bidirectionally. On update, ids removed from this list are unlinked; ids added are linked." },
                    "related": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" }, "rel": { "type": "string", "enum": ["supersedes", "references", "implements", "extends", "conflicts"] } }, "required": ["id", "rel"] }, "description": "Sibling/relative relationships to other documents." },
                    "split_level": { "type": "integer", "description": "ATX heading level at/above which the body is split into sections.", "default": 2 },
                    "auto_inject": { "type": "string", "description": "Auto-injection control.", "enum": ["auto", "full", "outline", "none"], "default": "auto" }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_doc_update_section".to_string(),
            description: "Replace a single section of a document by its seq number, without re-sending the whole document body. new_content is the replacement Markdown (including the section's own heading line); an empty string deletes the section. Sections are computed on-demand from the current body (same as doc_get/doc_save), so seq must match the document's current section numbering. expected_hash is an optional optimistic lock: when given, it must match the section's current content_hash or the call errors with the actual current hash (for retry) and makes no change. Updates the document's updated_at and content_hash, and — if a verification matrix item exists at this fragment_seq — surfaces a warning that it is now stale (its content_hash_at_verify no longer matches). Returns a JSON string {doc_id,seq,heading,content_hash,updated_at,section_count,warnings?:[…]}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to update." },
                    "seq": { "type": "integer", "description": "Section sequence number to replace (0 = preamble)." },
                    "new_content": { "type": "string", "description": "Replacement Markdown text for this section, including its own heading line. An empty string deletes the section." },
                    "expected_hash": { "type": "string", "description": "Optimistic lock: the section's expected current content_hash. If it does not match, the update is rejected with the actual current hash." }
                },
                "required": ["doc_id", "seq", "new_content"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_get".to_string(),
            description: "Read a document by doc_id or slug. format='full' returns the original Markdown body (read directly from _doc.<slug>.md) plus metadata; 'meta' returns metadata only (no body, cheap for graph traversal); 'section' returns one section's body (byte-sliced from the document body, requires seq). Returns a JSON string.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to read." },
                    "format": { "type": "string", "description": "Read mode.", "enum": ["full", "meta", "section"], "default": "full" },
                    "seq": { "type": "integer", "description": "Section sequence number. Required when format='section'." }
                },
                "required": ["doc_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_list".to_string(),
            description: "List/search documents. Filters (doc_type, tags [AND — every tag must be present], task_id) are applied first; an optional query BM25-ranks the survivors by title + tags + body text. include_body includes each matching document's full body, read from _doc.<slug>.md (default false — metadata only). Returns a JSON string {documents:[…]}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_type": { "type": "string", "description": "Filter by document type." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Filter: document must have every listed tag (AND)." },
                    "task_id": { "type": "string", "description": "Filter: only documents linked to this task." },
                    "include_body": { "type": "boolean", "description": "Include each document's full body.", "default": false },
                    "query": { "type": "string", "description": "BM25 text search over title + tags + body." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_doc_delete".to_string(),
            description: "Delete a document (by doc_id or slug) and its body file. Unlinks the document from any linked tasks' task_links, removes it from its parent's children list, and clears parent_id on any of its own children (orphaning them — delete does not cascade to descendants). Returns a JSON string {deleted,doc_id,section_count,warnings:[…]}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to delete." }
                },
                "required": ["doc_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_reassemble".to_string(),
            description: "Read a document's (by doc_id or slug) original Markdown body directly from _doc.<slug>.md, restoring BOM/frontmatter, and detect drift (the body's current content hash no longer matches its recorded content_hash — e.g. edited directly outside doc_save). Optionally writes the body to output_path. Returns a JSON string {doc_id,body,drifted,output_path?}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to reassemble." },
                    "output_path": { "type": "string", "description": "Optional filesystem path to write the reassembled body to." }
                },
                "required": ["doc_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_tree".to_string(),
            description: "Traverse a document's family tree (parent/children) starting from doc_id (id or slug), up to depth levels of descendants, plus the immediate parent (if any). include_related additionally attaches the document's related (semantic) links. Returns a JSON string tree {id,title,doc_type,parent,children:[…],related:[…]}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Root document id or slug to traverse from." },
                    "depth": { "type": "integer", "description": "How many levels of children to descend.", "default": 2 },
                    "include_related": { "type": "boolean", "description": "Also include the root document's `related` entries.", "default": false }
                },
                "required": ["doc_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_verify".to_string(),
            description: "Operate on a document's verification matrix (wiki/140-verification-matrix.md): generate (create a matrix from the document's current sections, error if one already exists), check (mark fragment_seq — or its sub_item_index — verified, recording verified_at/reviewer/notes/content_hash_at_verify — fragment_seq may be a single section seq or an array of seqs to verify in one call), check_all (verify every section and sub_item in the matrix in one call, error if no matrix exists yet), skip (mark fragment_seq — or its sub_item_index — skipped), sync (re-sync the matrix with the document's current sections — adds new sections as pending, removes deleted ones, preserves existing item status), set_refs (update impl_refs/test_refs for fragment_seq), add_item (v2, spec §7.2: with fragment_seq given, append a SubItem — description required — to that section's sub_items; with fragment_seq omitted, append a new freeform top-level item not tied to any section — label required), or suggest_refs (t124.6: read-only — scans the document's scope_paths for source/test files whose fn/struct/impl/mod definitions or test functions fuzzy-match each verification item's heading, and returns candidate impl_refs/test_refs per item for the caller to review and apply via set_refs; errors if no matrix exists yet). Overall verification_status is recomputed after every mutation: 'pending' if all items pending, 'verified' if all verified/skipped, else 'in_review'. Returns a JSON string {doc_id,verification_status,checked,skipped,pending,total,stale} for mutating actions, or {doc_id,suggestions:[{fragment_seq,heading,suggested_impl_refs,suggested_test_refs}]} for suggest_refs.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to operate on." },
                    "action": { "type": "string", "description": "Verification matrix action.", "enum": ["generate", "check", "check_all", "skip", "sync", "set_refs", "add_item", "suggest_refs"] },
                    "skip_seqs": { "type": "array", "items": { "type": "integer" }, "description": "generate only: section seqs to create as 'skipped' instead of 'pending'." },
                    "fragment_seq": { "description": "skip/set_refs: the section seq (VerificationItem.fragment_seq) to operate on. check: a single section seq, or an array of section seqs to verify in one call. add_item: the section seq to attach a new sub_item to; omit to add a freeform top-level item instead.", "oneOf": [ { "type": "integer" }, { "type": "array", "items": { "type": "integer" } } ] },
                    "sub_item_index": { "type": "integer", "description": "check/skip: the 0-based SubItem.index within fragment_seq's sub_items to operate on, instead of the parent item itself." },
                    "description": { "type": "string", "description": "add_item (fragment_seq given): the new sub_item's description. Required in this form." },
                    "label": { "type": "string", "description": "add_item (fragment_seq omitted): the new freeform top-level item's label. Required in this form." },
                    "category": { "type": "string", "description": "add_item: item/sub_item category (free-extensible, e.g. 'requirement', 'visual', 'regression', 'manual'). Defaults to 'requirement' for sub_items, 'visual' for freeform items." },
                    "reviewer": { "type": "string", "description": "check/check_all: who verified it.", "enum": ["ai", "user"] },
                    "notes": { "type": "string", "description": "check/check_all: optional free-text note." },
                    "impl_refs": { "type": "array", "items": { "type": "object", "properties": { "path": { "type": "string" }, "lines": { "type": "string" }, "label": { "type": "string" } }, "required": ["path"] }, "description": "set_refs: implementation code references to attach to fragment_seq." },
                    "test_refs": { "type": "array", "items": { "type": "object", "properties": { "path": { "type": "string" }, "lines": { "type": "string" }, "label": { "type": "string" } }, "required": ["path"] }, "description": "set_refs: test code references to attach to fragment_seq." }
                },
                "required": ["doc_id", "action"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_verify_status".to_string(),
            description: "Get a document's verification matrix status: overall verification_status, progress counts (checked/skipped/pending/total/stale/percentage — v2: counts leaf items, i.e. sub_items and freeform items, spec §7.4), and (when include_items=true) every item with a computed stale flag (its content_hash_at_verify no longer matches the section's current content_hash — spec §3.5). Errors if the document has no verification matrix yet (use handoff_doc_verify(action='generate') first). Returns a JSON string {doc_id,title,verification_status,progress:{…},items?:[…]} by default, or (format='checklist') a Markdown checklist rendering (spec §7.3) instead.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to read verification status for." },
                    "include_items": { "type": "boolean", "description": "Include the full per-item list (with stale detection).", "default": false },
                    "format": { "type": "string", "description": "Output format: 'json' (default, structured payload) or 'checklist' (Markdown checklist rendering with headings, sub_item checkboxes, refs, and categories).", "enum": ["json", "checklist"], "default": "json" }
                },
                "required": ["doc_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_graph".to_string(),
            description: "Build a graph of every document in the project: nodes (one per document, with id/slug/title/doc_type/tags/task_ids/section_count/updated_at, plus verification_progress {total,verified} when include_verification=true and a matrix exists), edges (explicit parent_id -> type='parent_child'/direction='down', explicit related[] -> type=<rel>/direction='forward', and — when include_implicit=true — implicit shared_task edges for documents sharing task_ids and shared_scope edges for documents sharing scope_paths), and layers (doc ids grouped by doc_type). Intended for graph-visualization UIs. Returns a JSON string {nodes:[…],edges:[…],layers:{…}}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "include_implicit": { "type": "boolean", "description": "Also emit shared_task/shared_scope implicit edges.", "default": true },
                    "include_verification": { "type": "boolean", "description": "Attach verification_progress {total,verified} to each node that has a verification matrix.", "default": false }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_doc_trace".to_string(),
            description: "Trace a document's family-tree lineage from doc_id (id or slug): direction='up' walks the child->parent chain to the root; 'down' walks parent->children (DFS); 'both' (default) merges the up chain, the target doc, and the down chain into one ordered chain (root to leaf). related (implements/references/etc.) documents encountered along the chain are appended as detour entries. Multi-child forks encountered in the down direction are additionally reported in branches[] (one entry per fork, {fork_from,docs:[…]}). Cycle-safe: a visited set skips any document already seen in the traversal. Returns a JSON string {chain:[{id,title,doc_type,rel}…],branches:[{fork_from,docs:[…]}…]}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to trace from." },
                    "direction": { "type": "string", "description": "Traversal direction.", "enum": ["up", "down", "both"], "default": "both" }
                },
                "required": ["doc_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_query".to_string(),
            description: "Inject document sections relevant to the current prompt/file/task (hook-driven context injection, mirrors memory_query at section granularity). Ranks by BM25 relevance + scope_paths match + task_id affinity, then stages each result as 'full' (whole section body, when its token estimate is within the inline threshold) or 'outline' (heading + sibling table of contents only, for larger sections — fetch the body via doc_get(format='section')). With session_id, already-injected sections (same content_hash) are skipped this session; mark_injected (default true) records survivors. suppress_doc_ids excludes given documents from this call's results; combined with suppress_until_changed=true (requires session_id), the suppression is recorded in the session's injected sidecar and persists across future calls until that document's content_hash changes. Returns a JSON string {documents:[…],injected_count}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "text": { "type": "string", "description": "Prompt/query text to rank sections against." },
                    "file_paths": { "type": "array", "items": { "type": "string" }, "description": "File paths in play; boosts documents whose scope_paths match." },
                    "task_id": { "type": "string", "description": "Boost sections belonging to documents linked to this task (highest-weight ranking signal)." },
                    "session_id": { "type": "string", "description": "Session id for per-session diff injection (skips sections already injected at their current content_hash)." },
                    "limit": { "type": "integer", "description": "Max number of sections to return.", "default": 5 },
                    "mark_injected": { "type": "boolean", "description": "Record returned sections in the session's injected sidecar.", "default": true },
                    "suppress_doc_ids": { "type": "array", "items": { "type": "string" }, "description": "Document ids to exclude entirely from this call's results." },
                    "suppress_until_changed": { "type": "boolean", "description": "With suppress_doc_ids and session_id: persist the suppression in the session's injected sidecar so those documents stay excluded from future doc_query calls until their content_hash changes.", "default": false }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_doc_analyze".to_string(),
            description: "Read-only scan of a Markdown file or directory (never writes). Auto-detects doc_type (keyword scan), tags (frontmatter + heading tokens), scope_paths (code/inline file paths), and a suggested_slug (derived from title) per file; extracts and classifies Markdown links (internal/external/broken); proposes a parent/children tree from directory structure (skip with flatten=true). Returns a JSON conditioning report {files_scanned,auto_resolved:[…],needs_review:[…],proposed_tree:{…}} for AI review before handoff_doc_import.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "path": { "type": "string", "description": "File or directory path (relative to project_dir) to scan." },
                    "recursive": { "type": "boolean", "description": "Recurse into subdirectories when path is a directory.", "default": true },
                    "flatten": { "type": "boolean", "description": "Skip parent/children tree inference; every file is a standalone document.", "default": false }
                },
                "required": ["path"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_import".to_string(),
            description: "Bulk-import pre-existing Markdown files. Each file becomes ONE document in .handoff/docs/ — the file's content is stored as-is, including its h1 heading. Do NOT split a single source file into multiple documents; MCP indexes sections internally. Writes an analyzed payload (from handoff_doc_analyze, with the AI's overrides applied) as new documents. Each analyzed.auto_resolved entry must carry its file's full Markdown 'body' (doc_import writes from the payload, it does not re-read the filesystem). Each document's slug is taken from its override's 'slug' if given, else its suggested_slug, disambiguated with a numeric suffix on collision. Persists every file as a document, applies proposed_tree parent/children relationships, links task_ids to every imported document (bidirectionally), and invalidates the doc corpus cache. Returns a JSON string {imported_count,documents:[{doc_id,slug,title,section_count}],warnings:[…]}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "analyzed": { "type": "object", "description": "The handoff_doc_analyze report, with each auto_resolved entry additionally carrying its file's 'body'." },
                    "overrides": { "type": "array", "items": { "type": "object", "properties": { "file": { "type": "string" }, "slug": { "type": "string" }, "title": { "type": "string" }, "doc_type": { "type": "string" }, "tags": { "type": "array", "items": { "type": "string" } }, "scope_paths": { "type": "array", "items": { "type": "string" } } }, "required": ["file"] }, "description": "Per-file AI overrides applied on top of analyzed.auto_resolved before writing." },
                    "task_ids": { "type": "array", "items": { "type": "string" }, "description": "Link every imported document to these tasks (bidirectionally)." }
                },
                "required": ["analyzed"]
            }),
        },
        ToolDefinition {
            name: "handoff_task_checklist".to_string(),
            description: "action=\"view\" (default): pure-view aggregation of a task's done_criteria and its linked documents' verification matrices. No new data is written — reads task_links (link_type=\"doc\") and each linked document's verification matrix, computed fresh on every call. Returns {task_id,title,no_linked_docs:true} as a fast-path response when the task has no linked documents. Otherwise returns {task_id,title,no_linked_docs:false,done_criteria:{items:[…],progress:{…}},verification_coverage:{documents:[{doc_id,slug,title,doc_type,items:[{fragment_seq,heading,status,stale,visual_state,impl_refs,test_refs}],progress:{…}}],overall:{…}},combined_readiness:{done_criteria_met,verification_complete,ready,blockers:[{type:\"criteria\"|\"verification\",…}]},suggested_actions:[…]}. Each item's visual_state is computed in priority order stale > skipped > verified > implemented (pending+impl_refs+test_refs) > in_progress (pending+impl_refs only) > untouched. action=\"generate\": turns a linked spec/design document's level-2 section headings into done_criteria items using hardcoded defaults (no config template) — format '[{doc_type}§{seq}] {heading}', seq=0 (preamble) plus any skip_seqs excluded. doc_id defaults to the first linked document with doc_type 'spec' or 'design' when omitted. mode=\"preview\" (default) returns the generated items without writing; \"append\" adds them to the task's existing done_criteria; \"replace\" overwrites done_criteria entirely — both writes go through the same optimistic-concurrency path as handoff_check_criterion. Returns {task_id,generated_criteria:[{item,fragment_seq}],applied,skipped_seqs,fixed_items}, where fixed_items is a doc_type-specific list of non-section checklist items (spec: 2 items; design: 1 item; other doc_types: []).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "task_id": { "type": "string", "description": "Task ID to build the checklist for (e.g. 't1', 't1.2')." },
                    "action": { "type": "string", "description": "Checklist action.", "enum": ["view", "generate"], "default": "view" },
                    "doc_id": { "type": "string", "description": "generate only: document id or slug to generate from. Defaults to the first task-linked document with doc_type 'spec' or 'design'." },
                    "mode": { "type": "string", "description": "generate only: 'preview' returns items without writing, 'append' adds to existing done_criteria, 'replace' overwrites done_criteria entirely.", "enum": ["preview", "append", "replace"], "default": "preview" },
                    "skip_seqs": { "type": "array", "items": { "type": "integer" }, "description": "generate only: additional section seqs to exclude, beyond the always-skipped seq=0 preamble." }
                },
                "required": ["task_id"]
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

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
                    "layer": {
                        "type": "string",
                        "description": "wiki/260-vmodel-m2-design.md §4.11 (M2-13): filter to tasks with at least one requirement-type task_links entry whose linked stable_id's effective layer (SubItem.layer, falling back to the owning document's layer) equals this value (e.g. \"requirement\", \"acceptance\"). Reads the document corpus once to resolve stable_id -> layer only when this filter is given."
                    },
                    "role": {
                        "type": "string",
                        "enum": ["implements", "executes"],
                        "description": "wiki/260-vmodel-m2-design.md §4.11 (M2-13): filter to tasks with at least one requirement-type task_links entry whose role (defaulting to \"implements\" when unset) equals this value."
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
            description: "Get full task details (notes, done_criteria, labels, links) by task ID. Use when list_tasks summary is not enough. Response includes trace: {layers, blockers} (wiki/260-vmodel-m2-design.md §3.4/§4.11, M2-13) when the task has at least one requirement-type task_links entry — layers is [{layer, role, count}] and blockers tallies not_run/failing/blocked/reverify/suspect among the linked items' direct verifiers (implements role) or the linked item's own result (executes role); null when the task has no requirement link. Read-only (E6): never writes runs/_latest.json or any document.".to_string(),
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
                    },
                    "include_dependents": {
                        "type": "boolean",
                        "description": "If true, scan the whole project and populate the response's 'dependents' field: tasks elsewhere that list this task in their own dependencies (each with its own title/status/notes/done_criteria). Check these before flagging a piece of this task as unwired, since a later dependent task may be the one that wires it. Default false, in which case 'dependents' is null (the scan does not run) — this scans every task file in the project, so only set it when you actually need to check for deferred wiring."
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
            description: "Add, update, or move a task. Manages the tasks/ directory structure. Include task.schedule.estimate_hours (raw human-effort hours, > 0) when moving a leaf task to in_progress/review/done; it is rejected without one unless the task is a parent, todo, blocked, or skipped. Done guard (wiki/260-vmodel-m2-design.md §3.4/§4.11, M2-13, [trace] done_guard in config.toml, default \"warn\"): when task.status is set to \"review\" or \"done\" and this task has at least one requirement-type task_links entry with a blocker (not_run/failing/blocked/reverify verifier, or a stale task-link suspect), \"warn\" lists them in the response's warnings (the transition still applies), \"block\" rejects the whole call unless force=true, and \"off\" does nothing. A task with no requirement links is never affected.".to_string(),
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
                            },
                            "scope_paths": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "File paths this task affects. Used for advisory conflict detection when another task claims overlapping scope."
                            },
                            "requirement_ids": {
                                "type": "array",
                                "items": { "type": "string" },
                                "description": "Stable IDs of requirement sub-items to link. On an existing task, this REPLACES the set: stable_ids newly present are added, previously-linked ones now absent are removed (bidirectional: SubItem.task_ids <-> Task.task_links). A stable_id being removed that no longer resolves to any SubItem (its item was deleted) still has its Task.task_links entry removed. Unresolved/ambiguous stable_ids on the add side are returned as warnings and not linked."
                            },
                            "requirement_roles": {
                                "type": "object",
                                "additionalProperties": { "type": "string", "enum": ["implements", "executes"] },
                                "description": "wiki/220-vmodel-integration-design.md §2.5 (M1 t360.7): {stable_id: \"implements\"|\"executes\"} — this task's relationship to a stable_id being linked via requirement_ids. Omitted stable_ids get their role inferred from the SubItem's effective-layer side (right, e.g. system_test/unit_test -> \"executes\"; left or no layer -> \"implements\"). A stable_id whose link membership is unchanged but whose requested role differs from its current one is updated in place. Only \"implements\" links (the default) propagate this task's status changes to the linked requirement's dev_stage — \"executes\" links (a test-execution task) never do."
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
                    },
                    "force": {
                        "type": "boolean",
                        "description": "wiki/260-vmodel-m2-design.md §3.4 (M2-13): when [trace] done_guard = \"block\" and this call moves task.status to \"review\"/\"done\" with an outstanding blocker, the call is rejected unless force=true. Has no effect under done_guard \"warn\"/\"off\", or when the task has no blockers. Default: false.",
                        "default": false
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
            description: "Run auto-scheduler to compute optimal task dates based on dependencies, estimates, and calendar capacity. Returns change diff; applies changes unless dry_run=true. Also reports agent_capacity (registered agents' claimed task count vs. max_concurrent_wts) and ready_tasks (dependency-resolved todo tasks sorted by priority) to support next-task auto-assignment.".to_string(),
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
            description: "Save a complete document. The body MUST be a whole, human-readable Markdown document starting with a level-1 heading (e.g. `# Authentication Spec`) — group related content into ONE document (for example, all ADRs belong in a single `# Architecture Decision Records` document with each ADR as an `## ADR-001: ...` section, NOT as separate documents). MCP handles section-level indexing internally, so do not pre-split content yourself. To append a new section to an existing document (e.g. adding ADR-003) without rewriting the whole body, pass `append_body` with just the new section instead of `body` — `body` and `append_body` are mutually exclusive, and `append_body` requires an existing `doc_id` (there is nothing to append to when creating a new document). To update only metadata (e.g. `task_ids`, `tags`, `auto_inject`) on an existing document without touching its content, pass `doc_id` and omit both `body` and `append_body` — the existing body is kept verbatim (sections/content_hash are recomputed from it, but nothing is rewritten to disk). New documents (no `doc_id`) always require `body`. The body (or, for append_body, the resulting combined body) is stored verbatim at _doc.<slug>.md and split in-memory into a `sections` byte-offset index (no per-section files), syncing the bidirectional task<->doc link when task_ids is given. Omit doc_id to create a new document (slug is then required and must be unique); pass an existing doc_id to update it (slug is taken from the existing document — it cannot be renamed via doc_save). Returns a JSON string {doc_id,slug,title,doc_type,section_count,content_hash,warnings:[…]} — warnings lists any task_ids that could not be resolved, and includes a soft notice when the saved body does not start with a level-1 heading (the save is never rejected for this).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Existing document id to update. Omit to create a new document. Required when append_body is given." },
                    "slug": { "type": "string", "description": "Human-readable file-naming slug ([a-z0-9-], max 60 chars), used to name _doc.<slug>.json/.md. Required and unique when creating; ignored on update (the existing document's slug is kept).", "pattern": "^[a-z0-9-]+$", "minLength": 1, "maxLength": 60 },
                    "title": { "type": "string", "description": "Document title. Required when creating; optional on update or append (defaults to the existing title)." },
                    "body": { "type": "string", "description": "Full Markdown document, starting with a level-1 heading. Mutually exclusive with append_body. Required for new documents; omit for metadata-only updates on existing documents." },
                    "append_body": { "type": "string", "description": "New section(s) to append to an existing document's body (e.g. `## ADR-003: ...`). Joined onto the existing body with `separator` before the usual split/save. Requires doc_id. Mutually exclusive with body. Use the same line-ending style as the existing document." },
                    "separator": { "type": "string", "description": "append_body only: text inserted between the existing body and append_body.", "default": "\n\n" },
                    "doc_type": { "type": "string", "description": "Document type.", "enum": ["spec", "design", "adr", "guide", "note"], "default": "note" },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Tags for filtering/search." },
                    "scope_paths": { "type": "array", "items": { "type": "string" }, "description": "Path prefixes this document applies to; boosts relevance in doc_list(query=...) when a file path matches." },
                    "parent_id": { "type": "string", "description": "Parent document id (family tree)." },
                    "task_ids": { "type": "array", "items": { "type": "string" }, "description": "Task ids to link bidirectionally. On update, ids removed from this list are unlinked; ids added are linked. The document's resulting task_ids is derived from the task-side link just written (wiki/260-vmodel-m2-design.md §4.8): an id that fails to resolve to an existing task is reported as a warning and omitted, not echoed back." },
                    "related": { "type": "array", "items": { "type": "object", "properties": { "id": { "type": "string" }, "rel": { "type": "string", "enum": ["supersedes", "references", "implements", "extends", "conflicts"] } }, "required": ["id", "rel"] }, "description": "Sibling/relative relationships to other documents." },
                    "split_level": { "type": "integer", "description": "ATX heading level at/above which the body is split into sections. On update, omitting this keeps the document's existing split_level (it does not reset to the default).", "default": 2 },
                    "auto_inject": { "type": "string", "description": "Auto-injection control.", "enum": ["auto", "full", "outline", "none"], "default": "auto" },
                    "layer": { "type": "string", "description": "V-model layer id (wiki/220-vmodel-integration-design.md §2.1): one of the 6 built-ins requirement/basic_spec/detailed_spec/acceptance/system_test/unit_test, or a project-defined id declared via [[trace.layer]] in config.toml (wiki/260-vmodel-m2-design.md §2.1) — an id the layer registry does not recognize (unknown, or an invalid custom declaration that was disabled with a warning) is treated as no layer. This is the only way to set a document's layer — omit to leave the existing value untouched, or pass an empty string \"\" to clear it. Setting this makes the document a layer document: doc_save/doc_update_section (and doc_verify sync) now parse the body's ID-prefixed headings into the verification matrix on every call — write requirement/verification items by editing the body, not via doc_verify(add_item/set_priority/set_refs with test_refs/backfill_stable_ids) or req_import, which are refused on a layer document (see the handoff-trace skill)." },
                    "trace_profile": { "type": "string", "description": "Per-document V-model profile override (wiki/260-vmodel-m2-design.md §2.1): one of the 4 built-ins minimal/standard/full/bugfix, or a key under [trace.profiles.<name>] in config.toml. Omit to leave the existing value untouched, or pass an empty string \"\" to clear it (falls back to the project default [trace] profile, then [trace] layers, then auto-detection). Only meaningful on a layer document." }
                },
                "oneOf": [
                    {
                        "title": "Create document",
                        "description": "Creating a document requires a valid, unique slug and omits doc_id.",
                        "required": ["slug"],
                        "not": { "required": ["doc_id"] }
                    },
                    {
                        "title": "Update document",
                        "description": "Updating a document requires doc_id and keeps the existing document slug.",
                        "required": ["doc_id"]
                    }
                ]
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
            description: "List/search documents. Filters (doc_type, tags [AND — every tag must be present], task_id) are applied first; an optional query BM25-ranks the survivors by title + tags + body text. include_body includes each matching document's full body, read from _doc.<slug>.md (default false — metadata only). unreadable lists any _doc.<slug>.md whose frontmatter failed to parse (FR-804: reported instead of silently dropped) as {slug,error,line?} — use handoff_doc_repair_frontmatter to fix known non-standard shapes. Returns a JSON string {documents:[…],unreadable:[…]}.".to_string(),
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
            description: "Operate on a document's verification matrix (wiki/140-verification-matrix.md): generate (create a matrix from the document's current sections, error if one already exists), check (mark fragment_seq — or its sub_item_id/sub_item_index — verified, recording verified_at/reviewer/notes/content_hash_at_verify — fragment_seq may be a single section seq or an array of seqs to verify in one call), check_all (verify every section and sub_item in the matrix in one call, error if no matrix exists yet), skip (mark fragment_seq — or its sub_item_id/sub_item_index — skipped), sync (re-sync the matrix with the document's current sections — adds new sections as pending, removes deleted ones, preserves existing item status), set_refs (update impl_refs/test_refs for fragment_seq, or — with sub_item_id/sub_item_index given — for that SubItem instead of the parent item; requirements-traceability P0 §3.3), set_dev_stage (requirements-traceability P0 §2.4/§3.3: set a SubItem's dev_stage — sub_item_id or sub_item_index required, since dev_stage is a SubItem-only field; value must be one of not_started/in_progress/implemented/tested/verified), set_priority (P0 §3.3: set a SubItem's priority — sub_item_id or sub_item_index required; value must be one of P0/P1/P2/P3), add_item (v2, spec §7.2: with fragment_seq given, append a SubItem — description required — to that section's sub_items; with fragment_seq omitted, append a new freeform top-level item not tied to any section — label required), backfill_stable_ids (requirements-traceability integration-reform §3.3: one-shot bulk backfill — scans every SubItem across the whole matrix and mints a stable_id, via the same derivation `add_item` uses, for every one that doesn't have one yet; SubItems that already have a stable_id are left untouched; collisions against already-assigned or already-backfilled ids are disambiguated with a numeric suffix, same as add_item; doc_id is the only input), or suggest_refs (t124.6: read-only — scans the document's scope_paths for source/test files whose fn/struct/impl/mod definitions or test functions fuzzy-match each verification item's heading, and returns candidate impl_refs/test_refs per item for the caller to review and apply via set_refs; errors if no matrix exists yet). To link a task to a requirement SubItem, use handoff_update_task(task.requirement_ids=[...]) instead (handoff_doc_verify(action=\"link_task\") was removed at the M3 release, wiki/270-vmodel-m3-design.md §4.8). For every SubItem-addressing action (check, skip, set_refs, set_dev_stage, set_priority), sub_item_id (the stable, immutable SubItem.stable_id) takes precedence over sub_item_index (positional, back-compat fallback) when both are given; a mismatch between them is reported as a non-fatal warning in the response, not an error. fragment_seq is optional when sub_item_id is given (FR-806, wiki/220 §4.1): the SubItem is then located by stable_id across every VerificationItem in the matrix, including freeform ones (fragment_seq: None, e.g. from add_item with no fragment_seq, or from handoff_doc_req_import). fragment_seq is still required when addressing by sub_item_index (which is only unique within one VerificationItem's sub_items) or when the action targets a section item directly (no sub_item_id/sub_item_index given at all). Overall verification_status is recomputed after every mutation: 'pending' if all items pending, 'verified' if all verified/skipped, else 'in_review'. `_requirements_summary.json` (the VSCode-extension-facing aggregate cache) is refreshed after every mutating action, including add_item and backfill_stable_ids. M1 layer document write guard (wiki/220-vmodel-integration-design.md §2.3): on a document with layer set, add_item, set_priority, backfill_stable_ids, and set_refs (only when the call includes test_refs — impl_refs-only is still allowed) are refused with an error telling the caller to edit the body instead, since those fields (description/layer/refines/verifies/method/priority/test_refs) are defined by the Markdown body and would just be overwritten or orphaned by the next sync; set_dev_stage, check, check_all, skip, sync (which delegates to the layer-body sync instead of the plain per-section rebuild), generate, and suggest_refs remain fully allowed on a layer document. Returns a JSON string {doc_id,verification_status,checked,skipped,pending,total,stale,warnings} for mutating actions, {doc_id,backfilled,verification_status,checked,skipped,pending,total,stale,warnings} for backfill_stable_ids, or {doc_id,suggestions:[{fragment_seq,heading,suggested_impl_refs,suggested_test_refs}]} for suggest_refs.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to operate on." },
                    "action": { "type": "string", "description": "Verification matrix action.", "enum": ["generate", "check", "check_all", "skip", "sync", "set_refs", "set_dev_stage", "set_priority", "add_item", "backfill_stable_ids", "suggest_refs"] },
                    "skip_seqs": { "type": "array", "items": { "type": "integer" }, "description": "generate only: section seqs to create as 'skipped' instead of 'pending'." },
                    "fragment_seq": { "description": "check/skip/set_refs/set_dev_stage/set_priority: the section seq (VerificationItem.fragment_seq) to operate on when addressing a section item directly, or when addressing a SubItem via sub_item_index (fragment_seq is required in both of those cases). May be omitted when sub_item_id is given instead (FR-806, wiki/220 §4.1) — the SubItem is then located by stable_id across every VerificationItem in the matrix, including freeform ones. check: a single section seq, or an array of section seqs to verify in one call. add_item: the section seq to attach a new sub_item to; omit to add a freeform top-level item instead.", "oneOf": [ { "type": "integer" }, { "type": "array", "items": { "type": "integer" } } ] },
                    "sub_item_id": { "type": "string", "description": "check/skip/set_refs/set_dev_stage/set_priority: the SubItem's stable, immutable stable_id (requirements-traceability P0 §2.5, e.g. \"C01-2.1.1.1\") to address, instead of the parent item itself. When given, fragment_seq may be omitted (FR-806, wiki/220 §4.1): the SubItem is found by stable_id across every VerificationItem in the matrix, including freeform ones (fragment_seq: None). Preferred over sub_item_index; if both are given and disagree, sub_item_id wins and a warning is returned." },
                    "sub_item_index": { "type": "integer", "description": "check/skip/set_refs/set_dev_stage/set_priority: the 0-based SubItem.index within fragment_seq's sub_items to operate on, instead of the parent item itself. Positional fallback used when sub_item_id is omitted or unavailable." },
                    "dev_stage": { "type": "string", "description": "set_dev_stage: the SubItem's implementation-progress stage (requirements-traceability P0 §2.4, distinct from the verification review 'status' field).", "enum": ["not_started", "in_progress", "implemented", "tested", "verified"] },
                    "priority": { "type": "string", "description": "set_priority: the SubItem's requirement priority.", "enum": ["P0", "P1", "P2", "P3"] },
                    "description": { "type": "string", "description": "add_item (fragment_seq given): the new sub_item's description. Required in this form." },
                    "label": { "type": "string", "description": "add_item (fragment_seq omitted): the new freeform top-level item's label. Required in this form." },
                    "category": { "type": "string", "description": "add_item: item/sub_item category (free-extensible, e.g. 'requirement', 'visual', 'regression', 'manual'). Defaults to 'requirement' for sub_items, 'visual' for freeform items." },
                    "reviewer": { "type": "string", "description": "check/check_all: who verified it.", "enum": ["ai", "user"] },
                    "notes": { "type": "string", "description": "check/check_all: optional free-text note." },
                    "impl_refs": { "type": "array", "items": { "type": "object", "properties": { "path": { "type": "string" }, "lines": { "type": "string" }, "label": { "type": "string" } }, "required": ["path"] }, "description": "set_refs: implementation code references to attach to fragment_seq, or to its addressed sub_item when sub_item_id/sub_item_index is given." },
                    "test_refs": { "type": "array", "items": { "type": "object", "properties": { "path": { "type": "string" }, "lines": { "type": "string" }, "label": { "type": "string" } }, "required": ["path"] }, "description": "set_refs: test code references to attach to fragment_seq, or to its addressed sub_item when sub_item_id/sub_item_index is given." }
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
            name: "handoff_doc_repair_task_ids".to_string(),
            description: "Explicit repair: forces a full, all-tasks-scanning rebuild of every requirement SubItem's task_ids from TaskData.task_links (the source of truth) across the whole document corpus, correcting any drift (manual edits, imported data, bugs). Also repairs each document's own task_ids (wiki/260-vmodel-m2-design.md §4.8/M2-15) from TaskLink{link_type:'doc'} entries, but append-only: an id already in a document's task_ids with no matching TaskLink{doc} is left in place rather than removed (handoff_trace_lint's task_ids_drift rule reports it instead) — only an explicit handoff_doc_save(task_ids=...) call ever removes one. Every live link-change path (handoff_update_task requirement_ids, handoff_doc_verify link_task, handoff_doc_save task_ids) and layer sync already keep task_ids in sync differentially as they run — this tool is only for when state has drifted anyway. Unlike handoff_trace_report's own self-repair pass (which is gated on the tasks_* input fingerprint, wiki/220-vmodel-integration-design.md §2.5/§4.3, and skips the rescan as a cheap no-op when nothing has changed since the last full-corpus sync), this tool always runs its rescan unconditionally with no gate — a caller reaching for it has already decided a repair is needed. Returns a JSON string {ran,sub_items_changed,docs_changed,doc_task_ids_appended} (ran is always true here).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_doc_repair_frontmatter".to_string(),
            description: "Reports and (optionally) fixes documents whose frontmatter fails to parse (FR-804, E11, wiki/260-vmodel-m2-design.md §4.12/§12 M2-18) — the same set handoff_doc_list's unreadable field reports. Only recognizes specific known non-standard shapes (a bare 'key:' line followed by its flow-style value, e.g. `[]`, on its own line; tab indentation) — an unrecognized malformation is reported in unrepaired rather than guessed at. dry_run (default true) reports what would change without writing anything; dry_run=false rewrites the document through the same canonical writer every doc_save-style mutation uses, so it re-enters handoff_doc_list's normal results afterward. slug narrows the scan to one document. Returns a JSON string {dry_run,repaired:[{slug,applied,fixes:[…]}],unrepaired:[{slug,error}]}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "slug": { "type": "string", "description": "Only attempt repair on this document's slug; omit to scan every unreadable document." },
                    "dry_run": { "type": "boolean", "description": "Report what would change without writing anything.", "default": true }
                }
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
            name: "handoff_doc_req_list".to_string(),
            description: "List individual requirements (SubItems with a stable_id) across every document's verification matrix, with filtering, sorting, and pagination (requirements-traceability P1 §4.2; task_id filter added by the integration-reform §3.1). SubItems without a stable_id yet are excluded. Filters: priority (SubItem.priority exact match), dev_stage (SubItem.dev_stage exact match; a SubItem with no dev_stage set counts as 'not_started'), category (stable_id's 'C{n}' prefix, e.g. 'C07'), has_tests (whether test_refs is non-empty), task_id (only SubItems whose task_ids contains this task id — set via handoff_doc_verify's link_task action; answers 'which requirements does task X implement?'). sort selects the ordering key ('stable_id' default, 'priority', 'category', 'dev_stage'; ties always break on stable_id for a stable ordering); order is 'asc' (default) or 'desc'. limit (default 100) and offset (default 0) paginate after filtering/sorting; total reflects the full filtered count before pagination. Each returned item's primary key is stable_id — sub_item_index is included only for back-compat with positional handoff_doc_verify addressing; prefer stable_id (as sub_item_id) for any follow-up handoff_doc_verify call. Returns a JSON string {items:[{stable_id,title,priority,dev_stage,verification_status,impl_refs,test_refs,doc_id,doc_slug,fragment_seq,sub_item_index,task_ids}],total,offset,limit}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "priority": { "type": "string", "description": "Filter: exact match on SubItem.priority (e.g. 'P0')." },
                    "dev_stage": { "type": "string", "description": "Filter: exact match on SubItem.dev_stage; a SubItem with no dev_stage set is treated as 'not_started'.", "enum": ["not_started", "in_progress", "implemented", "tested", "verified"] },
                    "category": { "type": "string", "description": "Filter: the 'C{n}' prefix of SubItem.stable_id (e.g. 'C07')." },
                    "has_tests": { "type": "boolean", "description": "Filter: true = only SubItems with a non-empty test_refs; false = only SubItems with an empty test_refs." },
                    "task_id": { "type": "string", "description": "Filter: only SubItems whose task_ids contains this task id (set via handoff_doc_verify's link_task action)." },
                    "sort": { "type": "string", "description": "Sort key. Ties always break on stable_id.", "enum": ["priority", "category", "dev_stage", "stable_id"], "default": "stable_id" },
                    "order": { "type": "string", "description": "Sort direction.", "enum": ["asc", "desc"], "default": "asc" },
                    "limit": { "type": "integer", "description": "Max number of items to return, after filtering/sorting.", "default": 100 },
                    "offset": { "type": "integer", "description": "Number of filtered/sorted items to skip before taking limit.", "default": 0 }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_doc_req_status".to_string(),
            description: "Cross-document requirements progress summary — aggregates every SubItem with a stable_id across every document's verification matrix into by_status (SubItem.dev_stage counts; a SubItem with no dev_stage set counts as 'not_started'), by_priority (per-priority {total,implemented,tested,verified}; a SubItem with no priority set is bucketed under 'unset'), by_category (per-'C{n}'-prefix {total,implemented,coverage_pct}, derived from stable_id), and coverage ({impl_pct,test_pct,verified_pct} across every counted SubItem) (requirements-traceability P1 §4.1). Filters, applied before aggregation: tags (only documents whose DocMetadata.tags contains at least one of the given tags), priority (only SubItems whose priority exactly matches), category (only SubItems whose stable_id's 'C{n}' prefix matches). Every call also refreshes .handoff/docs/_requirements_summary.json — a cache file the VSCode extension reads directly instead of calling MCP tools — with the FULL, unfiltered aggregate across every document, regardless of this call's filters. Returns a JSON string {total,by_status:{…},by_priority:{…},by_category:{…},coverage:{impl_pct,test_pct,verified_pct}}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "tags": { "type": "array", "items": { "type": "string" }, "description": "Filter: only include documents whose tags contain at least one of these." },
                    "priority": { "type": "string", "description": "Filter: exact match on SubItem.priority (e.g. 'P0')." },
                    "category": { "type": "string", "description": "Filter: the 'C{n}' prefix of SubItem.stable_id (e.g. 'C07')." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_doc_req_import".to_string(),
            description: "Bulk-generates SubItems (with stable_id, and optionally priority and dev_stage) from a document's Markdown requirement-tree heading hierarchy, merging against its existing verification matrix (requirements-traceability P1 §4.3-4.4, wiki/250 for the aelm-referral robustness additions below). Locates the section whose heading contains heading_pattern (default '要件ツリー'); when several headings match (or the shallowest match turns out to have no children at all — e.g. an unrelated appendix heading that only incidentally contains the pattern substring), the shallowest non-empty match wins. If no heading's text contains heading_pattern anywhere in the document, falls back (FR-802) to scanning for the first heading whose text starts with '2.' (the requirement-tree is conventionally section 2) and treats it plus its siblings as the section — this covers documents whose requirement-tree subsections are numbered but have no enclosing '## 2. 要件ツリー' wrapper heading; when this fallback fires it is reported as an informational parse_errors entry, not silently. Every deepest-level heading within the located section becomes one requirement candidate (its own heading text is the description; stable_id is derived the same way handoff_doc_verify's generate action derives one). When priority_source='gap_table' (default), also locates the section whose heading contains gap_table_pattern (default 'ギャップ分析') and parses every pipe-delimited Markdown table within it — not just the first — that has both >= 3 columns and a recognizable priority column (FR-802: a 2-column priority/status legend table, e.g. '優先度 | 定義', is skipped even though its header cell is also literally named '優先度'; a gap-analysis section split into several numbered subsections, each with its own per-item table, has every one of those tables' rows merged together). Each table's header cells are matched, per field, against column_map first when given (an explicit override — see column_map below — for a header wording no built-in synonym covers, e.g. '重要度'/'Pri.' for priority or '項目' for name), then against known synonyms (FR-802): the ID column (要件ID / ID / REQ ID), the name/description column (要件名 / 要件 / 機能, falling back to the first column that is neither the ID nor the priority column when no synonym matches), the priority column (優先度 / Priority), and — separately — an implementation-status column (実装状態 / 現状 / ステータス / status). A column_map field only applies to a table when it actually resolves against that table's own header (an index in range, or a header string matching one of its cells) — otherwise that field falls through to synonym auto-detection for that table, so one column_map still works across a document whose several gap tables aren't all shaped alike. Each candidate is matched against a table row by exact-ID-token comparison first (an ID-shaped token — digits plus '.'/'-'/'_' — extracted from the row's ID column, or from its name column when the table has no dedicated ID column, compared against the same kind of token extracted from the candidate's own heading text), falling back to fuzzy substring-containment matching against the row's name column only when no row's ID token matches; a matched row's priority cell yields P0/P1/P2/P3 (no match, or no recognized token, => priority left unset, with a warning on that preview entry) and, independently (FR-805), a dev_stage: from the dedicated status column when the table has one, or else from whatever text remains in the priority cell after stripping its matched P0-P3 token (covers merged cells like 'P0=出荷済'); status/remainder text is mapped to not_started/in_progress/implemented/verified via a fixed vocabulary (実装済・出荷済 => implemented, 部分実装 => in_progress, 検証済・verified => verified, 未実装・未定義・対象外・不要 => not_started), text this table doesn't recognize leaves dev_stage unset rather than guessing. Merge rules against the existing verification matrix: stable_id already present verbatim => action='update' (priority/description are updated from this pass; dev_stage is filled in only when the existing SubItem doesn't have one yet — a re-import never downgrades a manually-progressed dev_stage such as 'verified' back to whatever this document's gap table currently says; impl_refs/test_refs are preserved); no stable_id match but description fuzzy-matches (substring-containment after whitespace/case normalization) an existing SubItem => action='match' (that SubItem is re-linked to the newly derived stable_id, with the same dev_stage merge rule as 'update'); neither => action='create'. Existing SubItems with a stable_id that this import pass never touched are never deleted — they are reported in the top-level warnings array as orphans. Malformed heading lines and malformed/column-mismatched/unrecognized-priority table rows are reported in parse_errors rather than silently dropped, each as {line,text,reason}. dry_run (default true) returns would_create/would_update/would_skip counts plus a full preview array without writing anything. dry_run=false actually creates/updates SubItems on the document's verification matrix. If the document has no matrix yet, one is auto-generated first — one VerificationItem per document section, the same shape handoff_doc_verify's generate action produces (FR-806, wiki/220 §4.1) — and newly-created SubItems are placed in the VerificationItem for the section that contains the matched heading_pattern heading, so they are immediately addressable by stable_id (e.g. via handoff_update_task's requirement_ids, or handoff_doc_verify's sub_item_id addressing) without a separate generate/sync step. Then refreshes .handoff/docs/_requirements_summary.json. Returns a JSON string {doc_id,would_create,would_update,would_skip,parse_errors:[{line,text,reason}],preview:[{stable_id,title,priority,dev_stage,action,warning}],warnings:[...]} for dry_run=true, or {doc_id,created,updated,skipped,parse_errors,preview,warnings} for dry_run=false — dev_stage is omitted from a preview entry when this import pass would leave it unset/unchanged.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Document id or slug to import the requirement tree from." },
                    "dry_run": { "type": "boolean", "description": "When true (default), returns a preview without writing anything.", "default": true },
                    "priority_source": { "type": "string", "description": "Where to pull each requirement's priority from.", "enum": ["gap_table", "manual", "none"], "default": "gap_table" },
                    "heading_pattern": { "type": "string", "description": "Substring to match against heading text to locate the requirement-tree section.", "default": "要件ツリー" },
                    "gap_table_pattern": { "type": "string", "description": "Substring to match against heading text to locate the gap-analysis table section (used only when priority_source='gap_table').", "default": "ギャップ分析" },
                    "column_map": {
                        "type": "object",
                        "description": "Explicit gap-table column mapping (FR-802), for header wording no built-in synonym recognizes (e.g. '重要度'/'Pri.' for priority, '項目' for name). Each field is independently optional: a header string (matched against a table's own header cells) or a 0-based column index. A field given here takes priority over synonym auto-detection for that field; an omitted field, or one that doesn't resolve against a particular table's header, still falls back to auto-detection for that table.",
                        "properties": {
                            "id": { "description": "ID column: header string or 0-based index.", "oneOf": [ { "type": "string" }, { "type": "integer", "minimum": 0 } ] },
                            "name": { "description": "Name/description column: header string or 0-based index.", "oneOf": [ { "type": "string" }, { "type": "integer", "minimum": 0 } ] },
                            "priority": { "description": "Priority column: header string or 0-based index.", "oneOf": [ { "type": "string" }, { "type": "integer", "minimum": 0 } ] },
                            "status": { "description": "Implementation-status column: header string or 0-based index.", "oneOf": [ { "type": "string" }, { "type": "integer", "minimum": 0 } ] }
                        }
                    }
                },
                "required": ["doc_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_doc_req_scan".to_string(),
            description: "Scans source files under scope_paths for discoverable links to SubItem.stable_ids, returned as ranked suggestions only — never writes impl_refs/test_refs itself (requirements-traceability P2 §5.1, `.handoff/docs/_doc.req-traceability-mcp-plan.md`; follow up a confirmed suggestion with handoff_doc_verify's set_refs action). doc_id given => only that document's SubItems are scan targets, and its own scope_paths become the default scan paths when scope_paths is omitted; doc_id omitted => every document's SubItems are targets. A scope_paths entry that does not exist on disk contributes no files rather than erroring. patterns (default: all three) — 'test_name': matches a Rust `fn test_xxx(...)` name against every target stable_id converted to its expected lowercase/underscore-joined prefix (e.g. stable_id 'C01-2.1.1.1' -> prefix 'test_c01_2_1_1_1'), confidence 0.9. 'comment': matches an `Implements:`/`Requirement:`/`Req:` comment annotation (e.g. '// Implements: C07-2.3.1.1') directly against a target stable_id, confidence 0.95. 'symbol': fuzzy-matches a scanned file's stem against a target SubItem's description (substring containment after case normalization), confidence 0.6 — deliberately below the auto_linkable threshold. ref_type on each suggestion is 'test' when the file lives under a tests/test/ path segment or has a test-shaped stem, else 'impl'. auto_linkable counts suggestions with confidence > 0.8. Returns a JSON string {suggestions:[{stable_id,match_type,match,file,line,confidence,ref_type}],total,auto_linkable}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "doc_id": { "type": "string", "description": "Scan requirements from this specific document (id or slug). Omit to scan every document's SubItems." },
                    "scope_paths": { "type": "array", "items": { "type": "string" }, "description": "File/dir paths to scan (relative to project_dir, or absolute). Defaults to the union of the target document(s)' own scope_paths." },
                    "patterns": { "type": "array", "items": { "type": "string", "enum": ["test_name", "comment", "symbol"] }, "description": "Search pattern types to apply. Defaults to all three." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_doc_req_impact".to_string(),
            description: "Reverse-trace impact analysis: given a file path (or every file changed per `git diff HEAD --name-only` when git_diff=true), finds every requirement (a SubItem with a stable_id, across every document's verification matrix) affected by a change to that file (requirements-traceability P2 §5.2, `.handoff/docs/_doc.req-traceability-mcp-plan.md`). A SubItem is affected when the target file matches one of its impl_refs (match_type='impl_ref'), one of its test_refs (match_type='test_ref'), or — when neither ref matches — its owning document's scope_paths as a path prefix (match_type='scope_path', an indirect match). Path comparison is normalized (backslashes converted to '/', a leading './' and a trailing '/' stripped) so refs recorded with slightly different spelling still match. Exactly one of file/git_diff must effectively be used; when both are given, file takes priority. Omitting both is an error. Returns a JSON string {affected_requirements:[{stable_id,title,priority,dev_stage,match_type,doc_slug}],total}, sorted by stable_id.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "file": { "type": "string", "description": "File path to check impact for. Takes priority over git_diff when both are given." },
                    "git_diff": { "type": "boolean", "description": "Auto-detect changed files via `git diff HEAD --name-only` in project_dir, and check impact for all of them. Ignored if file is given." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_record".to_string(),
            description: "Records one execution batch (a set of {item,result} pairs — e.g. one CI run or one manual verification pass) as a single new file under .handoff/runs/, and refreshes the derived runs/_latest.json cache (each item's most recent result, so trace lookups never need to rescan every run file). result must be one of pass/fail/blocked/not_run/skipped. Each result's body_hash is filled in automatically from the matching SubItem's current body_hash (never supplied by the caller) — an item stable_id that does not resolve to any SubItem is still recorded, with a warning returned rather than an error. commit defaults to `git rev-parse --short HEAD` in project_dir (empty string on failure); task_id is optional free-form linkage. test_run_id (wiki/270-vmodel-m3-design.md §2.6/§4.4, M3-09, FR-304) optionally attributes this batch to a test run created via handoff_trace_test_run(action=\"create\") — folded additively into runs/_latest.json's by_test_run map (the existing items map is unaffected), queryable via handoff_trace_test_run(action=\"progress\"); no existence check against the test run's own definition file. Does NOT rebuild .handoff/docs/_trace_report.json itself (t360.13: measured ~271ms at L scale when tried, vs. this op's own ~100ms budget) — call handoff_trace_report (or CLI `trace report`) afterward to refresh it; a stale _trace_report.json is detectable via its inputs fingerprint. For `cargo test --format json` JSONL output, prefer `handoff_trace_ingest(format=\"cargo_json\")`, which parses that output and calls this tool internally per matched test. Returns a JSON string {run_id,recorded,warnings}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "results": {
                        "type": "array",
                        "description": "One or more {item,result,note?,evidence?} entries recorded together as a single run.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "item": { "type": "string", "description": "The stable_id this result is for." },
                                "result": { "type": "string", "description": "Result value.", "enum": ["pass", "fail", "blocked", "not_run", "skipped"] },
                                "note": { "type": "string", "description": "Free-form note." },
                                "evidence": { "type": "array", "items": { "type": "string" }, "description": "Free-form evidence references (e.g. test names, log excerpts, file paths)." }
                            },
                            "required": ["item", "result"]
                        }
                    },
                    "executor_kind": { "type": "string", "description": "Who/what ran this.", "enum": ["ai", "human"], "default": "ai" },
                    "executor_id": { "type": "string", "description": "Identifier of the executor (e.g. an agent id)." },
                    "commit": { "type": "string", "description": "Commit this run was executed against. Defaults to `git rev-parse --short HEAD` in project_dir (empty string if that fails)." },
                    "task_id": { "type": "string", "description": "Task this run is associated with, if any." },
                    "test_run_id": { "type": "string", "description": "Attributes this batch to a test run created via handoff_trace_test_run(action=\"create\"). No existence check against its definition file." }
                },
                "required": ["results"]
            }),
        },
        ToolDefinition {
            name: "handoff_trace_report".to_string(),
            description: "V-model trace derivation report (wiki/220-vmodel-integration-design.md §3.2): builds one derivation graph from every layer document, task<->requirement links, and the runs/_latest.json cache. Before aggregating, re-syncs any layer document whose body was edited directly since its last sync (raw-byte hash mismatch, not the lexsim content_hash) and self-repairs SubItem.task_ids drift (a no-op unless task links changed since the last full rebuild). Also (re)writes .handoff/docs/_trace_report.json (§3.4, t360.13; schema_version 2 as of M2-07, wiki/260-vmodel-m2-design.md §5.1) from the same graph — schema_version, an inputs freshness fingerprint (docs_max_mtime_ns/docs_count/tasks_max_mtime_ns/tasks_count/runs_count/runs_max_id/config_fnv), plus trace_layers/layer_defs/profile/coverage/gaps/gap_counts/suspect_counts/items/tasks (always as if include_items=true and with no gap_kinds/limit filtering, independent of this call's own arguments) — unformatted JSON, only actually rewritten when its content differs from what's on disk. Each items[] entry there (and in this call's own include_items=true response) additionally carries def_hash, coverage:{horizontal,vertical}, suspect:[{kind,upstream?,task?,link_type,baseline_hash,current_hash}], reverify, approval (draft|approved), acceptance:[{id,label,kind}], implicit_of?, derived?, waivers:[{axis,reason}], from? (wiki/260 §5.1 v2). This write, the resync, and the self-repair are the only side effects this tool has; CLI `trace report` calls this same handler. layers restricts the 'in use' layer set for this call's own response only (overrides [trace] layers config; omit/empty falls back to config, then the project default [trace] profile's own layers if one is set (wiki/260-vmodel-m2-design.md §2.1), then auto-detection from which layers actually have items) — a call with a non-empty layers override does not write _trace_report.json at all, since that file always reflects the configured/profile/auto-detected layer set. gap_kinds restricts the gaps[] list to the given kinds (gap_counts always reports every kind's total, independent of this filter). limit (default 50) truncates gaps[] after kind-filtering; a truncation is noted in warnings. include_items (default false) adds an items[] array (id, layer, side, title, state, refines, verifies, tasks:[{id,role}], doc, seq, sub_item_index, priority, dev_stage, category, impl_refs, test_refs, last_run?, profile — the named profile(s) governing the item's tree, [] when none) to this call's own response, same shape _trace_report.json's items[] always carries. Returns a JSON string {trace_layers:{in_use,source},coverage:{<layer>:{total,horizontal:{covered,partial,uncovered,waived,na},vertical:{covered,partial,uncovered,waived,na},state:{passing,failing,blocked,not_run,uncovered}}},gaps:[{kind,item,layer,detail}],gap_counts:{<kind>:count},warnings,items?} — each axis's coverage percentage is covered/(total-na-waived) (wiki/260-vmodel-m2-design.md §3.1/§5.1 v2; `partial` counts toward the denominator but not the numerator).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "layers": { "type": "array", "items": { "type": "string" }, "description": "Restrict the 'in use' layer set to these layer ids for this call only. Omit/empty to use [trace] layers config, falling back to auto-detection." },
                    "gap_kinds": { "type": "array", "items": { "type": "string", "enum": ["unverified", "unrefined", "orphan", "dangling", "invalid_link", "cycle", "duplicate_id", "task_unlinked"] }, "description": "Restrict the gaps[] list to these kinds. gap_counts always covers every kind regardless of this filter." },
                    "limit": { "type": "integer", "description": "Maximum number of gaps[] entries returned after kind-filtering.", "default": 50 },
                    "include_items": { "type": "boolean", "description": "Include a full items[] array in this call's own response, same shape _trace_report.json's items[] always carries.", "default": false }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_slice".to_string(),
            description: "Progressive-disclosure neighborhood view of the V-model trace graph around one task or item (wiki/220-vmodel-integration-design.md §3.3, FR-701 — saves AI context vs. a full handoff_trace_report). Exactly one of task_id/item is required; task_id's starting set is every stable_id that task has a requirement link to (any role). An unknown task_id or item is an error (not an empty {items:[],truncated:false} result — every real item always includes at least itself, so an empty result would otherwise be misread as \"this item has no links\"); an existing task with no requirement links returns an empty items[] (not an error). direction (default \"both\"): \"up\" follows refines toward upper left-side items plus verifies toward the left-side item being verified; \"down\" follows refines toward refining children plus the verifiers that verify this item; \"both\" is the union of the up-only and down-only walks from the same starting set (not a single traversal that can turn around partway and pull in unrelated siblings). depth (default unlimited) caps each one-way walk; a graph cycle always stops it via its own visited set regardless of depth. expand is a list of stable_ids for which the response also includes statement (re-extracted from the item's current layer-document body — every other item gets id/layer/side/title/state/refines/verifies/tasks/profile/coverage/suspect/reverify/approval only). max_items (default 30) caps the returned items[]; when the full reachable set is larger, items[] is truncated (farthest-reached items dropped first) and truncated is true. Only ids that resolve to a real item count toward max_items — a dangling id (e.g. a task's requirement link whose owning document was deleted) is never returned and never consumes a slot. Before traversing, re-syncs any directly-edited layer document (same side effect as handoff_trace_report); that resync's warnings (e.g. removed: [ids]) are returned in warnings[]. Returns a JSON string {items:[{id,layer,side,title,state,refines,verifies,tasks:[{id,role}],profile,coverage:{horizontal,vertical},suspect:[{kind,upstream?,task?,link_type,baseline_hash,current_hash}],reverify,approval,statement?}],truncated,warnings} (coverage/suspect/reverify/approval added M2-07, wiki/260-vmodel-m2-design.md §4.11/§3.1/§3.2/§3.3).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "task_id": { "type": "string", "description": "Starting set = every stable_id this task has a requirement link to. Mutually exclusive with item." },
                    "item": { "type": "string", "description": "Starting stable_id. Mutually exclusive with task_id." },
                    "direction": { "type": "string", "description": "Traversal direction relative to the starting set.", "enum": ["up", "down", "both"], "default": "both" },
                    "depth": { "type": "integer", "description": "Maximum BFS depth from the starting set. Omit for unlimited (still stops at a cycle)." },
                    "expand": { "type": "array", "items": { "type": "string" }, "description": "stable_ids to include a re-extracted statement for." },
                    "max_items": { "type": "integer", "description": "Maximum items[] entries returned.", "default": 30 }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_history".to_string(),
            description: "Execution history for one V-model trace item (wiki/220-vmodel-integration-design.md §3.4, VSCode FR-903's execution-history display; CLI `trace history`). Every recorded result for item across .handoff/runs/ (month subdirectories included), newest first by (executed_at, run_id). Pure read — never writes _trace_report.json or any other derived file. limit (default 20) caps the returned items[]. Returns a JSON string {items:[{run_id,executed_at,executor:{kind,id?},result,note,evidence,commit}]}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "item": { "type": "string", "description": "The stable_id whose recorded results to list." },
                    "limit": { "type": "integer", "description": "Maximum items[] entries returned.", "default": 20 }
                },
                "required": ["item"]
            }),
        },
        ToolDefinition {
            name: "handoff_trace_baseline".to_string(),
            description: "Baseline snapshots, their coverage-trend index, and diffs between them (wiki/270-vmodel-m3-design.md §2.4/§4.1, M3-06/M3-07, FR-405 in full). action=\"create\" (default, write): tag? (defaults to `git tag --points-at HEAD` in project_dir — the first tag when HEAD carries several, null when none; an explicit value is stored verbatim with no check against git), label?, commit?, executor_kind? (\"ai\" default | \"human\"), executor_id?. Internally regenerates .handoff/docs/_trace_report.json via the same write path handoff_trace_report uses (layer-doc resync + task_ids self-repair included — a no-op write when nothing changed since the last report, so this call is cheap when the report is already fresh and pays the same cost handoff_trace_report would when it isn't), then extracts a lightweight snapshot of each item ({id,layer,def_hash,refines,verifies,last_run,coverage,approval,suspect} — title/tasks/doc/profile and the rest are dropped) into a new .handoff/trace/baselines/<baseline_id>.json file alongside coverage_summary (per-layer horizontal/vertical CoverageCounts, same shape handoff_trace_report's own coverage field uses), gap_counts, and state_summary ({passing,failing,blocked,not_run,uncovered} tallied from the snapshot items' own state). Appends one entry to the derived .handoff/trace/baselines/_index.json cache (tmpfile->rename atomicity; rebuilt from baselines/*.json, deduped by baseline_id, whenever that cache is missing or corrupt) — handoff-vscode's t143 coverage-trend chart reads this file. Returns a JSON string {baseline_id,tag,items_count,coverage_summary,warnings}. action=\"list\" (read-only): limit? (default 20) — the most recent entries from _index.json, newest first. Returns a JSON string {baselines:[{baseline_id,created_at,tag,label,total_items,coverage_summary}],truncated}. action=\"diff\" (read-only): from and to (each required — a baseline_id, or the literal \"current\" for the on-disk _trace_report.json read as-is, never regenerated). Compares the two snapshots' items[] by id: added (only in to), removed (only in from), changed ([{id,old_hash,new_hash}] — def_hash differs on both sides), regression ([{layer,axis,old_pct,new_pct}] — a layer/axis whose covered-count ratio dropped from from to to, E20: an increase in partial at covered's expense also counts, an axis with zero items on the to side is never reported), state_changes ({<state>: delta} — only states whose count actually moved). An unresolvable from/to (unknown baseline_id, or \"current\" with no _trace_report.json yet) does not error — it is reported in warnings and the comparison is skipped (all fields empty). Returns a JSON string {added,removed,changed,regression,state_changes,warnings}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "action": { "type": "string", "description": "'create' (default, write) | 'list' (read-only) | 'diff' (read-only).", "enum": ["create", "list", "diff"], "default": "create" },
                    "tag": { "type": "string", "description": "action=create: explicit tag to record verbatim (no check against git). Omitted = auto-resolve from `git tag --points-at HEAD` (first tag if several, null if none)." },
                    "label": { "type": "string", "description": "action=create: free-form label (e.g. \"Sprint 5 release\")." },
                    "commit": { "type": "string", "description": "action=create: commit this baseline corresponds to. Not auto-resolved if omitted." },
                    "executor_kind": { "type": "string", "description": "action=create: who/what created this baseline.", "enum": ["ai", "human"], "default": "ai" },
                    "executor_id": { "type": "string", "description": "action=create: identifier of the executor (e.g. an agent id)." },
                    "limit": { "type": "integer", "description": "action=list: maximum baselines[] entries returned, newest first.", "default": 20 },
                    "from": { "type": "string", "description": "action=diff (required): a baseline_id, or the literal \"current\" for the on-disk _trace_report.json." },
                    "to": { "type": "string", "description": "action=diff (required): a baseline_id, or the literal \"current\" for the on-disk _trace_report.json." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_delta".to_string(),
            description: "Change-proposal (delta) lifecycle: create a pending bundle of ops for later human review, list/apply/reject it (wiki/270-vmodel-m3-design.md §2.5/§4.2, M3-08, FR-407). Only upsert_item/link/unlink/set ops may be proposed — record (a result) and clear_suspect (a suspect dismissal) are already-happened facts, never pending approval, and are rejected before anything is validated. action=\"create\" (write): ops (required, non-empty array, same op shapes as handoff_trace_update), description?, executor_kind? (\"ai\" default | \"human\"), executor_id?. Internally runs handoff_trace_update's own dry_run=true phase-1 validation + unified-diff preview generation over these ops (nothing is written to any document) and, only if that validation succeeds, persists the ops plus their previews and each target item's current def_hash (baseline_hashes, for later staleness detection) to a new .handoff/trace/deltas/<delta_id>.json file (create_new, NFR-007). A validation failure leaves no delta file behind. Returns {delta_id,ops_count,previews:[{op_index,diff}],warnings}. action=\"list\" (read-only, default): status? (\"pending\" default | \"applied\" | \"rejected\" | \"all\"), limit? (default 20). Returns the most recent matching deltas, newest first, each annotated with stale (true when any of its baseline_hashes no longer matches that item's current def_hash — computed fresh on every call, not cached). Returns {deltas:[{delta_id,created_at,status,description,ops_count,stale}],truncated}. action=\"apply\" (write): delta_id (required, must be status=\"pending\"), op_indices? (0-based positions within this delta's own ops array — omitted applies all of them), force? (default false — apply a stale delta anyway, with a warning, instead of erroring), executor_kind?, executor_id?. Staleness is checked first (same definition as list's stale flag) and blocks the apply unless force=true. The selected ops are then applied through handoff_trace_update's own real write path (dry_run=false) — same phase-1 validation, same fixed category order, same all-or-nothing-per-category failure contract. On success this delta's status becomes \"applied\" unconditionally; when op_indices named only some of the ops, the rest are moved into a brand-new pending delta with their own ops/previews/baseline_hashes renumbered from 0 (remainder_delta_id) — the original delta never goes back to pending once any of its ops have been applied. Returns {applied:[{op_index,op,result}],remainder_delta_id,warnings}. action=\"reject\" (write): delta_id (required, must be status=\"pending\"), reason?, executor_kind?, executor_id?. Sets status to \"rejected\" (terminal — a rejected delta can never be applied). Returns {delta_id,status:\"rejected\",warnings}. See also handoff_trace_update's own propose argument, which runs this same create path from inside a single trace_update call.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "action": { "type": "string", "description": "'create' (write) | 'list' (default, read-only) | 'apply' (write) | 'reject' (write).", "enum": ["create", "list", "apply", "reject"], "default": "list" },
                    "ops": {
                        "type": "array",
                        "description": "action=create (required, non-empty): upsert_item/link/unlink/set ops only (same shapes as handoff_trace_update's own ops) — record/clear_suspect are rejected.",
                        "items": { "type": "object" }
                    },
                    "description": { "type": "string", "description": "action=create: free-form description of what this delta proposes." },
                    "status": { "type": "string", "description": "action=list: filter by status.", "enum": ["pending", "applied", "rejected", "all"], "default": "pending" },
                    "limit": { "type": "integer", "description": "action=list: maximum deltas[] entries returned, newest first.", "default": 20 },
                    "delta_id": { "type": "string", "description": "action=apply|reject (required): the delta to act on." },
                    "op_indices": { "type": "array", "items": { "type": "integer" }, "description": "action=apply: 0-based positions within this delta's own ops array to apply. Omitted = apply all of them." },
                    "force": { "type": "boolean", "description": "action=apply: apply even if the delta is stale (one or more target items changed since it was created).", "default": false },
                    "reason": { "type": "string", "description": "action=reject: why this delta is being rejected, recorded in the delta file." },
                    "executor_kind": { "type": "string", "description": "Who/what is performing this action.", "enum": ["ai", "human"], "default": "ai" },
                    "executor_id": { "type": "string", "description": "Identifier of the executor (e.g. an agent id)." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_ingest".to_string(),
            description: "Ingests a cargo or JUnit XML test run and records matched results against V-model trace items (wiki/260-vmodel-m2-design.md §4.6, M2-11, FR-306): one aggregated runs/<run_id>.json entry per layer item (any fail wins, all-skipped wins next, else pass across every test matched to that item), one test_refs label update per matched test for layer-less items (M1's convention). format=\"cargo_json\" expects libtest's JSON-per-line output; getting that output requires an unstable flag: `cargo +nightly test -- -Z unstable-options --format json`, or on stable `RUSTC_BOOTSTRAP=1 cargo test -- -Z unstable-options --format json` (there is no stable `cargo test --format json`). format=\"junit_xml\" is recommended instead: `cargo nextest run` with a `[profile.<name>.junit]` section writes a JUnit XML report with no unstable flags needed. Matching (3 stages, tried in priority order): (1) exact match against one of an item's declared `- test: <value>` lines, (2) a `::`-boundary suffix match against one (e.g. an item's `test: lock::lock_after_5` matches output test `tests::e2e::lock::lock_after_5`), (3) the M1 stable_id->test-name-prefix convention (independent of any declared value, so a layer item with no `test` attribute at all still matches exactly as before this tool existed). An item with at least one declared `test` value is only recorded when this ingestion's output covers *every* one of its declared values — a value with zero matches is reported in missing_refs instead, and that item is not recorded at all this call (a partial run must never look like a fuller not_run/pass/fail overwrite). Exactly one of output/output_file is required (output takes priority when both given). dry_run (default false) parses and matches without writing anything. commit defaults to `git rev-parse --short HEAD`; task_id is optional free-form linkage; executor_kind defaults to \"ai\". test_run_id (wiki/270-vmodel-m3-design.md §2.6/§4.4, M3-09, FR-304) optionally attributes this ingestion's run to a test run created via handoff_trace_test_run(action=\"create\") — same additive runs/_latest.json by_test_run wiring handoff_trace_record's own test_run_id argument has. Returns a JSON string {run_id?,recorded,matched:[{item,result,tests:[string]}],missing_refs:[{item,test}],unmatched_tests_count,warnings,dry_run}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "format": { "type": "string", "description": "Input format.", "enum": ["cargo_json", "junit_xml"] },
                    "output": { "type": "string", "description": "Raw test-run output (libtest JSON-per-line for cargo_json, JUnit XML for junit_xml). Takes priority over output_file when both are given." },
                    "output_file": { "type": "string", "description": "Path to a file containing the test-run output. Ignored if output is given." },
                    "commit": { "type": "string", "description": "Commit this run was executed against. Defaults to `git rev-parse --short HEAD` in project_dir (empty string if that fails)." },
                    "task_id": { "type": "string", "description": "Task this run is associated with, if any." },
                    "test_run_id": { "type": "string", "description": "Attributes this ingestion's run to a test run created via handoff_trace_test_run(action=\"create\"). No existence check against its definition file." },
                    "executor_kind": { "type": "string", "description": "Who/what ran this.", "enum": ["ai", "human"], "default": "ai" },
                    "executor_id": { "type": "string", "description": "Identifier of the executor (e.g. an agent id)." },
                    "dry_run": { "type": "boolean", "description": "Parse and match without writing any run file or test_refs update.", "default": false }
                },
                "required": ["format"]
            }),
        },
        ToolDefinition {
            name: "handoff_trace_scaffold".to_string(),
            description: "Generates one verification-layer item per acceptance-criteria bullet of a source (left-side) item (wiki/260-vmodel-m2-design.md §4.7, M2-12, FR-305). Exactly one of items ([stable_id, ...]) or doc (a document slug/id — every item in it with an acceptance-criteria block) selects the source items; target_doc (required, a layer document's slug or id) is where generated items are appended. Each acceptance-criteria bullet becomes one new item: id = \"<target layer's default id prefix>-<source id>-<AC number>\" (e.g. \"AT-REQ-003-1\" from REQ-003's AC1, verifying an `acceptance` target_doc), body attrs `verifies: <source id>`, `from: <source id>#<AC label>`, `method: manual`. A `gwt`-kind AC (Given...When...Then) splits into a 手順 (steps, the Given/When clauses) and 期待結果 (expected result, the Then clause, also the heading title); an `ears`/`text`-kind AC has no distinguishable steps (手順: （記入）, a fill-in placeholder), uses its full text as 期待結果, and its first 40 characters as the heading title. Idempotent: an AC that already has a scaffolded item anywhere in the project (a `from` value equal to `<source id>#<AC label>`) is skipped (reported in skipped, not regenerated) and does not count against limit. An id collision with any existing stable_id in the project is avoided by appending a single lowercase-letter suffix (`AT-REQ-003-1a`, `...1b`, ...; a warning is returned if all 26 are already taken). mode=\"preview\" (default) computes generated/skipped without writing; mode=\"apply\" appends the rendered Markdown to target_doc's body in one `doc_save(append_body=...)` call (which runs the real parser + layer sync), skipping the write entirely when there is nothing to generate. limit (default 20) caps how many new items are generated per call (skipped ACs don't count). If a source AC already has an implicit acceptance-verification item (a minimal-profile document's auto-materialized `<source id>#<AC label>` SubItem) with a recorded run, a warning notes that the run is not carried over to the newly scaffolded item. Returns a JSON string {target_doc,mode,applied,generated:[{id,from,title}],skipped:[{ac,existing}],warnings}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "items": { "type": "array", "items": { "type": "string" }, "description": "Source stable_ids to scaffold acceptance criteria from. Mutually exclusive with 'doc'." },
                    "doc": { "type": "string", "description": "Source document (slug or id) — every item in it with an acceptance-criteria block is scaffolded. Mutually exclusive with 'items'." },
                    "target_doc": { "type": "string", "description": "The verification-layer document (slug or id) generated items are appended to." },
                    "mode": { "type": "string", "description": "'preview' (default) computes without writing; 'apply' appends the generated items to target_doc's body.", "enum": ["preview", "apply"], "default": "preview" },
                    "limit": { "type": "integer", "description": "Maximum number of new items generated per call (skipped/idempotent ACs don't count).", "default": 20 }
                },
                "required": ["target_doc"]
            }),
        },
        ToolDefinition {
            name: "handoff_trace_suspect".to_string(),
            description: "Derives and manages the 3 suspect kinds a V-model link/task/result can fall into when its upstream definition changed since it was baselined (wiki/260-vmodel-m2-design.md §3.2/§4.1, M2-05). action=\"list\" (default, read-only): {suspects:[{kind:\"link\"|\"task\"|\"result\",item,upstream?,task?,link_type?,baseline_hash,current_hash}],counts:{link,task,result},unbaselined:{links,tasks},reverify:[stable_id,...],truncated}. `link` = a child's refines/verifies reference whose baseline no longer matches the upstream's current def_hash (or ac_hash for an `X#ACn` sub-reference); `task` = a task's requirement link whose baseline_hash no longer matches the linked item's current def_hash; `result` = a verification item's latest recorded pass whose recorded def_hash (or body_hash, for a pre-M2-02 run) no longer matches. `unbaselined` counts references/links with no baseline yet (never suspects themselves — see action=\"baseline\"). `reverify` lists Passing verification items whose own result is suspect or one of whose verifies links is suspect (does not change `state`, wiki/260 §11 Q1). Filters: item, task_id, kinds (subset of link/task/result), layers, limit (default 50). action=\"clear\": targets (required, array — each entry is one of {item,upstream} exact link, {item} all of that item's suspect links, {upstream} every link pointing at that upstream (bulk), {task_id,item?} task link(s), {layer} every suspect in that layer, {result:item} that item's result suspect), reason (required), evidence? ({run_id?,commit?,note?}), executor_kind (default \"ai\"), executor_id?. Moves each selected suspect's baseline to the current hash (a link's SubItem.link_baselines entry, a task's TaskLink.baseline_hash) — a result clear instead records one new carried-forward runs/<run_id>.json entry (reusing the last result verbatim against the item's current hashes; the authority for results stays runs-only). Writes one audit file per call to .handoff/trace/clears/<id>.json. Returns {cleared:{links,tasks,results},clear_id,warnings}. action=\"baseline\": migration helper for links/tasks that have no baseline at all (wiki/260 §7) — dry_run (default true) previews the count; scope? ({doc} or {layer}, omitted = whole project) restricts which unbaselined references are considered. Never touches an already-suspect link. Returns {baselined:{links,tasks,results:0},dry_run,warnings}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "action": { "type": "string", "description": "'list' (default, read-only) | 'clear' | 'baseline'.", "enum": ["list", "clear", "baseline"], "default": "list" },
                    "item": { "type": "string", "description": "action=list: restrict to suspects about this stable_id." },
                    "task_id": { "type": "string", "description": "action=list: restrict to suspects about this task." },
                    "kinds": { "type": "array", "items": { "type": "string", "enum": ["link", "task", "result"] }, "description": "action=list: restrict to these suspect kinds." },
                    "layers": { "type": "array", "items": { "type": "string" }, "description": "action=list: restrict to suspects whose item is in one of these layers." },
                    "limit": { "type": "integer", "description": "action=list: maximum suspects[] entries returned.", "default": 50 },
                    "targets": { "type": "array", "items": { "type": "object" }, "description": "action=clear (required): one or more of {item,upstream}, {item}, {upstream}, {task_id,item?}, {layer}, {result:item}." },
                    "reason": { "type": "string", "description": "action=clear (required): why this suspect is safe to clear, recorded in the audit file." },
                    "evidence": { "type": "object", "description": "action=clear: optional {run_id?,commit?,note?} recorded in the audit file." },
                    "executor_kind": { "type": "string", "description": "Who/what is clearing this.", "enum": ["ai", "human"], "default": "ai" },
                    "executor_id": { "type": "string", "description": "Identifier of the executor (e.g. an agent id)." },
                    "dry_run": { "type": "boolean", "description": "action=baseline: preview without writing.", "default": true },
                    "scope": { "type": "object", "description": "action=baseline: restrict to {doc: \"<slug or id>\"} or {layer: \"<id>\"}. Omitted = whole project." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_impact".to_string(),
            description: "Read-only impact analysis for a proposed change (wiki/260-vmodel-m2-design.md §4.2, M2-06, FR-404) — never writes anything, including _trace_report.json. Exactly one entry point is required: (1) item (+ optional proposed or proposed_file, a Markdown block containing that item's own heading) — parses the proposed text the same way a real layer sync would to get a hypothetical new def_hash/ac_hash; omitting both proposed and proposed_file means \"assume changed\" (a sentinel hash that reads as different from every real baseline, since no concrete new hash is known). (2) doc (+ required proposed_body or proposed_body_file, a full layer-document body) — every item common to both the proposed parse and the current corpus whose def_hash/ac_hash would change is treated the same way, for the whole document at once; any id the target document currently owns (including its materialized implicit-acceptance items) that the proposed body no longer defines at all is reported as a deletion instead (see `removed` below), for item mode scoped to just that one item and its own acceptance criteria. (3) file — the M1-era req_impact entry point (shares its file-matching core with handoff_doc_req_impact): every requirement whose impl_refs/test_refs/scope_paths match this file is treated as \"implementation changed\" (no def_hash simulation, so would_suspect stays empty; only rerun_candidates is populated). (4) git_diff: true — same as file, but against every path `git diff HEAD --name-only` reports changed. Returns a JSON string {changed:[ids],removed:[{id,downstream_refs:[{child,type}],tasks:[ids]}],would_suspect:{links:[{child,upstream,type}],tasks:[{task,item}]},rerun_candidates:[ids],potential:[{id,depth}],truncated,warnings} — would_suspect only ever reports a downstream reference/task link that already has a recorded baseline (an unbaselined one is never guessed at); removed lists an id the proposal deletes outright, with its direct downstream references/task links (these would become dangling once the proposal lands, never suspect — there is no current hash left on the removed side to compare a baseline against; also echoed into warnings); rerun_candidates is every changed id's own verifying children (any verifies edge to it, regardless of baseline) plus a changed id that is itself a verification item; potential is a breadth-first walk over the same refines/verifies reverse-edge index starting from changed, reporting only depth >= 2 (\"if the intermediate item also changed\" ripple, informational only — suspects never actually propagate past one hop, wiki/260 §11 E1), capped by limit (default 50, truncated is true when more were reachable).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "item": { "type": "string", "description": "Entry point 1: a stable_id. Mutually exclusive with doc/file/git_diff." },
                    "proposed": { "type": "string", "description": "item mode: a Markdown block containing item's own heading. Takes priority over proposed_file when both are given. Omitting both means \"assume changed\"." },
                    "proposed_file": { "type": "string", "description": "item mode: path to a file containing the proposed Markdown block. Ignored if proposed is given." },
                    "doc": { "type": "string", "description": "Entry point 2: a layer document slug or id (a non-layer document is rejected). Requires proposed_body or proposed_body_file. Mutually exclusive with item/file/git_diff." },
                    "proposed_body": { "type": "string", "description": "doc mode (required unless proposed_body_file is given): the full proposed document body. Takes priority over proposed_body_file when both are given." },
                    "proposed_body_file": { "type": "string", "description": "doc mode: path to a file containing the full proposed document body. Ignored if proposed_body is given." },
                    "file": { "type": "string", "description": "Entry point 3: a file path. Mutually exclusive with item/doc/git_diff." },
                    "git_diff": { "type": "boolean", "description": "Entry point 4: use every path `git diff HEAD --name-only` reports changed. Mutually exclusive with item/doc/file.", "default": false },
                    "limit": { "type": "integer", "description": "Maximum potential[] entries returned.", "default": 50 }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_lint".to_string(),
            description: "Read-only lint over the whole trace graph (wiki/260-vmodel-m2-design.md §4.3, M2-08, FR-503/801; wiki/270-vmodel-m3-design.md §4.6, M3-10, FR-504 added the quality rules and action=\"quality_prompt\") — never writes anything, E6 (in-memory-only resync of a directly-edited layer document, runs::load_latest_readonly, task_ids resolved from the task side without self-repair). action? (\"lint\" default | \"quality_prompt\"). action=\"lint\": Built-in rules (default severity in parens, overridable per id via config.toml's [trace.lint.rules] = \"error\"|\"warning\"|\"info\"|\"off\"): structural — unverified/unrefined/orphan/task_unlinked (warning), dangling/invalid_link/cycle/duplicate_id (error); change — suspect_link/suspect_task/stale_result (warning), unbaselined (info); tailoring — waiver_on_na/unlabeled_acceptance/invalid_waiver/unknown_acceptance_ref (warning), redundant_waiver/layer_outside_profile (info); drift (FR-801) — unsynced_body/task_link_dangling (warning), task_ids_drift/orphaned_legacy/orphan_run/id_like_heading (info); quality (FR-504, info) — ambiguous_word (title matches a known vague term list, Japanese and English), missing_acceptance (a requirement-layer item with no acceptance-criteria block), passive_voice_hint (title matches a Japanese 「される」/「られる」 marker or an English \"is/are/was/were + past participle\" pattern); format — frontmatter_invalid (error, unparseable _doc.*.md). Project-defined [[trace.lint.require]] policy rules (e.g. \"approved P0/P1 requirements need a verifier\": when={layer,priority,method,doc,approval}, need=verified_by|refined_by|implemented_by_task|passing|no_suspect|auto_test, severity default error) are evaluated alongside the built-ins, each producing its own rule id in findings. Input: rules? (restrict to these rule/require ids), fail_on? (\"error\" default | \"warning\" — which severity makes exit_code 1), format? (\"json\" default | \"text\", adds a rendered text field for CLI display), limit? (truncates findings, counts/exit_code still reflect every match). Output: {findings:[{rule,severity,item?,task?,doc?,message}],counts:{error,warning,info},exit_code,warnings,text?} sorted deterministically (severity descending, then rule id, then item's natural order). CLI `trace lint` exit codes: 0 = no finding at/above fail_on, 1 = at least one, 2 = usage/config error (unknown rule id, invalid fail_on, or any other handler error). action=\"quality_prompt\" (E22: this server never calls an LLM itself — it only returns prompt templates for the caller, e.g. a session-loop AI agent, to fill and send to an LLM): items? ([stable_id, ...], present-but-empty is a usage error; omitted = every requirement-layer item), aspects? ([one of singular|verifiable|unambiguous|complete|feasible|traceable], ISO/IEC/IEEE 29148-aligned; an unrecognized name is rejected, not silently dropped; omitted = every aspect). Output: {prompts:[{item_id,title,text,aspects:[{name,prompt_template,context}]}],warnings} — prompt_template contains a literal \"{text}\" placeholder the caller substitutes with the item's own text (title and text are currently the same value — SubItem.description — since the body statement itself is not persisted) before sending it to an LLM; context is a one-line explanation of what that 29148 characteristic means.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "action": { "type": "string", "description": "'lint' (default): evaluate built-in + [[trace.lint.require]] rules. 'quality_prompt': return LLM prompt templates for the 29148 quality aspects (wiki/270 §4.6, FR-504).", "enum": ["lint", "quality_prompt"], "default": "lint" },
                    "rules": { "type": "array", "items": { "type": "string" }, "description": "action='lint' only. Restrict evaluation to these rule/require ids. Omitted = every rule." },
                    "fail_on": { "type": "string", "description": "action='lint' only. Severity that makes exit_code 1 (CLI).", "enum": ["error", "warning"], "default": "error" },
                    "format": { "type": "string", "description": "action='lint' only. 'json' (default) | 'text' (adds a rendered text field, for CLI display).", "enum": ["json", "text"], "default": "json" },
                    "limit": { "type": "integer", "description": "action='lint' only. Maximum findings[] entries returned (counts/exit_code still reflect every match)." },
                    "items": { "type": "array", "items": { "type": "string" }, "description": "action='quality_prompt' only. Explicit stable_ids to generate prompts for. Omitted = every requirement-layer item." },
                    "aspects": { "type": "array", "items": { "type": "string", "enum": ["singular", "verifiable", "unambiguous", "complete", "feasible", "traceable"] }, "description": "action='quality_prompt' only. Restrict to these 29148 aspects. Omitted = every aspect." }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_matrix".to_string(),
            description: "Read-only (E6, same fully-read-only load handoff_trace_lint/handoff_trace_impact use — in-memory-only layer-doc resync, runs::load_latest_readonly, task_ids resolved from the task side without self-repair) flat export of the whole trace graph as a CSV or Markdown table (wiki/260-vmodel-m2-design.md §4.4, M2-09, FR-505) — handoff-vscode's matrix-export feature (t135) calls this instead of reimplementing CSV/Markdown generation itself (NFR-005). format (required): \"markdown\" | \"csv\". shape (default \"tree\"): \"tree\" is one row per top-level (root_layer) left-side item — wiki/230-vmodel-ui-feature-inventory.md INV-614's 'トレース表（行 = V字1本）' — with columns = every currently in-use layer (left side top to bottom, then right side top to bottom; a layer no item in the project uses gets no column) populated with that row's reachable descendants (refining children, and verifiers of anything in the row), plus a tasks column (every task with a requirement link to any id in the row, omit via include_tasks=false), then state (the root item's own aggregate TraceGraph state, already folding in every descendant) and suspect (the count of graph suspects whose relevant item is in the row). \"edges\" is one row per resolved refines/verifies link in the whole graph — {from,to,link_type,from_layer,to_layer,state (the from item's own state),suspect (true iff a link-kind suspect matches this exact {item: from, link_type, upstream->to})} — for import into an external tool; a dangling/invalid-level reference never appears here (handoff_trace_lint's dangling/invalid_link gaps cover those). root_layer (tree only, default: the shallowest-level left-side layer currently in use) must name a registered layer id; naming one no item in the project uses is not an error, it just yields zero rows. layers? restricts/reorders the column set to this list, intersected with (and reordered to match) the canonical in-use order — any entry that is unknown or not currently in use is dropped with a warning, never an error. A cell holding more than one id joins them with '; ' in CSV, '<br>' in Markdown (also how a literal embedded newline renders); CSV is RFC 4180 (every cell quoted, no BOM, LF line endings), Markdown escapes a literal '|'. output_file? is a path relative to the project directory (not .handoff/) to write the rendered content to instead of returning it inline — an absolute path or one that would escape the project directory (a '..' component) is rejected; this is the only write this otherwise fully read-only tool ever makes. Returns a JSON string {format,shape,root_layer? (tree only),columns,rows (row count),content? (omitted when output_file was given),output_file?,warnings}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "format": { "type": "string", "description": "Required. Render target.", "enum": ["markdown", "csv"] },
                    "shape": { "type": "string", "description": "'tree' (default): one row per root_layer item, columns = in-use layers + tasks + state + suspect. 'edges': one row per resolved refines/verifies link.", "enum": ["tree", "edges"], "default": "tree" },
                    "root_layer": { "type": "string", "description": "shape='tree' only. A registered layer id to use as the row root. Default: the shallowest-level left-side layer currently in use." },
                    "layers": { "type": "array", "items": { "type": "string" }, "description": "Restricts/reorders the column set to these layer ids (canonical order wins over the array's own order). Entries unknown or not currently in use are dropped with a warning." },
                    "include_tasks": { "type": "boolean", "description": "shape='tree' only. Include the 'tasks' column.", "default": true },
                    "output_file": { "type": "string", "description": "Relative path (inside the project directory, not .handoff/) to write the rendered content to instead of returning it inline. An absolute path or one escaping the project directory is rejected." }
                },
                "required": ["format"]
            }),
        },
        ToolDefinition {
            name: "handoff_trace_next".to_string(),
            description: "Read-only (E6, same fully-read-only load handoff_trace_lint/handoff_trace_matrix use — in-memory-only layer-doc resync, runs::load_latest_readonly, task_ids resolved from the task side without self-repair) ranking of \"what to do next\" across the whole trace graph into 9 kinds (wiki/260-vmodel-m2-design.md §3.5/§4.5, M2-10, FR-703; wiki/270-vmodel-m3-design.md §4.5, M3-02, FR-307 added manual_pending), each rank's detection rule and suggested follow-up call: 1 fix_failing (a verification item whose state is failing/blocked -> handoff_trace_slice then fix then handoff_trace_record), 2 review_suspect (an item with a suspect upstream link -> handoff_trace_impact then fix the body or handoff_trace_suspect clear), 3 rerun (a reverify or never-run verifier whose verified target is already implemented or beyond -> run the test then handoff_trace_ingest/handoff_trace_record) and manual_pending sharing the same rank 3 (an assignee-bearing item whose method is manual/visual/review and whose latest result is not_run -> handoff_trace_record), 4 write_verification (a left-side item whose horizontal coverage is uncovered or partial -> handoff_trace_scaffold or handoff_trace_update upsert_item), 5 refine (a left-side item whose vertical coverage is uncovered or partial -> handoff_trace_update upsert_item), 6 create_task (a not_started left-side item with no implementing task yet -> handoff_trace_tasks), 7 fix_link (a dangling/invalid_link/cycle/duplicate_id/orphan structural gap -> fix the body), 8 baseline (an unbaselined refines/verifies reference -> handoff_trace_suspect baseline). Within the same rank, ordering is deterministic: item priority (P0 before P1 before P2 before P3, unknown/missing last) -> the item's own layer level (shallower/upper first) -> stable_id natural order. task_id? narrows the candidate set to only that task's own requirement-linked items (same starting-set rule handoff_trace_slice's task_id branch uses). layers?[] restricts to items whose own effective layer is one of these, applied before ranking (never lets an out-of-scope item's candidate displace an in-scope one before limit truncation). assignee? restricts to items whose own SubItem.assignee matches exactly, applied the same way as layers?[]. kinds?[] restricts to these kind ids only (same 'present but empty is a usage error, unknown id is rejected rather than silently dropped' policy handoff_trace_lint's rules filter applies). limit? (default 10) truncates the final list; truncated reports whether more candidates existed. Returns a JSON string {actions:[{rank,kind,item?,task?,priority?,reason,suggest:{tool,arguments}}],truncated,warnings}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "task_id": { "type": "string", "description": "Restricts the candidate set to this task's own requirement-linked items only." },
                    "layers": { "type": "array", "items": { "type": "string" }, "description": "Restricts the candidate set to items whose own effective layer is one of these ids." },
                    "assignee": { "type": "string", "description": "Restricts the candidate set to items whose own SubItem.assignee matches exactly (wiki/270 §4.5, FR-307)." },
                    "kinds": { "type": "array", "items": { "type": "string", "enum": ["fix_failing", "review_suspect", "rerun", "manual_pending", "write_verification", "refine", "create_task", "fix_link", "baseline"] }, "description": "Restrict to these next-action kinds only. Omit entirely to run every kind." },
                    "limit": { "type": "integer", "description": "Maximum actions[] entries returned (counts toward truncated, not a hard cap on candidates considered).", "default": 10 }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_test_run".to_string(),
            description: "Test run definitions and dynamically-computed progress (wiki/270-vmodel-m3-design.md §2.6/§4.4, M3-09, FR-304). action=\"create\" (default, write): scope ({layers?: [layer id, ...], kinds?: [one of fix_failing, review_suspect, rerun, manual_pending, write_verification, refine, create_task, fix_link, baseline — same vocabulary as handoff_trace_next's own kinds filter], assignee?}), label?. Enumerates target_items the same way handoff_trace_next would for this scope: when scope.kinds is given, candidates are every item handoff_trace_next's derivation would surface for those kinds (needs the same full trace-graph build handoff_trace_next itself pays, PR-7 class); when scope.kinds is omitted, candidates are simply every layer item whose effective layer/assignee match scope.layers/scope.assignee (no graph build at all, PR-4 class). Persists the enumerated set, plus a generated test_run_id, to a new .handoff/trace/test_runs/<test_run_id>.json definition file (never overwritten, NFR-007). Returns a JSON string {test_run_id,target_items:[ids],total_target_count,warnings}. action=\"list\" (read-only): limit? (default 20) — every test_runs/*.json definition, newest first. Returns a JSON string {test_runs:[{test_run_id,created_at,label,total_target_count}],truncated}. action=\"progress\" (read-only): test_run_id (required) — filters runs/_latest.json's by_test_run[test_run_id] map (populated by handoff_trace_record/handoff_trace_ingest's own test_run_id argument) against the definition's own target_items and dynamically tallies progress; a target item with no recorded result against this test run counts as not_run (never cached, always recomputed from runs/*.json). An unknown test_run_id is an error. Returns a JSON string {test_run_id,total,executed,pass,fail,blocked,skipped,not_run,progress_pct,remaining:[ids],warnings}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "action": { "type": "string", "description": "'create' (default, write) | 'list' (read-only) | 'progress' (read-only).", "enum": ["create", "list", "progress"], "default": "create" },
                    "scope": {
                        "type": "object",
                        "description": "action=create: restricts the enumerated target_items.",
                        "properties": {
                            "layers": { "type": "array", "items": { "type": "string" }, "description": "Restrict to items whose own effective layer is one of these ids." },
                            "kinds": { "type": "array", "items": { "type": "string", "enum": ["fix_failing", "review_suspect", "rerun", "manual_pending", "write_verification", "refine", "create_task", "fix_link", "baseline"] }, "description": "Restrict to items handoff_trace_next would surface for these kinds. Omitting this (or leaving it empty) uses the cheaper layers/assignee-only filter with no graph build." },
                            "assignee": { "type": "string", "description": "Restrict to items whose own SubItem.assignee matches exactly." }
                        }
                    },
                    "label": { "type": "string", "description": "action=create: free-form label (e.g. \"Sprint 5 regression\")." },
                    "test_run_id": { "type": "string", "description": "action=progress (required): the test run whose progress to compute." },
                    "limit": { "type": "integer", "description": "action=list: maximum test_runs[] entries returned, newest first.", "default": 20 }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_propose".to_string(),
            description: "Read-only suggestion of existing V-model items that may already cover a task, plus a ready-to-review Markdown template for a new one, sized to the project's applicable profile (wiki/260-vmodel-m2-design.md §4.10, M2-17, FR-1004). Exactly one of task_id (loads that task's own title/notes/scope_paths) or title (+ optional notes, for a task that doesn't exist yet) is required. candidates: every persisted item's stable_id/title (SubItem.description)/layer, scored by a lexical+semantic blend (the same one handoff_memory_save's near-duplicate detection uses) between the query text (title, or title+notes) and each item's own title — no document body is read, sorted by score descending, capped at limit (default 5). proposal: the applicable layer set and implicit_acceptance is resolved [trace] layers (explicit config) > the project default [trace] profile > a 'standard'-shaped fallback when neither is configured (each fallback/assumption is reported in warnings, never silent) — the new item's layer is the deepest left-side (definition) layer in that set. When implicit_acceptance is true (a minimal-shaped profile), the template is one item at that layer with an inline 受入基準 (acceptance criteria) block (its own layer sync auto-materializes the verification item per criterion, wiki/260 §2.5) — e.g. a minimal profile proposes one REQ- item. Otherwise the template is that item plus its paired verification-layer item (an explicit `- layer: <id>` override lets both live in the one proposed document even though its own default layer is the left one) — e.g. a standard profile proposes a SPEC- item paired with an ST- one. IDs are '<layer's default id prefix>-<max existing number for that prefix project-wide, +1>', zero-padded to 3 digits. doc is the layer document (matching the new item's target layer) whose scope_paths overlaps the task's own scope_paths (exact match or directory-prefix overlap, same classification handoff_claim_task's scope-conflict advisory uses); when none exists (including the title-only input, which has no task scope_paths to compare against), doc is instead a suggested (never created) new slug derived from the layer id and the query title, flagged in warnings. Creation is out of scope for this tool — apply the proposed markdown yourself via handoff_doc_save/handoff_doc_update_section, or via handoff_trace_update's upsert_item op (M2-14), only after the requester has reviewed it. Returns a JSON string {candidates:[{id,title,layer,score}],proposal:{profile,doc,markdown,next_ids}|null,warnings} — proposal is null when no left-side (definition) layer or reciprocal pair could be resolved for the applicable profile.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "task_id": { "type": "string", "description": "Loads this task's own title/notes/scope_paths as the query and placement input. Mutually exclusive with 'title'." },
                    "title": { "type": "string", "description": "Query title for a task that doesn't exist yet. Mutually exclusive with 'task_id'." },
                    "notes": { "type": "string", "description": "Optional extra query text alongside 'title' (ignored when 'task_id' is given — the task's own notes are used instead)." },
                    "limit": { "type": "integer", "description": "Maximum candidates[] entries returned.", "default": 5 }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_tasks".to_string(),
            description: "Generates one task per V-model item still missing the task role it needs (wiki/260-vmodel-m2-design.md §4.9, M2-16, FR-605). Target selection: a left-side (definition) item with no task yet holding an `implements` link to it, or a right-side (verification) item with no task yet holding an `executes` link to it AND whose latest recorded result (runs/_latest.json) is not `\"pass\"` (an already-passing verifier needs no task). An item that already has a task holding the role this call would otherwise generate is idempotent — reported in `skipped`, never regenerated. Exactly one of items ([stable_id, ...], explicit targets — any id not found in the project is reported in warnings) or select ({layers?: [layer id, ...], gap_kinds?: [one of unverified/unrefined/orphan/dangling/invalid_link/cycle/duplicate_id/task_unlinked], dev_stage?: \"not_started\"|\"in_progress\"|\"implemented\"|\"tested\"|\"verified\"}) restricts the scan; omitting both scans every item in the project. Each generated task: title `\"<stable_id> <item title>\"` (SubItem.description), a `requirement_ids`/`requirement_roles` reverse link to the source item (role implements/executes, same inference `handoff_update_task`'s own create path uses when no explicit role is given), `labels: [\"layer:<effective layer id>\"]`, `scope_paths` copied from the item's owning document (not the item itself) — done_criteria are deliberately never copied (FR-603). parent_id (optional) makes every generated task a child of that task. estimate_hours is required when mode=\"apply\" and [settings] require_estimate_hours is enabled project-wide (every generated task starts in status \"todo\", which handoff_update_task's own per-task estimate rule exempts, so this tool enforces its own upfront whole-batch requirement instead of silently creating unestimated tasks); when given, it is applied to every task this call creates. mode=\"preview\" (default) computes without writing; mode=\"apply\" creates the tasks via handoff_update_task's own shared create path (same validation: status/priority, done-guard pre-check, dependency validation, require_estimate_hours). limit (default 20) caps the number of generated/planned entries (skipped items don't count) — scanning is read-only (E6: in-memory-only layer-doc resync, runs::load_latest_readonly, task_ids resolved from the task side without self-repair), the only write path is the task creation itself. Returns a JSON string {mode,created|planned:[{task_id? (apply only),item,role,title}],skipped:[{item,role,existing}],warnings}.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "items": { "type": "array", "items": { "type": "string" }, "description": "Explicit stable_ids to consider. Mutually exclusive with 'select'. Omitting both scans every item in the project." },
                    "select": {
                        "type": "object",
                        "description": "Restricts the scan. Mutually exclusive with 'items'.",
                        "properties": {
                            "layers": { "type": "array", "items": { "type": "string" }, "description": "Restrict to items whose effective layer is one of these ids." },
                            "gap_kinds": { "type": "array", "items": { "type": "string", "enum": ["unverified", "unrefined", "orphan", "dangling", "invalid_link", "cycle", "duplicate_id", "task_unlinked"] }, "description": "Restrict to items that have at least one of these handoff_trace_lint gap kinds." },
                            "dev_stage": { "type": "string", "enum": ["not_started", "in_progress", "implemented", "tested", "verified"], "description": "Restrict to items whose own dev_stage equals this value." }
                        }
                    },
                    "parent_id": { "type": "string", "description": "Every generated task becomes a child of this task, if given." },
                    "estimate_hours": { "type": "number", "description": "Applied to every task this call creates (schedule.estimate_hours). Required for mode=\"apply\" when [settings] require_estimate_hours is enabled." },
                    "mode": { "type": "string", "description": "'preview' (default) computes without writing; 'apply' creates the tasks.", "enum": ["preview", "apply"], "default": "preview" },
                    "limit": { "type": "integer", "description": "Maximum generated/planned entries (skipped items don't count against it).", "default": 20 }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_trace_update".to_string(),
            description: "Bulk-mutation entry point over 5 op kinds in one call (wiki/260-vmodel-m2-design.md §4.8, M2-14, FR-702/FR-601's remainder). Input: ops (required, non-empty array — each entry has an 'op' field naming one of upsert_item/link/unlink/set/record/clear_suspect, see below), task_id? (default task for an op that omits its own task/task_id), dry_run? (default false), executor_kind? ('ai' default | 'human'), executor_id?, commit? (used by record ops). E15 atomicity: every op — input shape, enum values (role: implements/executes, priority: P0-P3, result, dev_stage, approval), and every referential lookup this op kind commits to (document existence, upsert_item's after target and its own id resolving to a real item once rendered, link/unlink's item and task, set's item) — is validated, in ops' input order, before anything is written; an item lookup is checked against the union of the pre-call corpus and every id an earlier upsert_item op in the same call already planned, so a link/set op can legally target an item an earlier op in the same call creates. record's item is deliberately not existence-checked (it is forwarded to handoff_trace_record, which already records an unresolvable item with a warning rather than rejecting it — trace_update does not impose a stricter policy than the single-op tool). If any op fails validation, nothing is written and the response's failed names that op (by its 0-based position in the input ops array, not execution order). Writes then run in the fixed category order §4.8/E15 specify regardless of input order — 本文 (upsert_item) -> リンク (link/unlink) -> 実行時データ (set) -> 記録 (record) -> 解除 (clear_suspect) — and a write-time failure (rare: an IO/optimistic-lock error) stops before the next category, returning applied for whatever already landed on disk (this tool never attempts a cross-file rollback, E15: 'ファイルをまたぐトランザクションにはしない'). upsert_item {doc, id, title?, statement?, acceptance?: [{label,text}], attrs?: {layer,refines,verifies,priority,method,test,rationale,derived,\"waive-verify\",\"waive-refine\"}, after?}: rewrites one item in a layer document's body (§2.2 notation, rendered via the same src/storage/docs/layer_render.rs trace_scaffold uses). An existing item (matched by id in doc's current body) has only the parts actually given replaced — omitted title/statement/acceptance/attrs sub-keys keep the item's current value (attrs is merged key-by-key, not replaced as a whole block, so an existing rationale/derived/waiver/reserved assignee/needs attribute the op doesn't mention survives untouched); a non-existent id creates a new item after the 'after' id (or at the document's end when 'after' is omitted — an 'after' id that does not resolve to an item in the document's current body fails validation). Writing a new/changed derived or waive-verify/waive-refine value (including a brand-new item created with one already set, and including changing an existing reason) always adds a 'waiver_added: <id> <axis> <reason>' line to warnings (E4 — never a silent AI-side waiver, not even via creation). A non-existent id's prefix must still match this document's layer configuration (§2.2's grammar) — an id that cannot form a recognized item heading fails validation instead of silently writing a heading no reader will ever recognize as an item. Multiple upsert_item ops targeting different documents in the same call are batch-synced together (local sync in memory for every touched document, then cross-document link_baselines resolved against that same in-memory state — never a stale on-disk read) so a same-call upstream item's brand-new hash — not a stale on-disk one — becomes a same-call downstream item's new refines/verifies link_baselines entry (§2.5 step 4); each touched document is still written to disk exactly once, under its own optimistic-lock check. link/unlink {item, task?, role?}: adds/removes a requirement-type task_links entry via the same task-side-primary path handoff_update_task(requirement_ids=...) uses (docs::apply_requirement_diff_and_propagate — role, when given, must be 'implements' or 'executes'; omitted is inferred from the item's category, same as that path); item must resolve (pre-call corpus or an id upserted earlier in the same call) or the op fails validation. multiple ops for the same task are merged into one task-file read-modify-write. set {item, dev_stage?, approval?, impl_refs?, priority?, test_refs?}: writes an item's runtime fields directly — dev_stage/approval/impl_refs work on any item; priority (P0-P3 only) /test_refs only on a non-layer item (a layer item's priority/test_refs are body-owned, §2.3 — rejected with an error naming the guard). Writing approval=\"approved\" sets the item's SubItem.status to \"verified\" and stamps verified_at (§3.3, E12); approval=\"draft\" resets status to \"pending\" and clears verified_at. At least one of the five fields must be given. record {item, result, note?, evidence?}: every record op across the whole call is merged into exactly one runs/<id>.json file via handoff_trace_record (result must be one of pass/fail/blocked/not_run/skipped). clear_suspect {item?, upstream?, task_id?, layer?, result?, reason}: one of the wiki/260 §4.1 clear-target shapes ({item,upstream} one link, {item} all of that item's suspect links, {upstream} every link pointing at it, {task_id, item?} task link(s), {layer} every suspect in that layer, {result} that item's result suspect) plus reason (required) — delegates to handoff_trace_suspect(action=\"clear\") exactly once per clear_suspect op (one audit file per op, same .handoff/trace/clears/<id>.json convention). dry_run=true skips every write and returns the same applied shape as a preview: for upsert_item, result.diff is a unified-diff hunk (@@ -start,oldlen +start,newlen @@) scoped to exactly the lines that op would change (not a whole-document diff) — every other op kind's result previews its resolved arguments. Returns a JSON string {applied:[{op_index,op,result}],failed?:{op_index,error},warnings,suspect_introduced?} (dry_run response additionally sets dry_run:true) — suspect_introduced (upsert_item only, same shape doc_save's own field uses) is present whenever at least one upsert_item changed an item's def_hash. propose=true (wiki/270-vmodel-m3-design.md §4.3, M3-08, FR-407; mutually exclusive with dry_run) runs this same phase-1 validation and preview generation but, instead of discarding it (dry_run) or writing it to the document, persists it as a new pending handoff_trace_delta — only upsert_item/link/unlink/set ops are allowed in a propose call (record/clear_suspect are rejected before validation even starts, same restriction handoff_trace_delta(action=\"create\") enforces); description? is forwarded to the created delta. Returns {delta_id,ops_count,previews:[{op_index,diff}],warnings,propose:true} — use handoff_trace_delta(action=\"apply\"|\"reject\") to resolve it later.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "ops": {
                        "type": "array",
                        "description": "Non-empty array of ops, each with an 'op' field naming one of upsert_item/link/unlink/set/record/clear_suspect, plus that op's own arguments (see this tool's description).",
                        "items": { "type": "object" }
                    },
                    "task_id": { "type": "string", "description": "Default task for a link/unlink/record op that omits its own task/task_id." },
                    "dry_run": { "type": "boolean", "description": "Validate and preview every op (including a unified-diff hunk per upsert_item) without writing anything. Mutually exclusive with 'propose'.", "default": false },
                    "propose": { "type": "boolean", "description": "Run the same phase-1 validation and preview as dry_run, but persist the result as a new pending handoff_trace_delta (wiki/270-vmodel-m3-design.md §4.3, M3-08, FR-407) instead of discarding it or writing it to the document. Only upsert_item/link/unlink/set ops are allowed (record/clear_suspect are rejected up front). Mutually exclusive with 'dry_run'.", "default": false },
                    "description": { "type": "string", "description": "propose=true: free-form description of what this delta proposes (forwarded to the created delta)." },
                    "executor_kind": { "type": "string", "description": "Who/what is applying these ops.", "enum": ["ai", "human"], "default": "ai" },
                    "executor_id": { "type": "string", "description": "Identifier of the executor (e.g. an agent id)." },
                    "commit": { "type": "string", "description": "Commit used by record ops (passed through to handoff_trace_record)." }
                },
                "required": ["ops"]
            }),
        },
        ToolDefinition {
            name: "handoff_task_checklist".to_string(),
            description: "action=\"view\" (default, and only supported action — action=\"generate\" was removed at the M3 release; use handoff_trace_scaffold instead): pure-view aggregation of a task's done_criteria and its linked documents' verification matrices. No new data is written — reads task_links (link_type=\"doc\") and each linked document's verification matrix, computed fresh on every call. Returns {task_id,title,no_linked_docs:true,trace} as a fast-path response when the task has no linked documents. Otherwise returns {task_id,title,no_linked_docs:false,done_criteria:{items:[…],progress:{…}},verification_coverage:{documents:[{doc_id,slug,title,doc_type,items:[{fragment_seq,heading,status,stale,visual_state,impl_refs,test_refs}],progress:{…}}],overall:{…}},combined_readiness:{done_criteria_met,verification_complete,ready,blockers:[{type:\"criteria\"|\"verification\",…}]},suggested_actions:[…],trace}. trace (wiki/260-vmodel-m2-design.md §3.4/§4.11, M2-13) is {layers,blockers} from this task's requirement-type task_links (independent of the doc-type links above), or null when it has none — same shape and read-only (E6) contract as handoff_get_task's trace field. Each item's visual_state is computed in priority order stale > skipped > verified > implemented (pending+impl_refs+test_refs) > in_progress (pending+impl_refs only) > untouched.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": { "type": "string", "description": "Project directory path. Defaults to current working directory." },
                    "task_id": { "type": "string", "description": "Task ID to build the checklist for (e.g. 't1', 't1.2')." },
                    "action": { "type": "string", "description": "Checklist action.", "enum": ["view"], "default": "view" }
                },
                "required": ["task_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_claim_task".to_string(),
            description: "Claim a task for exclusive work, guarded by a cross-process file lock (flock). Fails if the task is already claimed by another agent with a non-expired lease; an expired lease is silently taken over. A todo/blocked task moves to in_progress. Returns the updated task JSON, including the new lock. If the claimed task's scope_paths overlap another active (in_progress, locked) task's scope_paths, an advisory 'warnings' array is included ({level:\"info\"|\"warn\", message}) — same-directory overlap is 'info', same-file overlap is 'warn'. Warnings never block the claim.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to claim (e.g. 't1', 't1.2')."
                    },
                    "session_id": {
                        "type": "string",
                        "description": "Session ID claiming the task, recorded on the lock for diagnostics."
                    },
                    "lease_ttl": {
                        "type": "integer",
                        "description": "Lease duration in seconds before the claim expires and can be taken over. Defaults to 1800 (30 minutes)."
                    }
                },
                "required": ["task_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_release_task".to_string(),
            description: "Release a task previously claimed by this agent, clearing its lock and reverting its status. Fails if the task's lock is held by a different agent.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to release."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional free-text reason for releasing, echoed back in the response."
                    },
                    "revert_status": {
                        "type": "string",
                        "description": "Status to revert the task to after releasing.",
                        "enum": ["todo", "in_progress", "review", "done", "blocked", "skipped"],
                        "default": "todo"
                    }
                },
                "required": ["task_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_list_agents".to_string(),
            description: "List registered agents across worktrees (`.handoff/agents/`), with freshly-computed active/stale/disconnected status based on heartbeat age.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "status": {
                        "type": "string",
                        "description": "Filter by agent status. Omit or use \"all\" for every agent.",
                        "enum": ["all", "active", "stale", "disconnected"]
                    },
                    "include_tasks": {
                        "type": "boolean",
                        "description": "Include each agent's claimed_tasks list in the response. Defaults to false."
                    }
                }
            }),
        },
        ToolDefinition {
            name: "handoff_overview".to_string(),
            description: "Cross-worktree overview of a single project's multi-agent state: registered agents, a task x agent claim matrix, and a worktree x branch x session mapping. Call from the primary worktree to monitor every worktree working the same project. Works with zero registered agents/claims (single-WT environments).".to_string(),
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
            name: "handoff_reclaim_task".to_string(),
            description: "Forcibly release a task's claim lease as a management operation, regardless of which agent holds it or whether the lease has expired (no ownership check, unlike handoff_release_task). Reverts the task to todo and records a task.reclaimed event in events.jsonl. Fails if the task has no active lock.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task ID to reclaim."
                    },
                    "reason": {
                        "type": "string",
                        "description": "Optional free-text reason for reclaiming, recorded in the events.jsonl entry and echoed back in the response."
                    }
                },
                "required": ["task_id"]
            }),
        },
        ToolDefinition {
            name: "handoff_events".to_string(),
            description: "Query .handoff/events.jsonl (lease/agent/session lifecycle events: task.claimed, task.released, task.expired, task.reclaimed, agent.registered, session.created, session.closed) with optional filters. Returns matching events oldest-first, most-recent-first when truncated by limit.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_dir": {
                        "type": "string",
                        "description": "Project directory path. Defaults to current working directory."
                    },
                    "since": {
                        "type": "string",
                        "description": "ISO 8601 timestamp; only events at or after this time are returned."
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Only events for this task."
                    },
                    "agent_id": {
                        "type": "string",
                        "description": "Only events for this agent."
                    },
                    "event_type": {
                        "type": "string",
                        "description": "Only events of this kind, e.g. \"task.claimed\"."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum number of events to return, keeping the most recent. Defaults to 100.",
                        "minimum": 1
                    }
                }
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

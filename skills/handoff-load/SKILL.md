---
name: handoff-load
description: "Load handoff context for the current project. Calls handoff_load_context, summarizes tasks/decisions/blockers, and shows what to work on next. Triggers on '/handoff-load', 'コンテキスト読み込み', 'what was I working on', 'resume work'."
user-invocable: true
---

# Handoff Load

Load the project's handoff context at session start and get oriented on the current state.

## Procedure

1. Call `handoff_load_context` (no arguments — it uses the cwd)
   - **The previous session (lineage) is inferred automatically.** When `previous_session.inferred: true` is returned (the basis is in `evidence`), use it as is. Do not pass `continue_from`. The next `handoff_save_context` that creates a new session records it into `continues` automatically
   - **Only** when `ambiguous: true` is returned (there is no `previous_session` or `next_actions`, and candidates are listed in `other_heads`): compare each candidate's `summary` with the user's request and the tasks you are about to work on, then call `handoff_load_context` again with the `id` of the matching candidate as `continue_from`. If you cannot decide, ask the user. A candidate with `in_flight_by` is already continued by another agent's active session (if that session is your own, take it over with `session_id` set to the ACTIVE session id listed in `in_flight_by`, not the candidate's head id, plus `adopt: true`)
2. If `not_initialized` is returned:
   - Infer the project name from the directory name
   - Initialize with `handoff_init`
   - Report that initialization is complete and stop
3. If there are **paused sessions**:
   - Show `paused_sessions` to the user (ID, summary, branch)
   - To resume one: `handoff_load_context(session_id: "s-...")` transitions paused → active
   - To discard one: `handoff_save_context(close_session_id: "s-...")` transitions paused → closed directly (a paused / open session owned by another agent can also be closed this way; a `session.foreign_close` event is recorded)
   - **Other agents' active / paused sessions** are never selected implicitly; they are listed in `adoptable_sessions` (ID, summary, branch, `updated_at`, no body). If a restart, `--continue`, or an MCP reconnect gave your session a different `agent_id`, take it over with `handoff_load_context(session_id: "s-...")` (an active session may be in use by another window, so `adopt: true` is required; taking it over moves ownership to you and records `session.adopted`)
   - Another agent's **active** session cannot be closed or paused even when specified by ID (it is rejected with an error and nothing is saved or changed). Only when you must close it anyway, add `force_other_owner: true` and a `reason`. Note that `/clear` does not close the handoff session: to close it, call `handoff_save_context` before `/clear`
   - When `session_guidance.action` is `adopt_or_create`, check whether the `candidates` are yours and take one over, or create a new one with `session_status: "active"`
4. **If `session_guidance` is present (no active session established yet)**:
   - Before starting work, call `handoff_save_context` with `session_status: "active"` to establish the session
   - Carry over and include the contents of `session_guidance.suggested_fields` (summary, decisions, context_pointers, references, related_task_ids)
   - Include at least one `suggestion` in `handoff_notes` (what you are about to do)
   - This keeps "what we were trying to do" in `.handoff/` even if the session is interrupted
5. Review and summarize the returned context in this order:
   - **Previous session / previous_session**: summary, branch, commit, ended_at. Check the handed-over information (decisions, handoff_notes, context_pointers)
   - **Tasks**: `task_tree` is **compact by default**: it lists only the tasks that belong to this session, and each one has a `scope_reason` explaining why (`active` = in_progress/blocked/review, `scope`, `previous_session`, `sibling`, `unblocked`, `branch`, `milestone`, `priority`). A node without `scope_reason` is only an ancestor kept so the tree stays connected. Show them in priority order `blocked` → `in_progress`/`review` → `todo`
     - `task_tree_meta` says what was left out (`omitted_total`, `omitted_by_status`, `omitted_without_milestone`, `truncated`) and `hint` says how to get it. `task_summary` always counts **every** task, so use it (not the length of `task_tree`) for totals
     - You never need to page through the omitted todos at session start. When you do need other tasks, call `handoff_list_tasks` (`status_filter`, `ids`, `limit`, `fields`); call `handoff_load_context(task_tree: "full")` only when you really need the whole tree
     - When the session works on specific tasks, pass them: `handoff_load_context(task_scope: ["t75", "t78"])` includes those tasks and their unfinished subtrees (`scope_reason: "scope"`) regardless of the limit. The number of other tasks is capped by `settings.compact_task_limit` (default 40)
     - `session_guidance.suggested_fields.related_task_ids` lists only work in flight, the previous session's unfinished tasks and your claimed tasks (at most 20; `related_task_ids_truncated: true` when more were found)
   - **Effort summary**: report total_estimate_hours, total_actual_hours, completion_rate, overdue_count
   - **Overdue**: if overdue_count > 0, highlight the affected tasks
   - **Blockers**: highlight them if any
   - **Decisions**: warn about any whose confidence is `unverified`
   - **Handoff notes**: `caution` first, then `context` → `suggestion`
   - **Context pointers**: the list of files to read next
6. **Check related documents**:
   - If an `in_progress` task has linked documents, the hook's
     `handoff_doc_query` (`UserPromptSubmit`/`PreToolUse`) injects them automatically
     — there is no need to call it explicitly at session start
   - To read only a specific section, call `handoff_doc_get(doc_id, format="fragment", seq=N)`
     manually (for example when only the heading list was injected as an outline)
   - For document management details (staged injection, lineage tree, import procedure)
     see the `handoff-docs` skill
7. Report the current-state summary to the user in the user's language
8. Propose the next action ("Where would you like to start?")

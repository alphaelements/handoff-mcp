use std::sync::{LazyLock, Mutex};

use serde_json::{json, Value};

use super::handlers::{handle_tool_call, resolve_project_dir, HandlerContext};
use super::tools::{all_resource_definitions, all_tool_definitions};
use super::types::{
    InitializeResult, JsonRpcResponse, ResourcesCapability, ServerCapabilities, ServerInfo,
    ToolsCapability, ToolsListResult, INTERNAL_ERROR, METHOD_NOT_FOUND, PROTOCOL_VERSION,
};

/// Process-wide agent identity, set once `handoff_load_context` registers
/// this process as an agent (t240.12). `None` until then, and for any
/// process (e.g. tests) that never calls `handoff_load_context`.
///
/// A single global is deliberate: one running MCP server process serves
/// exactly one agent identity for its whole lifetime, and every subsequent
/// tool call needs that identity threaded into its [`HandlerContext`]
/// without the caller having to resend it on every request.
static AGENT_ID: LazyLock<Mutex<Option<String>>> = LazyLock::new(|| Mutex::new(None));

/// Record `id` as this process's agent identity for all future
/// [`HandlerContext`]s built by [`build_handler_context`].
pub fn set_agent_id(id: String) {
    *AGENT_ID.lock().unwrap_or_else(|e| e.into_inner()) = Some(id);
}

/// The agent identity registered by a prior `handoff_load_context` call in
/// this process, if any.
pub fn get_agent_id() -> Option<String> {
    AGENT_ID.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Test-only: force this process's shared agent identity back to `None`.
///
/// Integration test binaries (`tests/*.rs`) run every `#[test]` fn as a
/// thread inside one shared process, so [`AGENT_ID`] — once *any* test in
/// that binary calls [`set_agent_id`] (directly, or indirectly through
/// `handoff_load_context`) — stays set for the rest of that process's
/// lifetime, including for tests that run afterward and never call
/// `handoff_load_context` themselves. Those later tests rely on the
/// pre-registration fallback (`agent_id: None` -> `UNKNOWN_IDENTITY`), which
/// the test runner's nondeterministic thread scheduling then only
/// *sometimes* provides — an intermittent failure that has nothing to do
/// with the behavior under test (t372).
///
/// A test relying on that fallback must call this while holding its file's
/// `AGENT_ID_GLOBAL` serialization lock (see the doc comment on that static
/// in `tests/tool_dashboard.rs` / `tests/tool_claim_release.rs`), so the
/// reset is both immune to whatever earlier tests left behind and race-free
/// against any test running concurrently that sets its own identity.
///
/// Has no effect on a real server process: `main.rs` never calls this, and a
/// live MCP server always serves exactly one agent identity for its whole
/// lifetime by design (see [`AGENT_ID`]'s doc comment).
pub fn reset_agent_id_for_test() {
    *AGENT_ID.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// P-M7 (wiki/240-performance-design.md §3 C9, §4): a process-wide mutex
/// serializing every write-classified tool call. `main.rs` runs each
/// JSON-RPC request on its own worker thread and only *waits* up to the
/// request timeout for a reply — the worker itself keeps running to
/// completion even after the client-visible timeout fires (§3 C9 "30 秒の
/// リクエストタイムアウト後も worker が走り続け、次のリクエストと並行に書き
/// 込みうる"). Without this mutex, a second write-classified request whose
/// worker starts while the first (possibly already "timed out" from the
/// caller's point of view) is still running could interleave its own
/// document/task read-modify-write with the first's, silently losing
/// whichever side wrote last. Acquiring this guard around
/// [`handle_tool_call`] for write-classified tools means the *next* such
/// request's handler body does not even start running until the previous
/// one's has fully finished — "次の書き込みを開始しない", not merely
/// "don't corrupt the file once both are running" (that half is
/// [`crate::storage::docs::DocSetConflict`]'s job, for the case where the
/// racing writer is a *different* process entirely, which this in-process
/// mutex cannot see).
///
/// Read-only tools ([`is_write_tool`] returns `false`) never touch this
/// mutex at all — wiki/240 §4 P-M7 "読み取り専用リクエストはミューテックス
/// で待たせない（性能）" — so concurrent reads keep running in parallel with
/// each other and are never blocked behind a write.
static WRITE_MUTEX: Mutex<()> = Mutex::new(());

/// Tool names that never mutate `.handoff/` state (verified by reading each
/// handler body — the file it lives in, and the exact line range of the
/// `handle_*` function `mod.rs` dispatches to for that tool name, per
/// `src/mcp/handlers/mod.rs`'s `handle_tool_call`). Every tool name **not**
/// in this list is treated as a write by [`is_write_tool`] — a fail-safe
/// default (wiki/240 §4 P-M7), since a newly added tool that forgets to
/// register itself here ends up over-protected (serialized behind the write
/// mutex) rather than under-protected (racing document/task RMW).
///
/// A few "read"-shaped tools have a narrow, non-authoritative *cache* side
/// effect (`_requirements_summary.json` for `handoff_doc_req_status`, the
/// injected-docs dedup set for `handoff_doc_query`) and are still listed
/// here deliberately: they are the read-only-tool-with-a-derived-file-write
/// case wiki/240 §3 C4 itself names, the written file is a recomputable
/// cache rather than a document/task source of truth, and both are
/// hook-driven/high-frequency enough that serializing them behind every
/// other write would defeat P-M7's "don't make reads wait" goal. Tools that
/// mutate real document/task content even conditionally (`handoff_doc_scan`
/// with `apply=true`-style options, `handoff_doc_analyze`'s split mode,
/// `handoff_load_context`'s agent-record registration) are deliberately
/// **not** listed, even though their most common call shape is read-like.
///
/// N3 (t360.43 M1 review) re-examined this list for *every* write a listed
/// tool can reach, not just the two called out above.
///
/// `handoff_doc_req_status` (re-examined, decision unchanged): the
/// `_requirements_summary.json` refresh above is the *only* write it can
/// ever reach — covered by the paragraph above, kept read-classified.
///
/// `handoff_doc_get` and, transitively, every other listed tool that reads a
/// document (`handoff_doc_list`, `handoff_doc_reassemble`, `handoff_doc_tree`,
/// `handoff_doc_graph`, `handoff_doc_trace`, `handoff_doc_verify_status`,
/// `handoff_doc_query`, `handoff_doc_req_list`, `handoff_doc_req_impact`):
/// `crate::storage::docs::read_doc`/`read_all_docs` transparently migrate an
/// old-format `_doc.<slug>.json` + `_doc.<slug>.md` sidecar pair to the
/// current single-file frontmatter format on first read
/// (`migrate_legacy_doc`, t123.3) — a genuine, unconditional filesystem write
/// triggered from a "read" call whenever it happens to be the first one to
/// touch a not-yet-migrated document.
///
/// Decision: kept read-classified, not moved behind the write mutex.
/// Reasoning: first, every project created since t123.3 shipped never has a
/// `.json` sidecar at all, so this write path is already unreachable for the
/// overwhelmingly common case — reclassifying the entire doc-reading surface
/// (the busiest read paths in this server) behind the write mutex to protect
/// a legacy-format compatibility shim would trade a real, constant
/// performance cost (every read now waits behind every write, defeating
/// P-M7 for its primary use case) against a one-time, self-healing migration
/// that only exists for pre-t123.3 projects. Second, the migration write
/// itself goes through `write_doc_with_body` (`crate::storage::atomic_write`,
/// rename-based), so it can never leave a torn/partial file behind, and it
/// is idempotent per-document — once migrated, the `.json` sidecar is gone
/// and no later read of that document can trigger it again.
///
/// The residual risk this does *not* close — two literally-concurrent reads
/// of the exact same not-yet-migrated document racing `migrate_legacy_doc`'s
/// own read of the `.json` sidecar against the first reader's deletion of it
/// — is real but narrow enough (legacy-format project *and* a same-instant
/// double-read of the same unmigrated slug) that this task treats it as an
/// accepted, documented gap rather than a blocker; see this task's dev
/// report's "Discovered issues" for the narrowly-scoped follow-up this
/// implies for `migrate_legacy_doc` itself (treat "the `.json` sidecar
/// disappeared between my check and my read" as "already migrated, re-read
/// the `.md`" rather than propagating the resulting I/O error).
const READ_ONLY_TOOLS: &[&str] = &[
    "handoff_get_task",
    "handoff_list_tasks",
    "handoff_get_config",
    "handoff_get_metrics",
    "handoff_list_sessions",
    "handoff_list_assignees",
    "handoff_get_session",
    "handoff_get_capacity",
    "handoff_list_milestones",
    "handoff_memory_query",
    "handoff_timer_get_time",
    "handoff_doc_get",
    "handoff_doc_list",
    "handoff_doc_reassemble",
    "handoff_doc_tree",
    "handoff_doc_graph",
    "handoff_doc_trace",
    "handoff_doc_verify_status",
    "handoff_doc_query",
    "handoff_doc_req_list",
    "handoff_doc_req_status",
    "handoff_doc_req_impact",
    "handoff_list_agents",
    "handoff_overview",
    "handoff_events",
    "handoff_list_referrals",
    "handoff_get_referral",
    // t360.13: pure read over `.handoff/runs/`, no derived-file or other
    // write of any kind (unlike `handoff_trace_report`/`handoff_trace_slice`,
    // which are write-classified for their layer-doc-resync/self-repair side
    // effect and, for `handoff_trace_report`, its `_trace_report.json`
    // write).
    "handoff_trace_history",
    // M2-06 (wiki/260-vmodel-m2-design.md §4.2/E6): pure "what would happen
    // if..." analysis over an already-loaded `TraceInput`. Unlike
    // `handoff_trace_suspect` (not listed here because its `clear`/
    // `baseline(apply)` actions genuinely write), every `handoff_trace_impact`
    // path is read-only, so — rework round 2 fix — it uses
    // `trace::load_trace_input_read_only`, not plain `load_trace_input`: the
    // latter's `runs::sync` call can itself write `runs/_latest.json` (and
    // `.handoff/.gitignore`) on the cache's first materialization, which
    // would be a real write running outside `WRITE_MUTEX` if this tool used
    // it. `load_trace_input_read_only` does a plain, non-reconciling read of
    // `runs/_latest.json` instead (see its doc comment in
    // `src/mcp/handlers/trace.rs`), so this tool never calls
    // `resync_direct_edited_layer_docs`, `runs::sync`, or writes any derived
    // file at all.
    "handoff_trace_impact",
    // M2-17 (wiki/260-vmodel-m2-design.md §4.10/E6): existing-item similarity
    // + a new-item template proposal, over already-persisted document/task
    // metadata only — no `runs::sync`, no layer re-sync, and (unlike
    // `handoff_trace_scaffold`) no write path at all, so it needs no
    // `mode`/`dry_run` split the way that tool does. Creation is deliberately
    // out of scope (§4.10: "作成は利用者の確認後に…で行う").
    "handoff_trace_propose",
];

/// `true` for any tool name not in [`READ_ONLY_TOOLS`] — see that constant's
/// doc comment for the fail-safe-default rationale.
pub(crate) fn is_write_tool(name: &str) -> bool {
    !READ_ONLY_TOOLS.contains(&name)
}

/// Acquires the process-wide [`WRITE_MUTEX`] for the duration of a
/// write-classified tool call, or `None` for a read-classified one (which
/// must never block behind it). Exposed at `pub(crate)` visibility purely so
/// `mod tests` below can exercise the primitive directly and
/// deterministically (via threads + a `Barrier`) without needing to fabricate
/// a real slow handler — see
/// `write_mutex_guard_serializes_writes_and_never_blocks_reads`.
pub(crate) fn write_mutex_guard(name: &str) -> Option<std::sync::MutexGuard<'static, ()>> {
    if is_write_tool(name) {
        Some(WRITE_MUTEX.lock().unwrap_or_else(|e| e.into_inner()))
    } else {
        None
    }
}

pub fn handle_request(method: &str, params: Option<&Value>) -> JsonRpcResponse {
    match method {
        "initialize" => handle_initialize(),
        "notifications/initialized" => JsonRpcResponse {
            jsonrpc: "2.0".to_string(),
            id: None,
            result: None,
            error: None,
        },
        "tools/list" => handle_tools_list(),
        "tools/call" => handle_tools_call(params),
        "resources/list" => handle_resources_list(),
        "resources/read" => handle_resources_read(params),
        _ => JsonRpcResponse::error(
            None,
            METHOD_NOT_FOUND,
            format!("Method not found: {method}"),
        ),
    }
}

fn handle_initialize() -> JsonRpcResponse {
    let result = InitializeResult {
        protocol_version: PROTOCOL_VERSION.to_string(),
        capabilities: ServerCapabilities {
            tools: Some(ToolsCapability {
                list_changed: false,
            }),
            resources: Some(ResourcesCapability {}),
        },
        server_info: ServerInfo {
            name: "handoff-mcp".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
        },
        instructions: Some(
            "Handoff MCP server for AI session context persistence. \
             Call handoff_load_context at session start, \
             handoff_save_context at session end.\n\n\
             ## Session Start\n\
             1. Call handoff_load_context (no args needed — uses cwd)\n\
             2. If it returns \"not initialized\", call handoff_init with the project name\n\
             3. If session_guidance is present, call handoff_save_context with session_status='active' to establish a persistent session before starting work\n\
             4. Check the `next_actions` array first — these are the previous session's recommended next steps. Do not re-verify work the previous session already completed\n\n\
             ## During Work — Progressive Updates\n\
             - Use handoff_update_task to create/update tasks as work progresses\n\
             - Mark tasks in_progress when starting, done when complete\n\
             - Use handoff_check_criterion to check off task done_criteria as each item is verified — do not wait until the task is fully done\n\
             - Use handoff_update_session to progressively update the active session: toggle checklist items, append decisions, notes, or context pointers\n\
             - When work reaches a point requiring user confirmation, set the task status to review\n\
             - Record decisions as they are made, not just at session end\n\n\
             ## Session End\n\
             1. Call handoff_save_context with:\n\
                - summary: one-line description of what was accomplished\n\
                - decisions: key decisions made (with reason and confidence)\n\
                - blockers: anything preventing progress\n\
                - handoff_notes: caution/context/suggestion for the next session\n\
                - context_pointers: files and line ranges the next session should look at"
                .to_string(),
        ),
    };
    match serde_json::to_value(result) {
        Ok(value) => JsonRpcResponse::success(None, value),
        Err(e) => JsonRpcResponse::error(None, INTERNAL_ERROR, format!("Serialization error: {e}")),
    }
}

fn handle_tools_list() -> JsonRpcResponse {
    let result = ToolsListResult {
        tools: all_tool_definitions(),
    };
    match serde_json::to_value(result) {
        Ok(value) => JsonRpcResponse::success(None, value),
        Err(e) => JsonRpcResponse::error(None, INTERNAL_ERROR, format!("Serialization error: {e}")),
    }
}

fn handle_tools_call(params: Option<&Value>) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => {
            return JsonRpcResponse::error(
                None,
                super::types::INVALID_REQUEST,
                "tools/call requires params",
            );
        }
    };

    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return JsonRpcResponse::error(
                None,
                super::types::INVALID_REQUEST,
                "tools/call requires 'name' parameter",
            );
        }
    };

    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));

    let ctx = match build_handler_context(name, &arguments) {
        Ok(ctx) => ctx,
        Err(e) => {
            let tool_result = json!({
                "isError": true,
                "content": [{
                    "type": "text",
                    "text": format!("Error: {e}")
                }]
            });
            return JsonRpcResponse::success(None, tool_result);
        }
    };

    // P-M7 (wiki/240 §4): held for the whole handler call when `name` is
    // write-classified; `None` (no blocking at all) for a read. See
    // `write_mutex_guard`'s doc comment for why this has to wrap the handler
    // call itself, not just the eventual file write inside it.
    let _write_guard = write_mutex_guard(name);
    handle_tool_call(&ctx, name, &arguments)
}

/// Resolve `project_dir` and (for every tool except `handoff_init` /
/// `handoff_load_context`, which must tolerate a project with no
/// `.handoff/` yet) verify `.handoff/` exists, producing the shared
/// `HandlerContext` passed to every handler.
///
/// `agent_id` is populated from the process-wide identity set by a prior
/// `handoff_load_context` call (see [`set_agent_id`]); it stays `None` until
/// then.
fn build_handler_context(name: &str, arguments: &Value) -> anyhow::Result<HandlerContext> {
    let project_dir = resolve_project_dir(arguments)?;

    let handoff_dir = if matches!(name, "handoff_init" | "handoff_load_context") {
        crate::storage::handoff_dir(&project_dir)
    } else {
        crate::storage::ensure_handoff_exists(&project_dir)?
    };

    Ok(HandlerContext {
        agent_id: get_agent_id(),
        project_dir,
        handoff_dir,
    })
}

fn handle_resources_list() -> JsonRpcResponse {
    let resources = all_resource_definitions();
    let result = json!({ "resources": resources });
    JsonRpcResponse::success(None, result)
}

fn handle_resources_read(params: Option<&Value>) -> JsonRpcResponse {
    let params = match params {
        Some(p) => p,
        None => {
            return JsonRpcResponse::error(
                None,
                super::types::INVALID_REQUEST,
                "resources/read requires params",
            );
        }
    };

    let uri = match params.get("uri").and_then(|v| v.as_str()) {
        Some(u) => u,
        None => {
            return JsonRpcResponse::error(
                None,
                super::types::INVALID_REQUEST,
                "resources/read requires 'uri' parameter",
            );
        }
    };

    match super::resources::handle_resource_read(uri) {
        Ok(result) => JsonRpcResponse::success(None, result),
        Err(e) => JsonRpcResponse::error(
            None,
            super::types::INVALID_REQUEST,
            format!("Resource error: {e}"),
        ),
    }
}

#[cfg(test)]
mod write_mutex_tests {
    use super::*;
    use std::sync::{mpsc, Arc};
    use std::thread;

    /// P-M7 sanity check on the classification table itself: a couple of
    /// unmistakably-read and unmistakably-write tool names, so a future edit
    /// to `READ_ONLY_TOOLS` that accidentally flips one direction fails
    /// loudly here rather than only showing up as a latency regression.
    #[test]
    fn is_write_tool_classifies_known_read_and_write_tools_correctly() {
        assert!(
            !is_write_tool("handoff_doc_list"),
            "handoff_doc_list must be read-only"
        );
        assert!(
            !is_write_tool("handoff_doc_req_status"),
            "handoff_doc_req_status must be read-only (C4: its cache write is conditional/derived)"
        );
        assert!(
            is_write_tool("handoff_update_task"),
            "handoff_update_task must be write-classified"
        );
        assert!(
            is_write_tool("handoff_doc_save"),
            "handoff_doc_save must be write-classified"
        );
        assert!(
            is_write_tool("a_brand_new_tool_nobody_registered_yet"),
            "an unrecognized tool name must default to write-classified (fail safe)"
        );
    }

    /// P-M7 (wiki/240 §4): a read-classified tool call must never block
    /// behind a write-classified one holding the process-wide write mutex —
    /// proven deterministically via channels (no sleeps/polling): the test
    /// only calls `write_mutex_guard` for the read tool once it has
    /// positively confirmed (via `acquired_rx`) that the write-holding
    /// thread already has the guard.
    #[test]
    fn write_mutex_guard_never_blocks_a_read() {
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (acquired_tx, acquired_rx) = mpsc::channel::<()>();

        let writer = thread::spawn(move || {
            let _guard =
                write_mutex_guard("handoff_update_task").expect("write tool must get a guard");
            acquired_tx.send(()).unwrap();
            // Held until the main test thread says so, well past the point
            // where it has already exercised the read-tool assertion below.
            release_rx.recv().unwrap();
        });

        acquired_rx
            .recv()
            .expect("writer thread must confirm it holds the guard");

        assert!(
            write_mutex_guard("handoff_doc_list").is_none(),
            "a read-classified tool must never be handed a write guard, and must not block \
             waiting for one that a concurrent write holds"
        );

        release_tx.send(()).unwrap();
        writer.join().unwrap();
    }

    /// P-M7 (wiki/240 §3 C9, §4): two write-classified calls must be fully
    /// serialized — the second's critical section cannot start until the
    /// first's has ended. Deterministic: `b` is only spawned after `a_start_rx`
    /// confirms thread `a` already holds the guard, so `b`'s `write_mutex_guard`
    /// call is guaranteed (by `Mutex`'s own blocking semantics, not by timing)
    /// to block until `a` releases — the recorded `order` can only ever come
    /// out `["a-enter", "a-exit", "b-enter"]`, never interleaved.
    #[test]
    fn write_mutex_guard_serializes_two_concurrent_writes() {
        let order: Arc<std::sync::Mutex<Vec<&'static str>>> =
            Arc::new(std::sync::Mutex::new(Vec::new()));
        let (release_tx, release_rx) = mpsc::channel::<()>();
        let (a_start_tx, a_start_rx) = mpsc::channel::<()>();

        let order_a = Arc::clone(&order);
        let a = thread::spawn(move || {
            let _guard = write_mutex_guard("handoff_update_task").unwrap();
            order_a.lock().unwrap().push("a-enter");
            a_start_tx.send(()).unwrap();
            // Simulates the "worker keeps running past the client-visible
            // 30s timeout" scenario (wiki/240 §3 C9): held until the test
            // explicitly releases it.
            release_rx.recv().unwrap();
            order_a.lock().unwrap().push("a-exit");
        });

        a_start_rx
            .recv()
            .expect("thread a must confirm it holds the guard before b is spawned");

        let order_b = Arc::clone(&order);
        let b = thread::spawn(move || {
            let _guard = write_mutex_guard("handoff_doc_save").unwrap();
            order_b.lock().unwrap().push("b-enter");
        });

        release_tx.send(()).unwrap();
        a.join().unwrap();
        b.join().unwrap();

        assert_eq!(*order.lock().unwrap(), vec!["a-enter", "a-exit", "b-enter"]);
    }
}

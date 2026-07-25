//! Integration tests for the Codex CLI memory-injection hook
//! (`plugin-hooks/codex/handoff-memory-inject.sh`).
//!
//! The hook runs on every Codex turn, so its contract is strict: it must ALWAYS
//! exit 0 and ALWAYS print valid JSON on stdout. A non-zero exit or malformed
//! stdout turns a missing memory into a broken turn for the user.
//!
//! These tests drive the real script as a subprocess with real payloads.

use std::path::PathBuf;
use std::process::{Command, Stdio};

fn binary() -> PathBuf {
    let mut path = std::env::current_exe()
        .expect("current_exe")
        .parent()
        .expect("parent")
        .parent()
        .expect("parent")
        .to_path_buf();
    path.push("handoff-mcp");
    path
}

fn hook_script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("plugin-hooks")
        .join("codex")
        .join("handoff-memory-inject.sh")
}

/// Run the hook with `payload` on stdin. Returns (stdout, stderr, exit code).
fn run_hook(payload: &str) -> (String, String, i32) {
    use std::io::Write;

    let mut child = Command::new(hook_script())
        // Point the hook at the freshly built binary rather than whatever
        // handoff-mcp happens to be on the developer's PATH.
        .env("HANDOFF_MCP_BIN", binary())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn hook script");

    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write payload");

    let out = child.wait_with_output().expect("wait for hook");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

fn parse_json(s: &str) -> serde_json::Value {
    serde_json::from_str(s)
        .unwrap_or_else(|e| panic!("hook stdout was not valid JSON: {e}\nstdout was: {s:?}"))
}

fn init_project(dir: &std::path::Path) {
    let status = Command::new(binary())
        .args([
            "init",
            "--project-dir",
            dir.to_str().unwrap(),
            "--project-name",
            "CodexHookTest",
        ])
        .output()
        .expect("run init");
    assert!(status.status.success(), "init failed");
}

fn save_memory(dir: &std::path::Path, text: &str) {
    let out = Command::new(binary())
        .args([
            "memory",
            "save",
            "--project-dir",
            dir.to_str().unwrap(),
            "--text",
            text,
            "--kind",
            "gotcha",
        ])
        .output()
        .expect("run memory save");
    assert!(
        out.status.success(),
        "memory save failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// --- fail-safe contract ------------------------------------------------------

#[test]
fn empty_stdin_yields_empty_json_and_exit_zero() {
    let (stdout, _, code) = run_hook("");
    assert_eq!(code, 0, "hook must exit 0 on empty stdin");
    assert_eq!(parse_json(stdout.trim()), serde_json::json!({}));
}

#[test]
fn malformed_payload_yields_empty_json_and_exit_zero() {
    let (stdout, stderr, code) = run_hook("this is not json at all ]]}");
    assert_eq!(code, 0, "hook must exit 0 on malformed payload");
    assert_eq!(parse_json(stdout.trim()), serde_json::json!({}));
    // Parser noise must not leak into the user's Codex session.
    assert!(
        stderr.is_empty(),
        "hook must not write to stderr, got: {stderr:?}"
    );
}

#[test]
fn project_without_handoff_dir_injects_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let payload = format!(
        r#"{{"cwd":"{}","prompt":"anything","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );
    let (stdout, _, code) = run_hook(&payload);
    assert_eq!(code, 0);
    assert_eq!(
        parse_json(stdout.trim()),
        serde_json::json!({}),
        "a project with no .handoff/ must inject nothing, not error"
    );
}

#[test]
fn initialized_project_with_no_memories_injects_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());

    let payload = format!(
        r#"{{"cwd":"{}","prompt":"some unrelated question","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );
    let (stdout, _, code) = run_hook(&payload);
    assert_eq!(code, 0);
    assert_eq!(parse_json(stdout.trim()), serde_json::json!({}));
}

// --- injection behavior ------------------------------------------------------

#[test]
fn matching_memory_is_injected_as_additional_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());
    save_memory(
        dir.path(),
        "The deploy pipeline requires ZEBRA_TOKEN to be exported before running make release.",
    );

    // Memories saved without explicit keywords are matched on term overlap, so
    // the prompt has to actually share vocabulary with the memory to clear the
    // relevance threshold — a vaguely related prompt injects nothing by design.
    let payload = format!(
        r#"{{"cwd":"{}","prompt":"ZEBRA_TOKEN deploy pipeline make release","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );
    let (stdout, _, code) = run_hook(&payload);
    assert_eq!(code, 0);

    let v = parse_json(stdout.trim());
    let ctx = v
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("expected additionalContext, got: {v}"));

    assert!(
        ctx.contains("ZEBRA_TOKEN"),
        "injected context must carry the memory text, got: {ctx}"
    );
    assert_eq!(
        v.pointer("/hookSpecificOutput/hookEventName")
            .and_then(|e| e.as_str()),
        Some("UserPromptSubmit"),
        "Codex requires the event name to be echoed back verbatim"
    );
}

#[test]
fn injection_respects_memory_limit() {
    use std::io::Write;

    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());

    // Distinct wording on purpose: near-duplicate memories are deduplicated by
    // the store, so near-identical fixtures would collapse to a single result
    // and the cap could never be exceeded — making this test unfalsifiable.
    save_memory(
        dir.path(),
        "ZEBRA_TOKEN must be exported before make release; the CI runner masks it in logs.",
    );
    save_memory(
        dir.path(),
        "ZEBRA_TOKEN rotation happens quarterly and requires updating the vault entry first.",
    );
    save_memory(
        dir.path(),
        "ZEBRA_TOKEN scope must include write:packages or the release upload silently 403s.",
    );

    // Verified to match all three memories without a limit.
    let payload = format!(
        r#"{{"cwd":"{}","prompt":"ZEBRA_TOKEN rotation quarterly vault release make CI scope packages upload","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );

    let mut child = Command::new(hook_script())
        .env("HANDOFF_MCP_BIN", binary())
        .env("HANDOFF_MEMORY_LIMIT", "2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    assert_eq!(out.status.code(), Some(0));

    let v = parse_json(String::from_utf8_lossy(&out.stdout).trim());
    let ctx = v
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("expected additionalContext, got: {v}"));

    // Each memory is rendered as exactly one "- (kind) text" bullet.
    let bullets = ctx.matches("- (").count();
    assert_eq!(
        bullets, 2,
        "HANDOFF_MEMORY_LIMIT=2 must cap injection at 2 of the 3 matching \
         memories, got {bullets} in: {ctx}"
    );
}

#[test]
fn missing_binary_still_exits_zero_with_valid_json() {
    use std::io::Write;

    // The single most likely real-world failure: the hook is installed but
    // `handoff-mcp` is not on PATH. Codex treats a non-zero hook exit as a
    // failed hook, so this must still be a clean no-op.
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());

    let payload = format!(
        r#"{{"cwd":"{}","prompt":"anything","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );

    let mut child = Command::new(hook_script())
        .env("HANDOFF_MCP_BIN", "handoff-mcp-does-not-exist-xyz")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");

    assert_eq!(
        out.status.code(),
        Some(0),
        "a missing binary must not fail the hook"
    );
    assert_eq!(
        parse_json(String::from_utf8_lossy(&out.stdout).trim()),
        serde_json::json!({})
    );
}

#[test]
fn query_failure_still_exits_zero_with_valid_json() {
    use std::io::Write;

    // A binary that exists but errors out (corrupt store, bad flags, panic)
    // must degrade to an empty injection rather than breaking the turn.
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());

    let failing = dir.path().join("failing.sh");
    std::fs::write(&failing, "#!/bin/sh\necho 'boom' >&2\nexit 3\n").expect("write stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&failing, std::fs::Permissions::from_mode(0o755))
            .expect("chmod stub");
    }

    let payload = format!(
        r#"{{"cwd":"{}","prompt":"anything","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );

    let mut child = Command::new(hook_script())
        .env("HANDOFF_MCP_BIN", &failing)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");

    assert_eq!(
        out.status.code(),
        Some(0),
        "a failing query must not fail the hook"
    );
    assert_eq!(
        parse_json(String::from_utf8_lossy(&out.stdout).trim()),
        serde_json::json!({})
    );
    assert!(
        String::from_utf8_lossy(&out.stderr).is_empty(),
        "the failing binary's stderr must not leak into the Codex session"
    );
}

#[test]
fn hung_query_is_bounded_by_internal_timeout() {
    use std::io::Write;
    use std::time::Instant;

    // Codex enforces `timeoutSec` on the hook, but the script caps the query
    // itself so a wedged store cannot stall the turn for the full budget.
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());

    let hang = dir.path().join("hang.sh");
    std::fs::write(&hang, "#!/bin/sh\nsleep 60\n").expect("write hang stub");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&hang, std::fs::Permissions::from_mode(0o755))
            .expect("chmod hang stub");
    }

    let payload = format!(
        r#"{{"cwd":"{}","prompt":"anything","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );

    let started = Instant::now();
    let mut child = Command::new(hook_script())
        .env("HANDOFF_MCP_BIN", &hang)
        .env("HANDOFF_MEMORY_TIMEOUT", "2")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    let elapsed = started.elapsed();

    assert_eq!(out.status.code(), Some(0), "a hung query must still exit 0");
    assert_eq!(
        parse_json(String::from_utf8_lossy(&out.stdout).trim()),
        serde_json::json!({}),
        "a hung query must degrade to an empty injection"
    );
    assert!(
        elapsed.as_secs() < 30,
        "HANDOFF_MEMORY_TIMEOUT=2 must bound the query, took {elapsed:?}"
    );
}

#[test]
fn dash_prefixed_prompt_still_injects_memories() {
    // A prompt beginning with `--` ("--force is not working", a pasted flag or
    // diff) must not be swallowed by CLI flag parsing. This silently dropped
    // ALL memory injection before `--text -- "$prompt"` was used.
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());
    save_memory(
        dir.path(),
        "ZEBRA_TOKEN must be exported before make release; the CI runner masks it in logs.",
    );

    let payload = format!(
        r#"{{"cwd":"{}","prompt":"--ZEBRA_TOKEN must be exported before make release CI runner masks logs","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );
    let (stdout, _, code) = run_hook(&payload);
    assert_eq!(code, 0);

    let v = parse_json(stdout.trim());
    let ctx = v
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("a `--`-prefixed prompt must still match memories, got: {v}"));
    assert!(ctx.contains("ZEBRA_TOKEN"), "got: {ctx}");
}

/// Run the hook with `jq` removed from `PATH`, exercising the sed + python3
/// fallback branches that are otherwise never covered.
fn run_hook_without_jq(payload: &str) -> (String, String, i32) {
    use std::io::Write;

    let jq_dir = Command::new("sh")
        .args(["-c", "command -v jq"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| {
            PathBuf::from(String::from_utf8_lossy(&o.stdout).trim())
                .parent()
                .map(|p| p.to_path_buf())
                .unwrap_or_default()
        });

    let path = std::env::var("PATH").unwrap_or_default();
    let stripped = match jq_dir {
        Some(d) => std::env::split_paths(&path)
            .filter(|p| p != &d)
            .collect::<Vec<_>>(),
        None => std::env::split_paths(&path).collect(),
    };
    let new_path = std::env::join_paths(stripped).expect("join PATH");

    let mut child = Command::new(hook_script())
        .env("HANDOFF_MCP_BIN", binary())
        .env("PATH", &new_path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn hook");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(payload.as_bytes())
        .expect("write");
    let out = child.wait_with_output().expect("wait");
    (
        String::from_utf8_lossy(&out.stdout).to_string(),
        String::from_utf8_lossy(&out.stderr).to_string(),
        out.status.code().unwrap_or(-1),
    )
}

#[test]
fn sed_fallback_injects_when_jq_is_absent() {
    let dir = tempfile::tempdir().expect("tempdir");
    init_project(dir.path());
    save_memory(
        dir.path(),
        "ZEBRA_TOKEN must be exported before make release; the CI runner masks it in logs.",
    );

    // `prompt` deliberately precedes `cwd`: a greedy regex swallows every
    // trailing key into the prompt and poisons the query.
    let payload = format!(
        r#"{{"prompt":"ZEBRA_TOKEN must be exported before make release CI runner masks logs","cwd":"{}","hook_event_name":"UserPromptSubmit"}}"#,
        dir.path().display()
    );
    let (stdout, stderr, code) = run_hook_without_jq(&payload);
    assert_eq!(code, 0, "fallback path must exit 0");
    assert!(stderr.is_empty(), "fallback must be quiet, got: {stderr}");

    let v = parse_json(stdout.trim());
    let ctx = v
        .pointer("/hookSpecificOutput/additionalContext")
        .and_then(|c| c.as_str())
        .unwrap_or_else(|| panic!("sed/python3 fallback must still inject, got: {v}"));
    assert!(
        ctx.contains("ZEBRA_TOKEN"),
        "fallback lost the memory text: {ctx}"
    );
    assert!(
        !ctx.contains("hook_event_name"),
        "greedy prompt regex leaked trailing JSON keys into the query: {ctx}"
    );
}

// --- shipped config ----------------------------------------------------------

#[test]
fn shipped_hooks_json_matches_codex_schema() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("plugin-hooks")
        .join("codex")
        .join("hooks.json");
    let raw = std::fs::read_to_string(&path).expect("read codex hooks.json");
    let v: serde_json::Value = serde_json::from_str(&raw).expect("hooks.json must be valid JSON");

    // Codex 0.145.x: PascalCase event names, each mapping to an array of
    // matcher groups, each group holding its own `hooks` array.
    let entry = v
        .pointer("/hooks/UserPromptSubmit/0/hooks/0")
        .unwrap_or_else(|| panic!("expected /hooks/UserPromptSubmit/0/hooks/0 in {v}"));

    assert_eq!(
        entry.get("type").and_then(|t| t.as_str()),
        Some("command"),
        "Codex only supports command hooks — prompt/agent handlers are stubbed out"
    );
    assert!(
        entry.get("command").and_then(|c| c.as_str()).is_some(),
        "command must be a string (Codex shells it via $SHELL -lc), not an array"
    );
    assert!(
        entry.get("timeoutSec").is_some(),
        "handler fields are camelCase in the config file"
    );
}

#[test]
fn shipped_hooks_json_uses_no_unexpanded_placeholder() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("plugin-hooks")
        .join("codex")
        .join("hooks.json");
    let raw = std::fs::read_to_string(&path).expect("read codex hooks.json");

    // Codex has no plugin-root placeholder; ${CODEX_PLUGIN_ROOT} does not
    // expand and makes the hook fail at runtime. Verified against 0.145.0.
    assert!(
        !raw.contains("${CODEX_PLUGIN_ROOT}"),
        "hooks.json must not rely on ${{CODEX_PLUGIN_ROOT}} — it does not expand"
    );

    // The shipped value is a placeholder the user must replace. Guard that it
    // stays an obvious one rather than drifting into a real-looking path that
    // would fail silently on someone else's machine.
    let v: serde_json::Value = serde_json::from_str(&raw).expect("valid JSON");
    let cmd = v
        .pointer("/hooks/UserPromptSubmit/0/hooks/0/command")
        .and_then(|c| c.as_str())
        .expect("command string");
    assert!(
        cmd.starts_with('/'),
        "the documented command must be an absolute path, got: {cmd}"
    );
    assert!(
        cmd.contains("/ABSOLUTE/PATH/TO/"),
        "the shipped command must remain an obvious placeholder, got: {cmd}"
    );
}

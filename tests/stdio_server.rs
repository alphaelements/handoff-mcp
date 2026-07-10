//! Real-binary E2E tests for the stdio JSON-RPC server loop (`src/main.rs`).
//!
//! These tests spawn the actual `handoff-mcp` binary (no args => server mode),
//! write line-delimited JSON-RPC requests to its stdin, and read responses from
//! stdout — exercising the real transport instead of calling `process_line`
//! in-process. This covers the worker-thread + `recv_timeout` behavior added
//! for t71 (stdio concurrency fix): sequential correctness, notification
//! handling, malformed input, and the timeout fail-safe path.

use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

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

struct Server {
    child: Child,
    stdin: std::process::ChildStdin,
    /// Response lines forwarded by a dedicated reader thread that owns
    /// stdout for the lifetime of the server, so `read_line` can apply a
    /// `recv_timeout` without needing an interruptible pipe read.
    lines: Receiver<String>,
}

impl Server {
    fn spawn() -> Self {
        Self::spawn_with_timeout(None)
    }

    /// Spawn the server, optionally overriding the request timeout (seconds)
    /// via the `HANDOFF_MCP_REQUEST_TIMEOUT_SECS` env var so timeout-path
    /// tests don't need to wait on the production 30s default.
    fn spawn_with_timeout(timeout_secs: Option<u64>) -> Self {
        let mut cmd = Command::new(binary());
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        if let Some(secs) = timeout_secs {
            cmd.env("HANDOFF_MCP_REQUEST_TIMEOUT_SECS", secs.to_string());
        }
        let mut child = cmd.spawn().expect("failed to spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");

        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = String::new();
                match reader.read_line(&mut buf) {
                    Ok(0) => break, // EOF: child exited
                    Ok(_) => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            break; // receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Server {
            child,
            stdin,
            lines: rx,
        }
    }

    fn send_line(&mut self, line: &str) {
        writeln!(self.stdin, "{line}").expect("write to server stdin");
        self.stdin.flush().expect("flush server stdin");
    }

    /// Read one response line, waiting up to `timeout` before giving up.
    fn read_line(&mut self, timeout: Duration) -> Option<String> {
        self.lines.recv_timeout(timeout).ok()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn responds_to_initialize_over_real_stdio() {
    let mut server = Server::spawn();

    server.send_line(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"test","version":"0.1.0"}}}"#,
    );

    let line = server
        .read_line(Duration::from_secs(10))
        .expect("server should respond to initialize");
    let resp: serde_json::Value = serde_json::from_str(&line).expect("valid JSON response");
    assert_eq!(resp["id"], 1);
    assert!(resp["error"].is_null());
    assert_eq!(resp["result"]["serverInfo"]["name"], "handoff-mcp");
}

#[test]
fn processes_sequential_requests_in_order() {
    let mut server = Server::spawn();

    for i in 1..=5 {
        server.send_line(&format!(
            r#"{{"jsonrpc":"2.0","id":{i},"method":"tools/list","params":{{}}}}"#
        ));
        let line = server
            .read_line(Duration::from_secs(10))
            .unwrap_or_else(|| panic!("expected response #{i}"));
        let resp: serde_json::Value = serde_json::from_str(&line).expect("valid JSON response");
        assert_eq!(
            resp["id"], i,
            "response id should match request id in order"
        );
        assert!(resp["error"].is_null());
    }
}

#[test]
fn burst_of_many_requests_all_complete_without_hanging() {
    // Regression test for t71: simulates many parallel sub-agents each
    // firing hook-driven requests at the single stdio server in quick
    // succession (the VSCode-hang scenario from wiki/100-stdio-concurrency.md).
    // Every response must arrive well within the per-request timeout budget,
    // and the whole burst must complete in bounded wall-clock time — proving
    // the worker-thread + recv_timeout model doesn't let any single request
    // stall the queue indefinitely.
    let mut server = Server::spawn();
    const BURST_SIZE: usize = 50;

    let started = std::time::Instant::now();
    for i in 0..BURST_SIZE {
        server.send_line(&format!(
            r#"{{"jsonrpc":"2.0","id":{i},"method":"tools/list","params":{{}}}}"#
        ));
    }
    for i in 0..BURST_SIZE {
        let line = server
            .read_line(Duration::from_secs(10))
            .unwrap_or_else(|| panic!("burst response #{i} should arrive, not hang"));
        let resp: serde_json::Value = serde_json::from_str(&line).expect("valid JSON response");
        assert_eq!(resp["id"], i, "responses must stay in request order");
        assert!(resp["error"].is_null());
    }
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "burst of {BURST_SIZE} requests should complete well within 10s, took {:?}",
        started.elapsed()
    );
}

#[test]
fn notification_produces_no_response_but_next_request_still_works() {
    let mut server = Server::spawn();

    // Notifications (no `id`) must not produce a response line.
    server.send_line(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#);

    // Follow up with a real request; it must still get its own response,
    // proving the notification didn't leave the server stuck.
    server.send_line(r#"{"jsonrpc":"2.0","id":42,"method":"tools/list","params":{}}"#);
    let line = server
        .read_line(Duration::from_secs(10))
        .expect("request after a notification should still get a response");
    let resp: serde_json::Value = serde_json::from_str(&line).expect("valid JSON response");
    assert_eq!(resp["id"], 42);
    assert!(resp["error"].is_null());
}

#[test]
fn malformed_json_returns_parse_error_promptly() {
    let mut server = Server::spawn();

    server.send_line("not valid json{{{");
    let line = server
        .read_line(Duration::from_secs(10))
        .expect("server should return a parse error response");
    let resp: serde_json::Value = serde_json::from_str(&line).expect("valid JSON response");
    assert_eq!(resp["error"]["code"], -32700);

    // Server must still be alive and able to answer subsequent requests.
    server.send_line(r#"{"jsonrpc":"2.0","id":7,"method":"tools/list","params":{}}"#);
    let line = server
        .read_line(Duration::from_secs(10))
        .expect("server should still respond after a parse error");
    let resp: serde_json::Value = serde_json::from_str(&line).expect("valid JSON response");
    assert_eq!(resp["id"], 7);
}

#[test]
fn slow_request_times_out_with_jsonrpc_error_and_server_stays_alive() {
    // Override the request timeout to 0s so the deadline elapses before any
    // real handler (e.g. `tools/list`, ~2ms) can finish — deterministically
    // exercising the timeout fail-safe path without needing a built-in
    // "sleep" tool or waiting on the production 30s default.
    let mut server = Server::spawn_with_timeout(Some(0));

    server.send_line(r#"{"jsonrpc":"2.0","id":99,"method":"tools/list","params":{}}"#);
    let line = server
        .read_line(Duration::from_secs(10))
        .expect("server must respond (either real result or timeout error), never hang");
    let resp: serde_json::Value = serde_json::from_str(&line).expect("valid JSON response");
    assert_eq!(resp["id"], 99);
    assert_eq!(
        resp["error"]["code"], -32603,
        "timeout error must use JSON-RPC code -32603; got {resp}"
    );
    assert!(resp["error"]["message"]
        .as_str()
        .unwrap_or("")
        .contains("timed out"));

    // The server must remain responsive for the next request (worker thread
    // model, not a crashed process) even after a timeout was returned.
    server.send_line(r#"{"jsonrpc":"2.0","id":100,"method":"tools/list","params":{}}"#);
    let line2 = server
        .read_line(Duration::from_secs(10))
        .expect("server should remain alive and responsive after a timeout");
    let resp2: serde_json::Value = serde_json::from_str(&line2).expect("valid JSON response");
    assert_eq!(resp2["id"], 100);
}

/// t79 layer 1: the strengthened `estimate_hours` guidance must actually reach
/// the client over the real transport. A description that exists only in the
/// source is worthless — the calling model only ever sees `tools/list` output.
#[test]
fn tools_list_advertises_estimate_hours_requirement() {
    let mut server = Server::spawn();

    server.send_line(r#"{"jsonrpc":"2.0","id":1,"method":"tools/list","params":{}}"#);
    let line = server
        .read_line(Duration::from_secs(10))
        .expect("server should respond to tools/list");
    let resp: serde_json::Value = serde_json::from_str(&line).expect("valid JSON response");

    let tools = resp["result"]["tools"]
        .as_array()
        .expect("tools/list should return a tools array");
    let update_task = tools
        .iter()
        .find(|t| t["name"] == "handoff_update_task")
        .expect("handoff_update_task must be advertised");

    let estimate = &update_task["inputSchema"]["properties"]["task"]["properties"]["schedule"]
        ["properties"]["estimate_hours"];
    let desc = estimate["description"]
        .as_str()
        .expect("estimate_hours must carry a description");

    assert!(
        desc.contains("REQUIRED"),
        "estimate_hours description must state it is required: {desc}"
    );
    assert!(
        desc.contains("blocked") && desc.contains("skipped"),
        "estimate_hours description must name the exempt statuses: {desc}"
    );

    // The `task` object itself must warn before the caller ever opens `schedule`.
    let task_desc = update_task["inputSchema"]["properties"]["task"]["description"]
        .as_str()
        .expect("task object must carry a description");
    assert!(
        task_desc.contains("estimate_hours"),
        "task description must surface the estimate_hours requirement: {task_desc}"
    );
}

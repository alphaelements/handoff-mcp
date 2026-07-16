use std::io::{self, BufRead, Write};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use handoff_mcp::mcp::protocol::process_line;

/// Maximum time a single JSON-RPC request may take before the server gives
/// up waiting and returns a fail-safe error response. Processing continues
/// on the worker thread in the background; the timeout only bounds how long
/// the main loop blocks waiting for a reply. Override for tests via
/// `HANDOFF_MCP_REQUEST_TIMEOUT_SECS` (parsed once at startup; falls back to
/// the 30s default on missing/invalid values).
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

fn request_timeout() -> Duration {
    let secs = std::env::var("HANDOFF_MCP_REQUEST_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS);
    Duration::from_secs(secs)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 {
        match args[1].as_str() {
            "setup" => {
                let check = args.iter().any(|a| a == "--check");
                let uninstall = args.iter().any(|a| a == "--uninstall");
                let mcp_json = args.iter().any(|a| a == "--mcp-json");
                let yes = args.iter().any(|a| a == "-y" || a == "--yes");
                if let Err(e) =
                    handoff_mcp::setup::run_setup_with_opts(check, uninstall, mcp_json, yes)
                {
                    eprintln!("Error: {e:#}");
                    std::process::exit(1);
                }
                return;
            }
            "--version" | "-V" => {
                println!("handoff-mcp v{}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "--help" | "-h" => {
                print_help();
                return;
            }
            other => {
                if handoff_mcp::cli::is_cli_command(other) {
                    let cli_args: Vec<String> = args[1..].to_vec();
                    // Handle --help for group
                    if cli_args.len() >= 2 && (cli_args[1] == "--help" || cli_args[1] == "-h") {
                        handoff_mcp::cli::print_group_help(&cli_args[0]);
                        return;
                    }
                    let code = handoff_mcp::cli::run(&cli_args);
                    std::process::exit(code);
                }
                eprintln!("Unknown command: {other}");
                eprintln!("Run `handoff-mcp --help` for usage.");
                std::process::exit(1);
            }
        }
    }

    eprintln!("handoff-mcp v{}", env!("CARGO_PKG_VERSION"));

    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let timeout = request_timeout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("stdin read error: {e}");
                break;
            }
        };

        // Run this request on a dedicated worker thread so a slow handler
        // can't block the main loop forever: `recv_timeout` below bounds how
        // long we wait for a reply. The next line is only read after this
        // request's worker completes (or times out) — sequential processing
        // order is preserved; this is a fail-safe timeout, not parallelism.
        let (tx, rx) = mpsc::channel();
        let line_for_worker = line.clone();
        thread::spawn(move || {
            let result = process_line(&line_for_worker);
            let _ = tx.send(result);
        });

        match rx.recv_timeout(timeout) {
            Ok(Some(response)) => {
                if writeln!(stdout, "{response}").is_err() {
                    break;
                }
                if stdout.flush().is_err() {
                    break;
                }
            }
            Ok(None) => {
                // Notification: no response expected.
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {
                eprintln!("Request timed out after {}s", timeout.as_secs());
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|req| req.get("id").cloned())
                    .unwrap_or(serde_json::Value::Null);
                let error_response = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": {
                        "code": -32603,
                        "message": format!("Request timed out after {}s", timeout.as_secs())
                    }
                });
                if writeln!(stdout, "{error_response}").is_err() {
                    break;
                }
                if stdout.flush().is_err() {
                    break;
                }
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                eprintln!("Worker thread disconnected without a response");
            }
        }
    }
}

fn print_help() {
    handoff_mcp::cli::print_cli_help();
    println!(
        "
SERVER MODE:
    handoff-mcp              Start the MCP server (stdio transport)

SETUP:
    handoff-mcp setup              Install hooks + add handoff server to .mcp.json
    handoff-mcp setup --check      Check if hooks and .mcp.json are configured
    handoff-mcp setup --uninstall  Remove handoff hooks from Claude Code
    handoff-mcp setup --mcp-json   Add handoff server to .mcp.json (non-interactive)
    handoff-mcp setup -y           Skip all confirmation prompts

OPTIONS:
    -h, --help       Print this help message
    -V, --version    Print version"
    );
}

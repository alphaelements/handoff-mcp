use std::io::{self, BufRead, Write};

use handoff_mcp::mcp::protocol::process_line;

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() >= 2 {
        match args[1].as_str() {
            "setup" => {
                let check = args.iter().any(|a| a == "--check");
                let uninstall = args.iter().any(|a| a == "--uninstall");
                if let Err(e) = handoff_mcp::setup::run_setup(check, uninstall) {
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
                eprintln!("Unknown command: {other}");
                eprintln!("Run `handoff-mcp --help` for usage.");
                std::process::exit(1);
            }
        }
    }

    eprintln!("handoff-mcp v{}", env!("CARGO_PKG_VERSION"));

    let stdin = io::stdin();
    let mut stdout = io::stdout();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                eprintln!("stdin read error: {e}");
                break;
            }
        };

        if let Some(response) = process_line(&line) {
            if writeln!(stdout, "{response}").is_err() {
                break;
            }
            if stdout.flush().is_err() {
                break;
            }
        }
    }
}

fn print_help() {
    println!(
        "handoff-mcp v{version}
MCP server for AI session handoff

USAGE:
    handoff-mcp              Start the MCP server (stdio transport)
    handoff-mcp setup        Install memory auto-injection hooks into Claude Code
    handoff-mcp setup --check    Check if hooks are installed
    handoff-mcp setup --uninstall    Remove handoff hooks from Claude Code

OPTIONS:
    -h, --help       Print this help message
    -V, --version    Print version",
        version = env!("CARGO_PKG_VERSION")
    );
}

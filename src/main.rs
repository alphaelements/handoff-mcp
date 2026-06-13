use std::io::{self, BufRead, Write};

use handoff_mcp::mcp::protocol::process_line;

fn main() {
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

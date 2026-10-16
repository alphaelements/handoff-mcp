//! Minimal real-binary stdio JSON-RPC client shared by the requirement-tree
//! bench (`benches/trace_tree.rs`) and the determinism test
//! (`tests/trace_tree_determinism.rs`), plus the loader that saves the
//! synthetic corpus of [`crate::tree_synth`] into a fresh project.

#![allow(dead_code)] // each including crate uses a different subset

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{json, Value};

/// Generous: a cold 3,000-item report on a loaded CI box is still seconds.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(120);

pub struct Server {
    child: Child,
    stdin: ChildStdin,
    lines: Receiver<String>,
    next_id: u64,
    project: PathBuf,
}

impl Server {
    /// Spawns the server and initializes a fresh project in `dir`.
    pub fn start(dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_handoff-mcp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("failed to spawn handoff-mcp server");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = child.stdout.take().expect("stdout");
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut buf = String::new();
                match reader.read_line(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        if tx.send(buf.trim_end().to_string()).is_err() {
                            break;
                        }
                    }
                }
            }
        });
        let mut server = Server {
            child,
            stdin,
            lines: rx,
            next_id: 1,
            project: dir.to_path_buf(),
        };
        let init = server.call_text(
            "handoff_init",
            json!({"project_name": "tree-synth", "project_dir": dir.to_string_lossy()}),
        );
        assert!(!init.contains("\"isError\""), "init failed: {init}");
        server
    }

    pub fn project(&self) -> &Path {
        &self.project
    }

    /// Calls a tool against this server's project and returns the raw text
    /// payload of the response (so callers can measure its byte size).
    /// Panics on a tool error.
    pub fn call_text(&mut self, name: &str, mut arguments: Value) -> String {
        if arguments.get("project_dir").is_none() {
            arguments["project_dir"] = json!(self.project.to_string_lossy());
        }
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": {"name": name, "arguments": arguments},
        });
        writeln!(self.stdin, "{req}").expect("write to server stdin");
        self.stdin.flush().expect("flush server stdin");
        let line = self
            .lines
            .recv_timeout(RESPONSE_TIMEOUT)
            .unwrap_or_else(|_| panic!("no response for {name} within {RESPONSE_TIMEOUT:?}"));
        let resp: Value = serde_json::from_str(&line).expect("valid JSON-RPC response");
        assert_eq!(resp["id"], id, "response id must match request id");
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or_else(|| panic!("{name}: no text content in {resp}"))
            .to_string();
        assert!(
            !resp["result"]["isError"].as_bool().unwrap_or(false),
            "{name} failed: {text}"
        );
        text
    }

    /// Like [`Self::call_text`], parsed as JSON.
    pub fn call(&mut self, name: &str, arguments: Value) -> Value {
        let text = self.call_text(name, arguments);
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name}: invalid JSON ({e}): {text}"))
    }

    /// Saves every document of the synthetic corpus (upper layers first, so a
    /// `- parent:` / `- refines:` target always exists already).
    pub fn load_synthetic(&mut self) {
        for doc in crate::tree_synth::synth_docs() {
            let saved = self.call(
                "handoff_doc_save",
                json!({"slug": doc.slug, "title": doc.slug, "body": doc.body, "layer": doc.layer}),
            );
            assert!(saved.get("error").is_none(), "saving {}: {saved}", doc.slug);
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

//! Executes the bundled skill walkthrough
//! (`skills/handoff-trace/references/walkthrough.md`) against the real
//! `handoff-mcp` binary over stdio JSON-RPC.
//!
//! The walkthrough's ```` ```json walkthrough ```` blocks are the test script:
//! each block is `{"tool", "args", "expect"?, "save"?}`. Every call must
//! succeed, its response text must contain every `expect` string, and `save`
//! captures values (JSON pointers into the response) that later blocks use as
//! `${name}`. If a tool's behaviour or parameters drift from what the skill
//! teaches, this test fails instead of a plugin user finding out.

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const WALKTHROUGH: &str = "skills/handoff-trace/references/walkthrough.md";

struct Client {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Client {
    /// Spawns the binary with `project_dir` as its working directory, so the
    /// walkthrough's calls run without a `project_dir` argument — exactly as
    /// a plugin user's client would call them.
    fn spawn(project_dir: &Path) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_handoff-mcp"))
            .current_dir(project_dir)
            .env("HANDOFF_MCP_REQUEST_TIMEOUT_SECS", "900")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn handoff-mcp binary");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        let mut client = Client {
            child,
            stdin,
            stdout,
            next_id: 0,
        };
        client.rpc(
            "initialize",
            json!({"protocolVersion": "2024-11-05", "capabilities": {},
                   "clientInfo": {"name": "skill_walkthrough_e2e", "version": "0"}}),
        );
        let note = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        writeln!(client.stdin, "{note}").unwrap();
        client.stdin.flush().unwrap();
        client
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        self.next_id += 1;
        let line =
            json!({"jsonrpc": "2.0", "id": self.next_id, "method": method, "params": params});
        writeln!(self.stdin, "{line}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut buf = String::new();
        self.stdout.read_line(&mut buf).expect("read response line");
        let resp: Value = serde_json::from_str(&buf)
            .unwrap_or_else(|e| panic!("invalid JSON-RPC response: {e}: {buf}"));
        assert_eq!(
            resp["id"],
            json!(self.next_id),
            "response id mismatch: {buf}"
        );
        resp
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The ```` ```json walkthrough ```` blocks of `markdown`, in order, parsed.
fn walkthrough_steps(markdown: &str) -> Vec<Value> {
    let mut steps = Vec::new();
    let mut current: Option<String> = None;
    for line in markdown.lines() {
        match current.as_mut() {
            None if line.trim() == "```json walkthrough" => current = Some(String::new()),
            // An untagged JSON block in the walkthrough would silently not
            // run — every call example there must be executable.
            None if line.trim() == "```json" => {
                panic!("walkthrough has a ```json block without the `walkthrough` tag")
            }
            None => {}
            Some(_) if line.trim() == "```" => {
                let block = current.take().unwrap();
                let step: Value = serde_json::from_str(&block).unwrap_or_else(|e| {
                    panic!("walkthrough block is not valid JSON: {e}\n{block}")
                });
                steps.push(step);
            }
            Some(buf) => {
                buf.push_str(line);
                buf.push('\n');
            }
        }
    }
    assert!(current.is_none(), "unterminated walkthrough block");
    steps
}

/// Replaces every `${name}` in string values of `v` with its saved value.
fn substitute(v: &Value, vars: &HashMap<String, String>) -> Value {
    match v {
        Value::String(s) => {
            let mut out = s.clone();
            for (k, val) in vars {
                out = out.replace(&format!("${{{k}}}"), val);
            }
            assert!(!out.contains("${"), "unresolved placeholder in {s:?}");
            Value::String(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| substitute(x, vars)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| (k.clone(), substitute(x, vars)))
                .collect(),
        ),
        other => other.clone(),
    }
}

#[test]
fn walkthrough_parser_reads_blocks_in_order() {
    let md = "intro\n```json walkthrough\n{\"tool\": \"a\", \"args\": {}}\n```\ntext\n```toml\nnot = 1\n```\n```json walkthrough\n{\"tool\": \"b\",\n \"args\": {\"x\": \"${v}\"}}\n```\n";
    let steps = walkthrough_steps(md);
    assert_eq!(steps.len(), 2);
    assert_eq!(steps[1]["tool"], "b");
    let vars = HashMap::from([("v".to_string(), "42".to_string())]);
    assert_eq!(substitute(&steps[1]["args"], &vars), json!({"x": "42"}));
}

#[test]
#[should_panic(expected = "without the `walkthrough` tag")]
fn walkthrough_parser_rejects_untagged_json_blocks() {
    walkthrough_steps("```json\n{}\n```\n");
}

#[test]
fn skill_walkthrough_runs_against_the_real_binary() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let markdown = std::fs::read_to_string(root.join(WALKTHROUGH)).expect("read walkthrough");
    let steps = walkthrough_steps(&markdown);
    assert!(
        steps.len() >= 20,
        "walkthrough lost its steps: {}",
        steps.len()
    );

    let tmp = tempfile::tempdir().expect("tempdir");
    let mut client = Client::spawn(tmp.path());
    let mut vars: HashMap<String, String> = HashMap::new();

    for (n, step) in steps.iter().enumerate() {
        let tool = step["tool"]
            .as_str()
            .unwrap_or_else(|| panic!("step {n}: missing tool"));
        let args = substitute(&step["args"], &vars);
        let resp = client.rpc("tools/call", json!({"name": tool, "arguments": args}));
        let result = &resp["result"];
        let text: String = result["content"]
            .as_array()
            .map(|c| c.iter().filter_map(|x| x["text"].as_str()).collect())
            .unwrap_or_default();
        assert!(
            resp.get("error").is_none() && result["isError"] != json!(true),
            "step {n} ({tool}) failed:\n{resp}"
        );
        for expected in step["expect"].as_array().into_iter().flatten() {
            let expected = expected.as_str().expect("expect entries are strings");
            assert!(
                text.contains(expected),
                "step {n} ({tool}): response lacks {expected:?}:\n{text}"
            );
        }
        if let Some(save) = step["save"].as_object() {
            let parsed: Value = serde_json::from_str(&text).unwrap_or_else(|e| {
                panic!("step {n} ({tool}): `save` needs a JSON response: {e}\n{text}")
            });
            for (name, pointer) in save {
                let pointer = pointer.as_str().expect("save values are JSON pointers");
                let value = parsed
                    .pointer(pointer)
                    .and_then(Value::as_str)
                    .unwrap_or_else(|| {
                        panic!("step {n} ({tool}): {pointer} not found in:\n{text}")
                    });
                vars.insert(name.clone(), value.to_string());
            }
        }
    }
}

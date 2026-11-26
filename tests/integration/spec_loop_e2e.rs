//! End-to-end verification of spec-loop (t-spec-loop.11; REQ-SL-001..011).
//!
//! spec-loop is an agent-based workflow: the coordinator
//! (`plugin-task-loop/commands/spec-loop.md`) briefs Sonnet agents and
//! persists every result through `handoff_*` tools. These tests therefore do
//! two things:
//!
//! 1. validate the **shipped artifacts** (coordinator command, `spec-execute`
//!    skill and its references, the three `spec-*` agents, the tool-check
//!    procedure) and that every `handoff_*` tool they name really exists in
//!    the server;
//! 2. **simulate the coordinator** against the real `handoff-mcp` binary over
//!    JSON-RPC: Phase 0 parses a brainstorm fixture, Phases 1-3 produce
//!    content shaped by the skill's rules (architecture diagram, self-contained
//!    HTML mocks + `stateDiagram-v2`, use cases + `sequenceDiagram`) and save
//!    it with the exact tool arguments the skill prescribes, Phase 4 assembles
//!    the specification, exports a PDF and applies the approval hand-off.
//!    The agents' own generation is replaced by deterministic code; the
//!    skill's content/coverage checks are implemented here and also run on
//!    deliberately broken input.
//!
//! Optional tools (a plain `cargo test` must work on a bare machine):
//! * `typst` missing -> PDF assertions print `SKIPPED`, unless
//!   `HANDOFF_REQUIRE_TYPST=1`.
//! * a working `mmdc` + Chrome missing -> diagram validation prints
//!   `SKIPPED`, unless `HANDOFF_REQUIRE_MMDC=1`.

use std::collections::BTreeSet;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::OnceLock;

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
use serde_json::{json, Value};

const BINARY: &str = env!("CARGO_BIN_EXE_handoff-mcp");

// ---- real-binary JSON-RPC session -------------------------------------------

struct Session {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl Session {
    fn spawn() -> Self {
        let mut child = Command::new(BINARY)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn handoff-mcp");
        let stdin = child.stdin.take().expect("stdin");
        let stdout = BufReader::new(child.stdout.take().expect("stdout"));
        Session {
            child,
            stdin,
            stdout,
            next_id: 1,
        }
    }

    fn rpc(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        let req = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{req}").expect("write request");
        self.stdin.flush().expect("flush");
        let mut line = String::new();
        self.stdout.read_line(&mut line).expect("read response");
        let resp: Value = serde_json::from_str(line.trim()).expect("response is JSON");
        assert_eq!(resp["id"], id, "response id mismatch: {resp}");
        resp
    }

    /// Call a tool; returns `(is_error, text)`.
    fn call(&mut self, project: &Path, name: &str, mut arguments: Value) -> (bool, String) {
        arguments["project_dir"] = json!(project.to_string_lossy());
        let resp = self.rpc(
            "tools/call",
            json!({ "name": name, "arguments": arguments }),
        );
        let is_error = resp["result"]["isError"].as_bool().unwrap_or(false);
        let text = resp["result"]["content"][0]["text"]
            .as_str()
            .unwrap_or("")
            .to_string();
        (is_error, text)
    }

    fn ok(&mut self, project: &Path, name: &str, arguments: Value) -> Value {
        let (is_error, text) = self.call(project, name, arguments);
        assert!(!is_error, "{name} failed: {text}");
        serde_json::from_str(&text).unwrap_or_else(|e| panic!("{name} not JSON ({e}): {text}"))
    }

    /// Call a tool that must succeed and return its raw (non-JSON) text.
    fn ok_text(&mut self, project: &Path, name: &str, arguments: Value) -> String {
        let (is_error, text) = self.call(project, name, arguments);
        assert!(!is_error, "{name} failed: {text}");
        text
    }

    fn err(&mut self, project: &Path, name: &str, arguments: Value) -> String {
        let (is_error, text) = self.call(project, name, arguments);
        assert!(is_error, "{name} unexpectedly succeeded: {text}");
        text
    }

    fn tool_names(&mut self) -> BTreeSet<String> {
        let resp = self.rpc("tools/list", json!({}));
        resp["result"]["tools"]
            .as_array()
            .expect("tools array")
            .iter()
            .map(|t| t["name"].as_str().expect("tool name").to_string())
            .collect()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn init_project(s: &mut Session) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let (is_error, text) = s.call(
        dir.path(),
        "handoff_init",
        json!({ "project_name": "spec-loop-e2e" }),
    );
    assert!(!is_error, "init failed: {text}");
    dir
}

// ---- optional tools -----------------------------------------------------------

fn require_env(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|v| !v.is_empty() && v != "0")
}

fn typst_available() -> bool {
    match detect(Tool::Typst) {
        ToolStatus::Found { .. } => true,
        other => {
            assert!(
                !require_env("HANDOFF_REQUIRE_TYPST"),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            false
        }
    }
}

/// A `mmdc` that can really render a diagram here, probed once.
fn working_mmdc() -> Option<&'static Path> {
    static PROBE: OnceLock<Option<PathBuf>> = OnceLock::new();
    PROBE
        .get_or_init(|| {
            let ToolStatus::Found { path, .. } = detect(Tool::Mmdc) else {
                return None;
            };
            if std::env::var_os("PUPPETEER_EXECUTABLE_PATH").is_none() {
                if let ToolStatus::Found { path: chrome, .. } = detect(Tool::ChromeHeadlessShell) {
                    std::env::set_var("PUPPETEER_EXECUTABLE_PATH", chrome);
                }
            }
            let dir = tempfile::tempdir().ok()?;
            let ok = render_mermaid(&path, dir.path(), "probe", "flowchart TD\n  A-->B\n").is_ok();
            ok.then_some(path)
        })
        .as_deref()
}

fn mmdc_or_skip() -> Option<&'static Path> {
    let mmdc = working_mmdc();
    if mmdc.is_none() {
        assert!(
            !require_env("HANDOFF_REQUIRE_MMDC"),
            "HANDOFF_REQUIRE_MMDC is set but mmdc cannot render diagrams here"
        );
        eprintln!("SKIPPED: no working mmdc (+ Chrome)");
    }
    mmdc
}

/// Same switch the PDF pipeline applies (`htmlLabels` off), see spec-execute section 6.
const MMDC_CONFIG: &str =
    r#"{"htmlLabels":false,"flowchart":{"htmlLabels":false},"class":{"htmlLabels":false}}"#;

/// Renders `source` with `mmdc` into `dir`; `Err` carries mmdc's stderr.
fn render_mermaid(mmdc: &Path, dir: &Path, stem: &str, source: &str) -> Result<(), String> {
    let config = dir.join("mmdc-config.json");
    fs::write(&config, MMDC_CONFIG).unwrap();
    let input = dir.join(format!("{stem}.mmd"));
    let output = dir.join(format!("{stem}.svg"));
    fs::write(&input, source).unwrap();
    let out = Command::new(mmdc)
        .args(["-q", "-i"])
        .arg(&input)
        .arg("-o")
        .arg(&output)
        .arg("-c")
        .arg(&config)
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() && output.is_file() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).into_owned())
    }
}

// ---- artifact paths -------------------------------------------------------------

fn plugin_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("plugin-task-loop")
}

fn read_plugin(rel: &str) -> String {
    let path = plugin_dir().join(rel);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn fixture(name: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/spec-loop")
        .join(name);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

/// `key: value` pairs of the leading `---` block, plus the body after it.
fn split_frontmatter(name: &str, text: &str) -> (Vec<(String, String)>, String) {
    let rest = text
        .strip_prefix("---\n")
        .unwrap_or_else(|| panic!("{name}: must start with a --- frontmatter block"));
    let end = rest
        .find("\n---\n")
        .unwrap_or_else(|| panic!("{name}: frontmatter is not closed"));
    let pairs = rest[..end]
        .lines()
        .map(|l| {
            let (k, v) = l
                .split_once(':')
                .unwrap_or_else(|| panic!("{name}: bad frontmatter line {l:?}"));
            (k.trim().to_string(), v.trim().to_string())
        })
        .collect();
    (pairs, rest[end + 5..].to_string())
}

fn fm<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("frontmatter key {key} missing"))
}

/// Contents of every ```` ```mermaid ```` fence in `text`.
fn mermaid_blocks(text: &str) -> Vec<String> {
    let mut blocks = Vec::new();
    let mut current: Option<Vec<&str>> = None;
    for line in text.lines() {
        match (&mut current, line.trim_end()) {
            (None, l) if l.trim_start() == "```mermaid" => current = Some(Vec::new()),
            (Some(buf), l) if l.trim_start() == "```" => {
                blocks.push(buf.join("\n") + "\n");
                current = None;
            }
            (Some(buf), _) => buf.push(line),
            _ => {}
        }
    }
    blocks
}

// ---- 1. shipped artifacts -----------------------------------------------------

const SPEC_AGENTS: [(&str, &str); 3] = [
    ("spec-architect", "Phase 1"),
    ("spec-gui-designer", "Phase 2"),
    ("spec-usecase-analyst", "Phase 3"),
];

#[test]
fn artifacts_exist_with_valid_structure() {
    // Coordinator command.
    let cmd = read_plugin("commands/spec-loop.md");
    let (pairs, body) = split_frontmatter("spec-loop.md", &cmd);
    assert!(!fm(&pairs, "description").is_empty());
    assert!(!fm(&pairs, "argument-hint").is_empty());
    for heading in [
        "## Flow overview",
        "## Suspend and resume",
        "### Phase 0",
        "### Phase 1",
        "### Phase 2",
        "### Phase 3",
        "### Phase 4",
        "## Rules",
    ] {
        assert!(body.contains(heading), "spec-loop.md lacks {heading}");
    }

    // Skill + references.
    let skill = read_plugin("skills/spec-execute/SKILL.md");
    let (pairs, body) = split_frontmatter("SKILL.md", &skill);
    assert_eq!(fm(&pairs, "name"), "spec-execute");
    assert!(!fm(&pairs, "description").is_empty());
    for heading in [
        "## 2. Phase 1",
        "## 3. Phase 2",
        "## 4. Phase 3",
        "## 5. Phase 4",
        "## 6. Mermaid validation",
    ] {
        assert!(body.contains(heading), "SKILL.md lacks {heading}");
    }
    for rel in [
        "skills/spec-execute/references/diagram-templates.md",
        "skills/spec-execute/references/mock-guidelines.md",
    ] {
        assert!(plugin_dir().join(rel).is_file(), "{rel} missing");
        assert!(
            skill.contains(rel.rsplit('/').next().unwrap()),
            "{rel} unreferenced"
        );
    }

    // Agents: file exists, frontmatter consistent, contract sections present,
    // and both the coordinator and the skill name them.
    for (name, phase) in SPEC_AGENTS {
        let text = read_plugin(&format!("agents/{name}.md"));
        let (pairs, body) = split_frontmatter(name, &text);
        assert_eq!(fm(&pairs, "name"), name);
        assert_eq!(fm(&pairs, "model"), "sonnet", "{name}");
        let tools: Vec<_> = fm(&pairs, "tools").split(',').map(str::trim).collect();
        for t in ["Read", "Bash", "Grep"] {
            assert!(tools.contains(&t), "{name}: tool {t}");
        }
        for needle in [phase, "## Input", "## Output", "context is discarded"] {
            assert!(body.contains(needle), "{name} lacks {needle}");
        }
        assert!(
            body.contains("Do NOT call any state-modifying handoff tools"),
            "{name}: agents must stay read-only against handoff state"
        );
        assert!(cmd.contains(name), "spec-loop.md does not name {name}");
        assert!(skill.contains(name), "SKILL.md does not name {name}");
    }

    // Tool-check procedure referenced by the coordinator.
    let tool_check = read_plugin("agents/_spec-loop-tool-check.md");
    for needle in [
        "mmdc",
        "typst",
        "Chrome",
        "HANDOFF_MMDC_BIN",
        "optional",
        "required",
    ] {
        assert!(tool_check.contains(needle), "tool-check lacks {needle}");
    }
    assert!(cmd.contains("_spec-loop-tool-check.md"));
    assert!(cmd.contains("handoff-task-loop:spec-execute"));
}

#[test]
fn every_handoff_tool_named_by_spec_loop_artifacts_exists_in_the_server() {
    let mut texts = vec![
        read_plugin("commands/spec-loop.md"),
        read_plugin("skills/spec-execute/SKILL.md"),
        read_plugin("agents/_spec-loop-tool-check.md"),
    ];
    texts.extend(
        SPEC_AGENTS
            .iter()
            .map(|(n, _)| read_plugin(&format!("agents/{n}.md"))),
    );
    let mut named = BTreeSet::new();
    for text in &texts {
        let mut rest = text.as_str();
        while let Some(i) = rest.find("handoff_") {
            let tail = &rest[i..];
            let len = tail
                .find(|c: char| !(c.is_ascii_lowercase() || c == '_'))
                .unwrap_or(tail.len());
            let name = tail[..len].trim_end_matches('_');
            // `handoff_notes` is an argument of handoff_save_context, not a tool.
            if name.len() > "handoff_".len() && name != "handoff_notes" {
                named.insert(name.to_string());
            }
            rest = &tail[len.max(1)..];
        }
    }
    let mut s = Session::spawn();
    let known = s.tool_names();
    // The tools the simulation below drives must be among the named ones.
    for tool in [
        "handoff_doc_save",
        "handoff_doc_export",
        "handoff_doc_get",
        "handoff_doc_list",
        "handoff_save_context",
    ] {
        assert!(named.contains(tool), "artifacts no longer mention {tool}");
    }
    let unknown: Vec<_> = named.iter().filter(|n| !known.contains(*n)).collect();
    assert!(unknown.is_empty(), "named but not served: {unknown:?}");
}

// ---- 2. coordinator simulation ------------------------------------------------

#[derive(Debug, Default)]
struct Screen {
    name: String,
    function: String,
}

/// Phase 0 result: what the coordinator extracts from a brainstorm report.
#[derive(Debug, Default)]
struct Import {
    topic: String,
    scope: Vec<(String, String)>, // (BS-SCOPE-NNN, title)
    priorities: Vec<String>,
    hints: Vec<String>,
    out_of_scope: Vec<String>,
    screens: Vec<Screen>,
    screen_flow: Vec<(String, String)>,
    ux_policy: Vec<String>,
}

impl Import {
    fn has_gui(&self) -> bool {
        !self.screens.is_empty()
    }
}

/// `#### BS-<kind>-NNN title` headings, as `(id, title)`.
fn bs_headings(text: &str, kind: &str) -> Vec<(String, String)> {
    let prefix = format!("BS-{kind}-");
    text.lines()
        .filter_map(|l| l.strip_prefix("#### "))
        .filter(|l| l.starts_with(&prefix))
        .map(|l| {
            let (id, title) = l.split_once(' ').unwrap_or((l, ""));
            (id.to_string(), title.trim().to_string())
        })
        .collect()
}

/// Bullet lines (`- x`) of the section whose heading line contains `heading`,
/// up to the next heading.
fn bullets_under(text: &str, heading: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut inside = false;
    for line in text.lines() {
        if line.starts_with('#') {
            inside = line.contains(heading);
        } else if inside {
            if let Some(b) = line.strip_prefix("- ") {
                out.push(b.trim().to_string());
            }
        }
    }
    out
}

/// Heading-name based extraction (the section numbers shift for GUI projects).
fn import_report(text: &str) -> Import {
    let topic = text
        .lines()
        .next()
        .and_then(|l| l.strip_prefix("# ブレストレポート: "))
        .unwrap_or("")
        .trim()
        .to_string();
    let screens = text
        .lines()
        .skip_while(|l| !l.contains("画面一覧"))
        .skip(1)
        .take_while(|l| !l.starts_with('#'))
        .filter(|l| l.starts_with('|') && !l.contains("---") && !l.contains("名前"))
        .map(|l| {
            let cells: Vec<_> = l.trim_matches('|').split('|').map(str::trim).collect();
            Screen {
                name: cells[0].to_string(),
                function: cells[1].to_string(),
            }
        })
        .collect();
    let screen_flow = bullets_under(text, "画面遷移図")
        .iter()
        .filter_map(|l| {
            // "ホーム → 支出入力", "支出入力 → ホーム（保存後）"
            let (from, to) = l.split_once('→')?;
            let to = to.split('（').next().unwrap_or(to);
            Some((from.trim().to_string(), to.trim().to_string()))
        })
        .collect();
    Import {
        topic,
        scope: bs_headings(text, "SCOPE"),
        priorities: bs_headings(text, "PRIORITY")
            .into_iter()
            .map(|(_, t)| t)
            .collect(),
        hints: bs_headings(text, "HINT")
            .into_iter()
            .map(|(_, t)| t)
            .collect(),
        out_of_scope: bullets_under(text, "Out-of-scope"),
        screens,
        screen_flow,
        ux_policy: bullets_under(text, "UX 方針"),
    }
}

/// spec-architect's `Input` rule: empty `scope_items` is `blocked`, never invented scope.
fn brief_architect(import: &Import) -> Result<(), String> {
    if import.scope.is_empty() {
        Err("status: blocked - scope_items is empty".to_string())
    } else {
        Ok(())
    }
}

/// Short component name for a scope title: text before the first bracketed detail.
fn component_name(title: &str) -> String {
    title
        .split(['（', '('])
        .next()
        .unwrap_or(title)
        .trim()
        .to_string()
}

// -- Phase 1 --

/// Deterministic stand-in for `spec-architect`'s `flowchart TD`.
fn architecture_diagram(import: &Import) -> String {
    let mut d = String::from("flowchart TD\n  USER([利用者 / User])\n");
    d.push_str("  subgraph APP[\"システム / System\"]\n");
    for (i, (id, title)) in import.scope.iter().enumerate() {
        // Mermaid has no trailing comments: the trace comment gets its own line.
        d.push_str(&format!(
            "    %% {id}\n    C{}[\"{}\"]\n",
            i + 1,
            component_name(title)
        ));
    }
    d.push_str("    STORE[(\"データストア / Data store\")]\n  end\n");
    for i in 1..=import.scope.len() {
        d.push_str(&format!("  USER -->|操作 / interaction| C{i}\n"));
        d.push_str(&format!("  C{i} -->|保存 / write| STORE\n"));
    }
    d
}

/// The skill's Phase 1 content rules: subsystem boundary, every scope item
/// traced to a component, labelled data flows, an external actor, no orphan
/// component.
fn check_architecture(diagram: &str, import: &Import) -> Result<(), String> {
    if !diagram.starts_with("flowchart ") && !diagram.starts_with("C4Context") {
        return Err("first line must be the diagram type".into());
    }
    if !diagram.contains("subgraph") {
        return Err("no subsystem boundary (subgraph)".into());
    }
    if !diagram.contains("([") {
        return Err("no external actor/system (stadium node)".into());
    }
    for (id, _) in &import.scope {
        if !diagram.contains(id.as_str()) {
            return Err(format!("scope item {id} not placed"));
        }
    }
    for line in diagram.lines().filter(|l| l.contains("-->")) {
        if !line.contains('|') {
            return Err(format!("unlabelled data flow: {}", line.trim()));
        }
    }
    for i in 1..=import.scope.len() {
        let c = format!("C{i}");
        let touches = diagram
            .lines()
            .filter(|l| l.contains("-->"))
            .any(|l| l.split_whitespace().any(|w| w == c));
        if !touches {
            return Err(format!("component {c} has no data flow"));
        }
    }
    Ok(())
}

// -- Phase 2 --

fn screen_id(i: usize) -> String {
    format!("screen-{}", i + 1)
}

fn escape_html(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Deterministic stand-in for `spec-gui-designer`: one self-contained mock per
/// screen, links only to sibling mocks. Returns the written file paths.
fn write_mocks(import: &Import, dir: &Path) -> Vec<PathBuf> {
    fs::create_dir_all(dir).unwrap();
    let id_of = |name: &str| {
        import
            .screens
            .iter()
            .position(|s| s.name == name)
            .map(screen_id)
    };
    let mut paths = Vec::new();
    for (i, screen) in import.screens.iter().enumerate() {
        let links: String = import
            .screen_flow
            .iter()
            .filter(|(from, _)| *from == screen.name)
            .filter_map(|(_, to)| {
                id_of(to).map(|id| format!("<a href=\"{id}.html\">{}</a>\n", escape_html(to)))
            })
            .collect();
        let html = format!(
            "<!DOCTYPE html>\n<html lang=\"ja\">\n<head>\n<meta charset=\"utf-8\">\n\
             <title>{name}</title>\n<style>body {{ font-family: system-ui, sans-serif; }}</style>\n\
             </head>\n<body>\n<header><h1>{name}</h1></header>\n<main><p>{function}</p>\n\
             <nav>{links}</nav></main>\n</body>\n</html>\n",
            name = escape_html(&screen.name),
            function = escape_html(&screen.function),
        );
        let path = dir.join(format!("{}.html", screen_id(i)));
        fs::write(&path, html).unwrap();
        paths.push(path);
    }
    paths
}

/// Coordinator review checklist from `references/mock-guidelines.md`.
fn check_mocks(import: &Import, dir: &Path) -> Result<(), String> {
    for i in 0..import.screens.len() {
        let path = dir.join(format!("{}.html", screen_id(i)));
        let html = fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        if !html.starts_with("<!DOCTYPE html>") || !html.contains("<style>") {
            return Err(format!(
                "{}: not a self-contained html document",
                screen_id(i)
            ));
        }
        for forbidden in ["<script", "<iframe", "http://", "https://", "<link"] {
            if html.contains(forbidden) {
                return Err(format!("{}: contains {forbidden}", screen_id(i)));
            }
        }
        for part in html.split("href=\"").skip(1) {
            let target = part.split('"').next().unwrap_or("");
            if target.contains("://") || !dir.join(target).is_file() {
                return Err(format!("{}: dangling link {target}", screen_id(i)));
            }
        }
    }
    Ok(())
}

fn transition_diagram(import: &Import) -> String {
    let idx = |name: &str| import.screens.iter().position(|s| s.name == name);
    let mut d = String::from("stateDiagram-v2\n  [*] --> S1\n");
    for (i, s) in import.screens.iter().enumerate() {
        d.push_str(&format!(
            "  state \"{} (S{})\" as S{}\n",
            s.name,
            i + 1,
            i + 1
        ));
    }
    for (from, to) in &import.screen_flow {
        if let (Some(f), Some(t)) = (idx(from), idx(to)) {
            d.push_str(&format!("  S{} --> S{}: {to}へ\n", f + 1, t + 1));
        }
    }
    d.push_str("  S1 --> [*]\n");
    d
}

/// Every brainstorm `screen_flow` line must appear as an edge of the diagram.
fn check_transitions(diagram: &str, import: &Import) -> Result<(), String> {
    for (from, to) in &import.screen_flow {
        let f = import.screens.iter().position(|s| s.name == *from);
        let t = import.screens.iter().position(|s| s.name == *to);
        let (Some(f), Some(t)) = (f, t) else {
            return Err(format!("flow {from} -> {to} names an unknown screen"));
        };
        if !diagram.contains(&format!("S{} --> S{}", f + 1, t + 1)) {
            return Err(format!("flow {from} -> {to} missing from stateDiagram"));
        }
    }
    Ok(())
}

// -- Phase 3 --

#[derive(Clone)]
struct UseCase {
    n: usize,
    title: String,
    realizes: Vec<String>,
    screens: Vec<String>,
}

fn use_cases(import: &Import) -> Vec<UseCase> {
    import
        .scope
        .iter()
        .enumerate()
        .map(|(i, (id, title))| UseCase {
            n: i + 1,
            title: component_name(title),
            realizes: vec![id.clone()],
            screens: import
                .screens
                .get(i)
                .map(|s| vec![s.name.clone()])
                .unwrap_or_default(),
        })
        .collect()
}

fn render_use_case(uc: &UseCase, comp: &str) -> String {
    let screens = if uc.screens.is_empty() {
        "none".to_string()
    } else {
        uc.screens.join(", ")
    };
    format!(
        "### UC-{n}: {title}\nRealizes: {realizes}\nComponents: {comp}    Screens: {screens}\n\
         - actor: 利用者; supporting: none\n- preconditions: none\n- main scenario:\n  \
         1. 利用者が{title}を開始する\n  2. システムが結果を表示する\n- alternative scenarios:\n  \
         - at step 1: 入力を取り消す\n- exceptions:\n  - at step 2: 保存に失敗した場合はエラーを表示する\n\
         - postconditions: 結果が保存されている\n\n```mermaid\nsequenceDiagram\n  autonumber\n  \
         actor U as 利用者\n  participant C as {comp}\n  U->>C: {title}を開始\n  \
         alt 成功\n    C-->>U: 結果を表示\n  else 失敗\n    C-->>U: エラーを表示\n  end\n```\n",
        n = uc.n,
        title = uc.title,
        realizes = uc.realizes.join(", "),
    )
}

/// Phase 3 coverage check: missing / duplicate / unplaced findings.
fn coverage_findings(import: &Import, ucs: &[UseCase]) -> Vec<String> {
    let mut findings = Vec::new();
    for (id, _) in &import.scope {
        if !ucs.iter().any(|u| u.realizes.contains(id)) {
            findings.push(format!("missing: {id} has no use case"));
        }
    }
    for s in &import.screens {
        if !ucs.iter().any(|u| u.screens.contains(&s.name)) {
            findings.push(format!("missing: screen {} used by no use case", s.name));
        }
    }
    for (i, a) in ucs.iter().enumerate() {
        for b in &ucs[i + 1..] {
            if a.title == b.title {
                findings.push(format!("duplicate: UC-{} and UC-{}", a.n, b.n));
            }
        }
        if a.realizes.is_empty() {
            findings.push(format!("unplaced: UC-{} realizes nothing", a.n));
        }
    }
    findings
}

// -- Phase 4 --

fn assemble_spec(
    import: &Import,
    report_doc: &str,
    architecture: &str,
    mock_paths: &[PathBuf],
    transitions: Option<&str>,
    ucs: &[UseCase],
    status: &str,
) -> String {
    let topic = if import.topic.is_empty() {
        "spec"
    } else {
        &import.topic
    };
    let mut b = format!(
        "# 要求仕様書: {topic}\n\n## 1. 概要\n\n- ブレストレポート: {report_doc}\n- ステータス: {status}\n\n\
         ## 2. スコープ\n\n| ID | タイトル | 優先度 |\n|---|---|---|\n"
    );
    for (i, (id, title)) in import.scope.iter().enumerate() {
        let prio = import.priorities.get(i).map_or("-", String::as_str);
        b.push_str(&format!("| {id} | {title} | {prio} |\n"));
    }
    b.push_str("\nOut-of-scope:\n\n");
    for o in &import.out_of_scope {
        b.push_str(&format!("- {o}\n"));
    }
    b.push_str(&format!(
        "\n## 3. システムアーキテクチャ\n\n```mermaid\n{architecture}```\n\n## 4. 画面設計\n\n"
    ));
    match transitions {
        Some(t) => {
            b.push_str("| 画面 | モック |\n|---|---|\n");
            for (s, p) in import.screens.iter().zip(mock_paths) {
                b.push_str(&format!("| {} | {} |\n", s.name, p.display()));
            }
            b.push_str(&format!("\n```mermaid\n{t}```\n"));
        }
        None => b.push_str("Phase 2 skipped: no GUI\n"),
    }
    b.push_str("\n## 5. ユースケース\n\n");
    for uc in ucs {
        b.push_str(&render_use_case(
            uc,
            &component_name(&import.scope[uc.n - 1].1),
        ));
        b.push('\n');
    }
    b.push_str("## 6. 制約・未決事項\n\n");
    for h in &import.hints {
        b.push_str(&format!("- {h}\n"));
    }
    if !import.ux_policy.is_empty() {
        b.push_str("\nUX 方針:\n\n");
        for u in &import.ux_policy {
            b.push_str(&format!("- {u}\n"));
        }
    }
    b
}

/// What a full run produced, for the per-fixture assertions.
struct Run {
    import: Import,
    project: tempfile::TempDir,
    arch_doc: String,
    screens_doc: Option<String>,
    spec_doc: String,
    pdf_path: Option<String>,
    mock_paths: Vec<PathBuf>,
    spec_body: String,
}

fn doc_get(s: &mut Session, project: &Path, doc_id: &str) -> Value {
    s.ok(project, "handoff_doc_get", json!({ "doc_id": doc_id }))
}

fn doc_tags(doc: &Value) -> Vec<String> {
    doc["tags"]
        .as_array()
        .unwrap_or_else(|| panic!("doc has no tags: {doc}"))
        .iter()
        .map(|t| t.as_str().unwrap().to_string())
        .collect()
}

/// Runs the whole coordinator flow for one brainstorm fixture against the real
/// server, asserting each phase's contract on the way.
fn run_spec_loop(s: &mut Session, fixture_name: &str) -> Run {
    let report = fixture(fixture_name);
    let project = init_project(s);
    let pd = project.path();

    // Phase 0: the brainstorm report is read-only input (a plain document here).
    let import = import_report(&report);
    assert!(!import.scope.is_empty(), "{fixture_name}: no scope parsed");
    brief_architect(&import).expect("scope is approved and non-empty");
    let report_doc = s.ok(
        pd,
        "handoff_doc_save",
        json!({
            "slug": "brainstorm-report-fixture", "title": format!("ブレストレポート: {}", import.topic),
            "doc_type": "note", "tags": ["brainstorm-report"], "body": report,
        }),
    )["doc_id"]
        .as_str()
        .unwrap()
        .to_string();

    // Phase 1: architecture -> intermediate design document.
    let diagram = architecture_diagram(&import);
    check_architecture(&diagram, &import).expect("architecture follows the content rules");
    let arch_body = format!(
        "# Architecture: {}\n\n```mermaid\n{diagram}```\n",
        import.topic
    );
    let saved = s.ok(
        pd,
        "handoff_doc_save",
        json!({
            "slug": "spec-e2e-architecture", "title": format!("Architecture: {}", import.topic),
            "doc_type": "design", "tags": ["spec-loop", "spec-intermediate", "phase-1"],
            "body": arch_body,
        }),
    );
    let arch_doc = saved["doc_id"].as_str().unwrap().to_string();
    let got = doc_get(s, pd, &arch_doc);
    assert_eq!(got["doc_type"], "design");
    assert_eq!(
        doc_tags(&got),
        ["spec-loop", "spec-intermediate", "phase-1"]
    );
    assert!(got["body"]
        .as_str()
        .unwrap()
        .contains("```mermaid\nflowchart TD"));

    // Phase 2: mocks + stateDiagram, or skipped.
    let mocks_dir = pd.join(".handoff/exports/spec-e2e/mocks");
    let (screens_doc, mock_paths, transitions) = if import.has_gui() {
        let paths = write_mocks(&import, &mocks_dir);
        assert_eq!(paths.len(), import.screens.len());
        check_mocks(&import, &mocks_dir).expect("mocks are self-contained with live links");
        let t = transition_diagram(&import);
        check_transitions(&t, &import).expect("every screen_flow line is an edge");
        let body = format!(
            "# Screens: {}\n\n{}\n\n```mermaid\n{t}```\n",
            import.topic,
            paths
                .iter()
                .map(|p| format!("- {}", p.display()))
                .collect::<Vec<_>>()
                .join("\n")
        );
        let id = s.ok(
            pd,
            "handoff_doc_save",
            json!({
                "slug": "spec-e2e-screens", "title": format!("Screens: {}", import.topic),
                "doc_type": "design", "tags": ["spec-loop", "spec-intermediate", "phase-2"],
                "body": body,
            }),
        )["doc_id"]
            .as_str()
            .unwrap()
            .to_string();
        (Some(id), paths, Some(t))
    } else {
        assert!(
            !mocks_dir.exists(),
            "Phase 2 skipped: no mock output directory may be created"
        );
        (None, Vec::new(), None)
    };

    // Phase 3: use cases -> coverage check -> create the spec document (no doc_id).
    let ucs = use_cases(&import);
    assert_eq!(
        coverage_findings(&import, &ucs),
        Vec::<String>::new(),
        "approved use cases cover scope and screens"
    );
    let draft = assemble_spec(
        &import,
        &report_doc,
        &diagram,
        &mock_paths,
        transitions.as_deref(),
        &ucs,
        "draft",
    );
    let created = s.ok(
        pd,
        "handoff_doc_save",
        json!({
            "slug": "spec-e2e", "title": format!("要求仕様書: {}", import.topic),
            "doc_type": "spec", "tags": ["spec-loop", "requirements-spec"], "body": draft,
        }),
    );
    let spec_doc = created["doc_id"].as_str().unwrap().to_string();
    assert_ne!(spec_doc, arch_doc, "spec doc must not reuse Phase 1 doc id");
    if let Some(sd) = &screens_doc {
        assert_ne!(&spec_doc, sd, "spec doc must not reuse Phase 2 doc id");
    }
    // Suspend/resume note format from the coordinator.
    let note =
        format!("spec-loop: spec_doc={spec_doc} brainstorm_report={report_doc} phase_done=3");
    s.ok_text(
        pd,
        "handoff_save_context",
        json!({
            "session_status": "active", "summary": "Spec: e2e",
            "handoff_notes": [{ "category": "context", "note": note }],
        }),
    );

    // Phase 4: update the same doc, export, final-gate approval.
    let final_body = assemble_spec(
        &import,
        &report_doc,
        &diagram,
        &mock_paths,
        transitions.as_deref(),
        &ucs,
        "draft",
    );
    let updated = s.ok(
        pd,
        "handoff_doc_save",
        json!({
            "doc_id": spec_doc, "body": final_body, "tags": ["spec-loop", "requirements-spec"],
        }),
    );
    assert_eq!(updated["doc_id"], spec_doc.as_str(), "updated in place");
    let pdf_path = if typst_available() {
        let out = s.ok(
            pd,
            "handoff_doc_export",
            json!({ "doc_id": spec_doc, "format": "pdf", "template": "spec" }),
        );
        let path = out["pdf_path"]
            .as_str()
            .unwrap_or_else(|| panic!("export has no pdf_path: {out}"))
            .to_string();
        assert!(
            path.ends_with("spec-e2e.pdf"),
            "pdf is named after the slug: {path}"
        );
        let bytes = fs::read(&path).unwrap_or_else(|e| panic!("read {path}: {e}"));
        assert!(bytes.starts_with(b"%PDF-"), "{path} is not a PDF");
        assert!(
            out["warnings"].is_array() || out["warnings"].is_null(),
            "{out}"
        );
        Some(path)
    } else {
        None
    };

    // Approve: tags replace, so the full list plus `approved`; status flips in section 1.
    let approved_body = assemble_spec(
        &import,
        &report_doc,
        &diagram,
        &mock_paths,
        transitions.as_deref(),
        &ucs,
        "approved",
    );
    s.ok(
        pd,
        "handoff_doc_save",
        json!({
            "doc_id": spec_doc, "body": approved_body,
            "tags": ["spec-loop", "requirements-spec", "approved"],
        }),
    );
    s.ok_text(
        pd,
        "handoff_save_context",
        json!({
            "session_status": "closed", "summary": "Spec complete: e2e",
            "handoff_notes": [{
                "category": "suggestion",
                "note": format!("spec-loop: spec_doc={spec_doc} brainstorm_report={report_doc} status=approved. Next: /requirement-loop"),
            }],
        }),
    );

    Run {
        import,
        project,
        arch_doc,
        screens_doc,
        spec_doc,
        pdf_path,
        mock_paths,
        spec_body: approved_body,
    }
}

/// Post-run assertions shared by every fixture: the hand-off to
/// requirement-loop finds exactly the approved spec, intermediates untouched.
fn assert_handoff_state(s: &mut Session, run: &Run) {
    let pd = run.project.path();
    let listed = s.ok(
        pd,
        "handoff_doc_list",
        json!({ "tags": ["requirements-spec", "approved"] }),
    );
    let ids: Vec<_> = listed["documents"]
        .as_array()
        .or_else(|| listed.as_array())
        .unwrap_or_else(|| panic!("unexpected doc_list shape: {listed}"))
        .iter()
        .map(|d| d["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        ids,
        std::slice::from_ref(&run.spec_doc),
        "only the spec is approved"
    );

    let spec = doc_get(s, pd, &run.spec_doc);
    assert_eq!(spec["doc_type"], "spec");
    let body = spec["body"].as_str().unwrap();
    assert_eq!(body.trim_end(), run.spec_body.trim_end());
    assert!(body.contains("ステータス: approved"));

    // BS-* ids are kept in scope table and use cases for `refines`, but are
    // never defined as `#### BS-...` headings (they would collide).
    for (id, _) in &run.import.scope {
        assert!(
            body.contains(&format!("| {id} |")),
            "scope table lacks {id}"
        );
        assert!(
            body.contains(&format!("Realizes: {id}")),
            "no UC realizes {id}"
        );
    }
    assert!(
        !body.lines().any(|l| l.starts_with("#### BS-")),
        "spec must not define #### BS- headings"
    );
    // All six layout sections, in order.
    let mut at = 0;
    for h in [
        "## 1. 概要",
        "## 2. スコープ",
        "## 3. システムアーキテクチャ",
        "## 4. 画面設計",
        "## 5. ユースケース",
        "## 6. 制約・未決事項",
    ] {
        let pos = body[at..]
            .find(h)
            .unwrap_or_else(|| panic!("section {h} missing or out of order"));
        at += pos + h.len();
    }

    // Intermediates keep their own tags and never gained `approved`.
    let arch = doc_get(s, pd, &run.arch_doc);
    assert_eq!(
        doc_tags(&arch),
        ["spec-loop", "spec-intermediate", "phase-1"]
    );
    if let Some(sd) = &run.screens_doc {
        let screens = doc_get(s, pd, sd);
        assert_eq!(
            doc_tags(&screens),
            ["spec-loop", "spec-intermediate", "phase-2"]
        );
    }

    // The session closed with the resume/hand-off note.
    let ctx = s.ok_text(pd, "handoff_load_context", json!({}));
    assert!(
        ctx.contains("Next: /requirement-loop"),
        "closing hand-off note missing: {ctx}"
    );
}

/// Validates every Mermaid block of the spec with a real `mmdc`.
fn assert_diagrams_render(spec_body: &str) {
    let Some(mmdc) = mmdc_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let blocks = mermaid_blocks(spec_body);
    assert!(!blocks.is_empty());
    for (i, block) in blocks.iter().enumerate() {
        render_mermaid(mmdc, dir.path(), &format!("d{i}"), block)
            .unwrap_or_else(|e| panic!("diagram {i} failed mmdc:\n{block}\n{e}"));
    }
}

#[test]
fn full_run_gui_fixture_phases_0_to_4() {
    let mut s = Session::spawn();
    let run = run_spec_loop(&mut s, "gui_project_brainstorm.md");

    // Phase 0 extraction.
    assert_eq!(run.import.topic, "家計簿アプリ");
    assert_eq!(run.import.scope.len(), 4);
    assert_eq!(run.import.screens.len(), 4);
    assert_eq!(run.import.screen_flow.len(), 5);
    assert_eq!(run.import.hints.len(), 3);
    assert_eq!(run.import.out_of_scope.len(), 2);
    assert_eq!(run.import.ux_policy.len(), 3);

    // Phase 2 outputs exist on disk and in the spec.
    assert_eq!(run.mock_paths.len(), 4);
    for p in &run.mock_paths {
        assert!(p.is_file(), "{} missing", p.display());
    }
    assert!(run.screens_doc.is_some());
    let body = &run.spec_body;
    assert!(body.contains("stateDiagram-v2") && body.contains("sequenceDiagram"));
    assert!(body.contains("ホーム") && body.contains("支出入力"));
    assert_eq!(body.matches("### UC-").count(), 4);
    assert_eq!(
        mermaid_blocks(body).len(),
        1 + 1 + 4,
        "architecture + transitions + one sequence per use case"
    );

    assert_handoff_state(&mut s, &run);
    assert_diagrams_render(&run.spec_body);
    if let Some(pdf) = &run.pdf_path {
        assert!(Path::new(pdf).starts_with(run.project.path().join(".handoff/exports")));
    }
}

#[test]
fn full_run_cli_fixture_skips_phase_2() {
    let mut s = Session::spawn();
    let run = run_spec_loop(&mut s, "cli_project_brainstorm.md");

    assert!(!run.import.has_gui());
    assert!(run.import.screen_flow.is_empty());
    assert!(
        run.screens_doc.is_none(),
        "no Phase 2 document without a GUI"
    );
    assert!(run.mock_paths.is_empty());
    assert!(run.spec_body.contains("Phase 2 skipped: no GUI"));
    assert!(!run.spec_body.contains("stateDiagram-v2"));
    // Screen-less use cases say so explicitly instead of dropping the field.
    assert!(run.spec_body.contains("Screens: none"));

    assert_handoff_state(&mut s, &run);
    assert_diagrams_render(&run.spec_body);
}

#[test]
fn full_run_minimal_fixture_scope_only() {
    let mut s = Session::spawn();
    let run = run_spec_loop(&mut s, "minimal_brainstorm.md");

    assert_eq!(run.import.scope.len(), 2);
    assert!(run.import.hints.is_empty() && run.import.priorities.is_empty());
    assert!(run.spec_body.contains("Phase 2 skipped: no GUI"));
    // Missing priorities are shown as "-" rather than invented.
    assert!(run
        .spec_body
        .contains("| BS-SCOPE-001 | メモの作成と編集 | - |"));

    assert_handoff_state(&mut s, &run);
    assert_diagrams_render(&run.spec_body);
}

// ---- 3. error handling and missing data ----------------------------------------

#[test]
fn empty_or_missing_report_is_blocked_not_invented() {
    for text in [
        "",
        "# ブレストレポート: 空\n\n## 5. スコープ\n\n### 5.1 In-scope\n\n_(未記入)_\n",
    ] {
        let import = import_report(text);
        assert!(import.scope.is_empty());
        let err = brief_architect(&import).unwrap_err();
        assert!(err.contains("blocked"), "{err}");
    }
}

#[test]
fn import_tolerates_missing_sections_and_extra_whitespace() {
    // No GUI section at all, hints without priorities: still parses scope.
    let import = import_report(&fixture("minimal_brainstorm.md"));
    assert_eq!(import.topic, "メモ共有ツール");
    assert_eq!(
        import.scope,
        [
            ("BS-SCOPE-001".to_string(), "メモの作成と編集".to_string()),
            ("BS-SCOPE-002".to_string(), "メモの URL 共有".to_string()),
        ]
    );
    assert!(!import.has_gui());
}

#[test]
fn architecture_check_rejects_incomplete_diagrams() {
    let import = import_report(&fixture("gui_project_brainstorm.md"));
    let good = architecture_diagram(&import);
    check_architecture(&good, &import).unwrap();

    let no_boundary = good.replace("subgraph", "%% sub");
    assert!(check_architecture(&no_boundary, &import)
        .unwrap_err()
        .contains("boundary"));
    let no_external = good.replace("USER([", "USER[");
    assert!(check_architecture(&no_external, &import)
        .unwrap_err()
        .contains("external"));
    let unlabelled = good.replace("-->|操作 / interaction|", "-->");
    assert!(check_architecture(&unlabelled, &import)
        .unwrap_err()
        .contains("unlabelled"));
    let unplaced = good.replace("BS-SCOPE-003", "BS-SCOPE-999");
    assert!(check_architecture(&unplaced, &import)
        .unwrap_err()
        .contains("BS-SCOPE-003"));
    let orphan = format!("{good}  %% BS-SCOPE-009\n  C9[\"孤立\"]\n");
    let mut more = import_report(&fixture("gui_project_brainstorm.md"));
    more.scope.push(("BS-SCOPE-009".into(), "孤立".into()));
    assert!(check_architecture(&orphan, &more)
        .unwrap_err()
        .contains("C5"));
}

#[test]
fn mock_check_rejects_external_references_scripts_and_dangling_links() {
    let import = import_report(&fixture("gui_project_brainstorm.md"));
    let dir = tempfile::tempdir().unwrap();
    write_mocks(&import, dir.path());
    check_mocks(&import, dir.path()).unwrap();

    let first = dir.path().join("screen-1.html");
    let original = fs::read_to_string(&first).unwrap();

    fs::write(
        &first,
        original.replace("</body>", "<script src=\"x.js\"></script></body>"),
    )
    .unwrap();
    assert!(check_mocks(&import, dir.path())
        .unwrap_err()
        .contains("<script"));

    fs::write(
        &first,
        original.replace("</main>", "<a href=\"https://example.com\">x</a></main>"),
    )
    .unwrap();
    assert!(check_mocks(&import, dir.path())
        .unwrap_err()
        .contains("https://"));

    fs::write(&first, original.replace("screen-2.html", "gone.html")).unwrap();
    assert!(check_mocks(&import, dir.path())
        .unwrap_err()
        .contains("dangling"));

    fs::write(&first, &original).unwrap();
    fs::remove_file(dir.path().join("screen-4.html")).unwrap();
    assert!(
        check_mocks(&import, dir.path()).is_err(),
        "missing mock file"
    );
}

#[test]
fn mocks_escape_untrusted_screen_text() {
    let mut import = import_report(&fixture("gui_project_brainstorm.md"));
    import.screens[0].name = "<script>alert(1)</script>".to_string();
    import.screens[0].function = "a & \"b\"".to_string();
    let dir = tempfile::tempdir().unwrap();
    write_mocks(&import, dir.path());
    let html = fs::read_to_string(dir.path().join("screen-1.html")).unwrap();
    assert!(!html.contains("<script>"), "{html}");
    assert!(html.contains("&lt;script&gt;") && html.contains("a &amp; &quot;b&quot;"));
}

#[test]
fn transition_check_flags_missing_and_unknown_flows() {
    let mut import = import_report(&fixture("gui_project_brainstorm.md"));
    let diagram = transition_diagram(&import);
    check_transitions(&diagram, &import).unwrap();

    let dropped = diagram.replace("S1 --> S2", "S1 --> S9");
    assert!(check_transitions(&dropped, &import)
        .unwrap_err()
        .contains("missing from stateDiagram"));

    import
        .screen_flow
        .push(("ホーム".into(), "存在しない画面".into()));
    assert!(check_transitions(&diagram, &import)
        .unwrap_err()
        .contains("unknown screen"));
}

#[test]
fn coverage_check_reports_missing_duplicate_and_unplaced() {
    let import = import_report(&fixture("gui_project_brainstorm.md"));
    let ucs = use_cases(&import);
    assert!(coverage_findings(&import, &ucs).is_empty());

    // Drop the use case for SCOPE-003 (and its screen): missing x2.
    let mut missing = ucs.clone();
    missing.remove(2);
    let f = coverage_findings(&import, &missing);
    assert!(f.iter().any(|x| x.contains("BS-SCOPE-003")), "{f:?}");
    assert!(f.iter().any(|x| x.contains("予算設定")), "{f:?}");

    let mut dup = ucs.clone();
    dup[1].title = dup[0].title.clone();
    assert!(coverage_findings(&import, &dup)
        .iter()
        .any(|x| x.starts_with("duplicate")));

    let mut unplaced = ucs;
    unplaced[0].realizes.clear();
    assert!(coverage_findings(&import, &unplaced)
        .iter()
        .any(|x| x.starts_with("unplaced")));
}

#[test]
fn server_rejects_bad_spec_tool_calls() {
    let mut s = Session::spawn();
    let project = init_project(&mut s);
    let pd = project.path();

    // Export / read / update of a document that does not exist.
    let msg = s.err(
        pd,
        "handoff_doc_export",
        json!({ "doc_id": "no-such-spec", "format": "pdf", "template": "spec" }),
    );
    assert!(msg.contains("no-such-spec"), "{msg}");
    s.err(pd, "handoff_doc_get", json!({ "doc_id": "no-such-spec" }));
    s.err(
        pd,
        "handoff_doc_save",
        json!({ "doc_id": "no-such-spec", "body": "# x\n" }),
    );

    // Creating without a body / with an invalid slug.
    s.err(
        pd,
        "handoff_doc_save",
        json!({ "slug": "spec-empty", "title": "t" }),
    );
    s.err(
        pd,
        "handoff_doc_save",
        json!({ "slug": "Bad Slug!", "title": "t", "body": "# t\n" }),
    );

    // A valid spec exports only with a known template.
    let id = s.ok(
        pd,
        "handoff_doc_save",
        json!({
            "slug": "spec-ok", "title": "ok", "doc_type": "spec",
            "tags": ["spec-loop", "requirements-spec"], "body": "# ok\n\ntext\n",
        }),
    )["doc_id"]
        .as_str()
        .unwrap()
        .to_string();
    let msg = s.err(
        pd,
        "handoff_doc_export",
        json!({ "doc_id": id, "format": "pdf", "template": "no-such-template" }),
    );
    assert!(msg.contains("template"), "{msg}");

    // No such approved spec yet: requirement-loop's lookup finds nothing.
    let listed = s.ok(
        pd,
        "handoff_doc_list",
        json!({ "tags": ["requirements-spec", "approved"] }),
    );
    assert!(
        listed["documents"]
            .as_array()
            .or_else(|| listed.as_array())
            .is_some_and(Vec::is_empty),
        "{listed}"
    );
}

#[test]
fn unapproved_stop_leaves_document_saved_without_approved_tag() {
    // Final gate answer "Stop": no `approved` tag, document stays saved.
    let mut s = Session::spawn();
    let project = init_project(&mut s);
    let pd = project.path();
    let import = import_report(&fixture("minimal_brainstorm.md"));
    let ucs = use_cases(&import);
    let diagram = architecture_diagram(&import);
    let body = assemble_spec(&import, "none", &diagram, &[], None, &ucs, "draft");
    let id = s.ok(
        pd,
        "handoff_doc_save",
        json!({
            "slug": "spec-stopped", "title": "stopped", "doc_type": "spec",
            "tags": ["spec-loop", "requirements-spec"], "body": body,
        }),
    )["doc_id"]
        .as_str()
        .unwrap()
        .to_string();
    let doc = doc_get(&mut s, pd, &id);
    assert!(!doc_tags(&doc).contains(&"approved".to_string()));
    assert!(doc["body"].as_str().unwrap().contains("ステータス: draft"));
}

#[test]
fn broken_mermaid_is_caught_by_mmdc_validation() {
    let Some(mmdc) = mmdc_or_skip() else { return };
    let dir = tempfile::tempdir().unwrap();
    let err = render_mermaid(mmdc, dir.path(), "bad", "flowchart TD\n  A -->|x\n  [[[\n")
        .expect_err("syntactically broken diagram must fail validation");
    assert!(!err.is_empty());
}

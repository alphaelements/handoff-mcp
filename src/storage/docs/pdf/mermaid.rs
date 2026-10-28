//! Mermaid -> SVG pre-conversion for the PDF export pipeline (REQ-PDF-003).
//!
//! Fenced ` ```mermaid ` blocks in a Markdown body are rendered to SVG with
//! `mmdc` (Mermaid CLI) and replaced by an image reference
//! (`![Mermaid diagram N](<cache>/mermaid-….svg)`), which the downstream
//! Markdown -> Typst converter embeds like any other image.
//!
//! Failure policy (AC2): a missing `mmdc`, or a single diagram that `mmdc`
//! rejects, never aborts the export. The affected block stays a plain code
//! block and a [`MermaidWarning`] is returned for the caller to surface.
//!
//! Lifecycle (AC3): generated SVGs live in a cache directory (conventionally
//! [`mermaid_cache_dir`], `.handoff/exports/.mermaid-cache/`). They are owned
//! by the returned [`MermaidOutput`], which deletes them on
//! [`MermaidOutput::cleanup`] or when dropped, so the caller keeps the value
//! alive until the PDF has been produced.

use std::fmt;
use std::io::Read;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};

/// Cache directory for generated SVGs, relative to the `.handoff/` dir.
pub const MERMAID_CACHE_SUBDIR: &str = "exports/.mermaid-cache";

/// Upper bound for one `mmdc` invocation (it launches a headless browser).
pub const MMDC_TIMEOUT: Duration = Duration::from_secs(60);

/// mmdc configuration (`-c`). Mermaid's default HTML labels live in
/// `<foreignObject>`, which Typst's SVG renderer drops (diagram boxes would
/// come out empty), so labels are switched to plain SVG `<text>`. The
/// top-level key covers sequence/state/etc.; `flowchart` and `class` carry
/// their own per-diagram setting.
const MMDC_CONFIG_JSON: &str =
    r#"{"htmlLabels":false,"flowchart":{"htmlLabels":false},"class":{"htmlLabels":false}}"#;

/// Maximum number of `mmdc` stderr bytes quoted in a warning.
const MAX_REASON_LEN: usize = 400;

/// Poll interval while waiting for `mmdc` to exit.
const POLL_INTERVAL: Duration = Duration::from_millis(20);

/// Makes generated file names unique across calls within one process.
static RUN_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `<handoff_dir>/exports/.mermaid-cache`.
pub fn mermaid_cache_dir(handoff_dir: &Path) -> PathBuf {
    handoff_dir.join(MERMAID_CACHE_SUBDIR)
}

/// A fenced mermaid code block located in a Markdown body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MermaidBlock {
    /// Byte range of the whole fenced block, fences included. Whether the
    /// closing line's newline is part of the range is up to the parser;
    /// callers must not rely on either.
    pub range: Range<usize>,
    /// Diagram source with the fences (and any container indentation) removed.
    pub source: String,
    /// Diagram title taken from the source itself (REQ-PDF-051), see
    /// [`extract_title`]. `None` when the diagram has no (non-blank) title.
    pub title: Option<String>,
}

/// Non-fatal problems met while converting; the block stays a code block.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MermaidWarning {
    /// `mmdc` is not installed; `blocks` diagrams were left as code blocks.
    MmdcNotFound { blocks: usize },
    /// Diagram number `index` (1-based, document order) could not be rendered.
    RenderFailed { index: usize, reason: String },
}

impl fmt::Display for MermaidWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MermaidWarning::MmdcNotFound { blocks } => write!(
                f,
                "mmdc (Mermaid CLI) not found (PATH or $HANDOFF_MMDC_BIN): {blocks} mermaid block(s) left as code \
                 blocks. Install it with `npm install -g @mermaid-js/mermaid-cli`."
            ),
            MermaidWarning::RenderFailed { index, reason } => write!(
                f,
                "mermaid block #{index} could not be rendered and was left as a code block: {reason}"
            ),
        }
    }
}

/// Result of [`convert_mermaid_blocks`]. Owns the generated SVG files.
#[derive(Debug)]
pub struct MermaidOutput {
    /// Markdown with every successfully rendered block replaced by an image.
    pub markdown: String,
    /// Problems that left blocks unconverted.
    pub warnings: Vec<MermaidWarning>,
    generated: Vec<PathBuf>,
    cache_dir: PathBuf,
}

impl MermaidOutput {
    /// SVG files currently owned by this value.
    pub fn generated_files(&self) -> &[PathBuf] {
        &self.generated
    }

    /// Delete the generated SVGs (and the cache dir if it is now empty).
    /// Idempotent. Keeps going after a failure and returns the first error.
    pub fn cleanup(&mut self) -> std::io::Result<()> {
        let mut first_err = None;
        for path in self.generated.drain(..) {
            match std::fs::remove_file(&path) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => {
                    first_err.get_or_insert(e);
                }
            }
        }
        // `remove_dir` only succeeds on an empty directory, so concurrent
        // exports' files and unrelated content are never touched.
        // Failure (non-empty / already gone) is the expected "leave it" case.
        let _ = std::fs::remove_dir(&self.cache_dir);
        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }
}

impl Drop for MermaidOutput {
    fn drop(&mut self) {
        // Best effort: a Drop cannot report failure, and callers wanting the
        // error call `cleanup()` explicitly first (which empties the list).
        let _ = self.cleanup();
    }
}

/// Locate every fenced ` ```mermaid ` block (info string's first word,
/// ASCII case-insensitive). Fenced blocks nested in code blocks or inline
/// code are not matched because detection goes through a CommonMark parser.
pub fn find_mermaid_blocks(markdown: &str) -> Vec<MermaidBlock> {
    let mut blocks = Vec::new();
    let mut current: Option<(Range<usize>, String)> = None;
    for (event, range) in Parser::new_ext(markdown, Options::empty()).into_offset_iter() {
        match event {
            Event::Start(Tag::CodeBlock(CodeBlockKind::Fenced(info)))
                if info
                    .split_whitespace()
                    .next()
                    .is_some_and(|lang| lang.eq_ignore_ascii_case("mermaid")) =>
            {
                current = Some((range, String::new()));
            }
            Event::Text(text) => {
                if let Some((_, source)) = current.as_mut() {
                    source.push_str(&text);
                }
            }
            Event::End(TagEnd::CodeBlock) => {
                if let Some((range, source)) = current.take() {
                    let title = extract_title(&source);
                    blocks.push(MermaidBlock {
                        range,
                        source,
                        title,
                    });
                }
            }
            _ => {}
        }
    }
    blocks
}

/// Convert every mermaid block in `markdown`.
///
/// `mmdc` must be resolved by the caller through
/// [`deps::detect`](super::deps::detect)`(Tool::Mmdc)` so that the
/// `HANDOFF_MMDC_BIN` override and the minimum-version check apply; this
/// module deliberately has no PATH lookup of its own.
///
/// * `mmdc == None` -> every block stays a code block plus one
///   [`MermaidWarning::MmdcNotFound`] (only when blocks exist).
/// * Rendered SVGs are written to `cache_dir` (created on demand) and the
///   block is replaced by `![Mermaid diagram N](<cache_dir>/<file>.svg)`, so
///   a relative `cache_dir` yields a relative image reference and the caller
///   decides what base the later image resolution uses.
/// * A block `mmdc` rejects (syntax error, timeout, no output) stays a code
///   block with a [`MermaidWarning::RenderFailed`]; other blocks still convert.
pub fn convert_mermaid_blocks_with(
    markdown: &str,
    cache_dir: &Path,
    mmdc: Option<&Path>,
    timeout: Duration,
) -> MermaidOutput {
    let blocks = find_mermaid_blocks(markdown);
    let mut out = MermaidOutput {
        markdown: String::new(),
        warnings: Vec::new(),
        generated: Vec::new(),
        cache_dir: cache_dir.to_path_buf(),
    };

    let Some(mmdc) = mmdc.filter(|_| !blocks.is_empty()) else {
        if !blocks.is_empty() {
            out.warnings.push(MermaidWarning::MmdcNotFound {
                blocks: blocks.len(),
            });
        }
        out.markdown = markdown.to_string();
        return out;
    };

    let run_id = RUN_COUNTER.fetch_add(1, Ordering::Relaxed);
    let mut result = String::with_capacity(markdown.len());
    let mut cursor = 0;
    for (i, block) in blocks.iter().enumerate() {
        let index = i + 1;
        let svg_path = cache_dir.join(format!(
            "mermaid-{}-{run_id}-{index}.svg",
            std::process::id()
        ));
        match render_block(mmdc, &block.source, &svg_path, timeout) {
            Ok(()) => {
                // Registered before the reference check so the file is
                // cleaned up whichever way this block ends.
                out.generated.push(svg_path.clone());
                match image_reference(index, block.title.as_deref(), &svg_path) {
                    Ok(reference) => {
                        result.push_str(&markdown[cursor..block.range.start]);
                        result.push_str(&reference);
                        if markdown[block.range.clone()].ends_with('\n') {
                            result.push('\n');
                        }
                        cursor = block.range.end;
                    }
                    Err(reason) => out
                        .warnings
                        .push(MermaidWarning::RenderFailed { index, reason }),
                }
            }
            Err(reason) => out
                .warnings
                .push(MermaidWarning::RenderFailed { index, reason }),
        }
    }
    result.push_str(&markdown[cursor..]);
    out.markdown = result;
    out
}

/// Markdown image reference for a rendered diagram. The alt text (which the
/// Typst converter turns into the figure caption) is the diagram `title`, or
/// "Mermaid diagram N" when there is none. Paths containing whitespace or
/// parentheses use the `<…>` destination form; paths that no Markdown
/// destination can express are rejected.
fn image_reference(index: usize, title: Option<&str>, svg_path: &Path) -> Result<String, String> {
    let path = svg_path.to_string_lossy();
    if path.contains(['<', '>', '\n', '\r']) {
        return Err(format!(
            "cache path {path:?} contains characters that cannot appear in a Markdown image path"
        ));
    }
    let destination = if path.contains([' ', '\t', '(', ')']) {
        format!("<{path}>")
    } else {
        path.into_owned()
    };
    let alt = match title.map(str::trim).filter(|t| !t.is_empty()) {
        Some(title) => escape_alt_text(title),
        None => format!("Mermaid diagram {index}"),
    };
    Ok(format!("![{alt}]({destination})"))
}

/// Backslash-escapes every character that could change the meaning of
/// Markdown image alt text (emphasis, code, links, images, raw HTML,
/// entities). CommonMark resolves each `\<punct>` back to the literal
/// character, so the caption shows the title verbatim.
fn escape_alt_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(
            c,
            '\\' | '`' | '*' | '_' | '[' | ']' | '(' | ')' | '<' | '>' | '&' | '!' | '~'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// Diagram types that have a `title` statement of their own. Anything else
/// (e.g. a flowchart node called `title`) is never scanned, so ordinary
/// diagram content cannot be mistaken for a title.
const TITLED_DIAGRAMS: &[&str] = &[
    "sequenceDiagram",
    "gantt",
    "pie",
    "journey",
    "timeline",
    "quadrantChart",
    "xychart-beta",
];

/// The diagram's title as written in its Mermaid source (REQ-PDF-051), in
/// priority order:
///
/// 1. the top-level `title:` of the leading YAML frontmatter (`---` … `---`,
///    Mermaid v10+, every diagram type); keys nested under `config:` do not
///    count;
/// 2. the diagram's own `title` statement (`title X` / `title: X`, and the
///    inline `pie showData title X` form), for the diagram types in
///    [`TITLED_DIAGRAMS`].
///
/// `%%` comment lines and `%%{ … }%%` directives are skipped. Pure source
/// text analysis: no `mmdc` involved. Blank titles count as absent, so the
/// caller falls back to its generic caption.
pub fn extract_title(source: &str) -> Option<String> {
    let mut lines = source.lines().peekable();
    while lines.next_if(|l| l.trim().is_empty()).is_some() {}

    let mut front_title = None;
    if lines.next_if(|l| l.trim_end() == "---").is_some() {
        let mut frontmatter = Vec::new();
        let mut closed = false;
        for line in lines.by_ref() {
            if line.trim_end() == "---" {
                closed = true;
                break;
            }
            frontmatter.push(line);
        }
        if !closed {
            // Unterminated frontmatter is not frontmatter; Mermaid itself
            // rejects such a source, so there is no title to report.
            return None;
        }
        front_title = frontmatter_title(&frontmatter);
    }
    if front_title.is_some() {
        return front_title;
    }

    let mut in_directive = false;
    let mut seen_header = false;
    for line in lines {
        let trimmed = line.trim();
        if in_directive {
            in_directive = !trimmed.contains("}%%");
            continue;
        }
        if trimmed.is_empty() || trimmed.starts_with("%%") && !trimmed.starts_with("%%{") {
            continue;
        }
        if trimmed.starts_with("%%{") {
            in_directive = !trimmed.contains("}%%");
            continue;
        }
        if !seen_header {
            // First statement line: the diagram type keyword.
            let keyword = trimmed.split_whitespace().next().unwrap_or("");
            if !TITLED_DIAGRAMS.contains(&keyword) {
                return None;
            }
            seen_header = true;
            if keyword == "pie" {
                if let Some(title) = pie_header_title(trimmed) {
                    return Some(title);
                }
            }
            continue;
        }
        if let Some(title) = title_statement(trimmed) {
            return Some(title);
        }
    }
    None
}

/// Value of the top-level (unindented) `title:` key of a YAML frontmatter.
fn frontmatter_title(lines: &[&str]) -> Option<String> {
    for line in lines {
        let Some(value) = line.strip_prefix("title:") else {
            continue;
        };
        return clean_title(&unquote_yaml_scalar(value.trim()));
    }
    None
}

/// Plain YAML scalar value: strips matching quotes (handling their escapes) or,
/// for an unquoted value, a trailing ` #` comment.
fn unquote_yaml_scalar(value: &str) -> String {
    if let Some(inner) = value
        .strip_prefix('"')
        .and_then(|v| v.rfind('"').map(|end| &v[..end]))
    {
        return inner.replace("\\\"", "\"").replace("\\\\", "\\");
    }
    if let Some(inner) = value
        .strip_prefix('\'')
        .and_then(|v| v.rfind('\'').map(|end| &v[..end]))
    {
        return inner.replace("''", "'");
    }
    match value.find(" #") {
        Some(at) => value[..at].to_string(),
        None => value.to_string(),
    }
}

/// `title` for a trimmed `title X` / `title: X` statement line.
fn title_statement(trimmed: &str) -> Option<String> {
    let rest = trimmed.strip_prefix("title")?;
    let rest = match rest.chars().next() {
        Some(':') => &rest[1..],
        Some(c) if c.is_whitespace() => rest,
        // `titled`, `title-x`, or a bare `title` with nothing after it.
        _ => return None,
    };
    clean_title(rest)
}

/// Inline title of a `pie [showData] title X` header line.
fn pie_header_title(header: &str) -> Option<String> {
    let mut words = header.split_whitespace();
    words.next()?; // `pie`
    let mut rest = header.trim_start().strip_prefix("pie")?.trim_start();
    for word in words {
        if word == "title" {
            return title_statement(rest);
        }
        rest = rest.strip_prefix(word)?.trim_start();
    }
    None
}

/// Collapses whitespace runs to single spaces; blank -> `None`.
fn clean_title(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    (!collapsed.is_empty()).then_some(collapsed)
}

/// Run `mmdc` for one diagram. The temporary `.mmd` input is always removed;
/// on any failure a partially written SVG is removed too.
fn render_block(
    mmdc: &Path,
    source: &str,
    svg_path: &Path,
    timeout: Duration,
) -> Result<(), String> {
    if source.trim().is_empty() {
        return Err("diagram is empty".to_string());
    }
    let cache_dir = svg_path
        .parent()
        .ok_or_else(|| format!("invalid output path {}", svg_path.display()))?;
    std::fs::create_dir_all(cache_dir)
        .map_err(|e| format!("cannot create {}: {e}", cache_dir.display()))?;

    let input_path = svg_path.with_extension("mmd");
    std::fs::write(&input_path, source)
        .map_err(|e| format!("cannot write {}: {e}", input_path.display()))?;
    let config_path = svg_path.with_extension("config.json");
    let outcome = std::fs::write(&config_path, MMDC_CONFIG_JSON)
        .map_err(|e| format!("cannot write {}: {e}", config_path.display()))
        .and_then(|()| run_mmdc(mmdc, &input_path, &config_path, svg_path, timeout));
    // Best effort: input and config are scratch files inside the cache dir; a
    // removal failure only leaves strays and must not mask `outcome`.
    let _ = std::fs::remove_file(&input_path);
    let _ = std::fs::remove_file(&config_path);
    if outcome.is_err() {
        // Same reasoning: drop a possibly half-written SVG, ignore if absent.
        let _ = std::fs::remove_file(svg_path);
    }
    outcome
}

fn run_mmdc(
    mmdc: &Path,
    input: &Path,
    config: &Path,
    output: &Path,
    timeout: Duration,
) -> Result<(), String> {
    let mut child = Command::new(mmdc)
        .arg("-i")
        .arg(input)
        .arg("-o")
        .arg(output)
        .arg("-q")
        .arg("-c")
        .arg(config)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", mmdc.display()))?;

    // Drain stderr on a thread so a chatty child cannot block on a full pipe
    // while we poll for exit.
    let mut stderr = child.stderr.take();
    let reader = std::thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(pipe) = stderr.as_mut() {
            // A read error just means we quote less of stderr in the warning.
            let _ = pipe.read_to_end(&mut buf);
        }
        String::from_utf8_lossy(&buf).into_owned()
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() >= deadline => {
                // Kill/reap failures only matter if the process lingers; the
                // timeout error below is what the caller needs to see.
                let _ = child.kill();
                let _ = child.wait();
                // Deliberately not joining `reader`: a grandchild (mmdc spawns
                // a headless browser) that inherited the stderr pipe keeps it
                // open and would turn the timeout into a hang. The detached
                // reader ends when the pipe finally closes.
                return Err(format!("mmdc timed out after {}s", timeout.as_secs_f32()));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => return Err(format!("waiting for mmdc failed: {e}")),
        }
    };
    // Ends once mmdc (and any grandchildren) close stderr.
    let stderr_text = reader.join().unwrap_or_default();

    if !status.success() {
        let detail = summarize(&stderr_text);
        return Err(if detail.is_empty() {
            format!("mmdc exited with {status}")
        } else {
            format!("mmdc exited with {status}: {detail}")
        });
    }
    match std::fs::metadata(output) {
        Ok(m) if m.len() > 0 => Ok(()),
        _ => Err("mmdc reported success but produced no SVG".to_string()),
    }
}

/// Collapse stderr to one bounded line for use in a warning.
fn summarize(stderr: &str) -> String {
    let joined = stderr.split_whitespace().collect::<Vec<_>>().join(" ");
    if joined.chars().count() <= MAX_REASON_LEN {
        return joined;
    }
    let cut: String = joined.chars().take(MAX_REASON_LEN).collect();
    format!("{cut}…")
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- find_mermaid_blocks -------------------------------------------

    #[test]
    fn finds_single_block_with_source_and_range() {
        let md = "intro\n\n```mermaid\nflowchart TD\n  A-->B\n```\n\noutro\n";
        let blocks = find_mermaid_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].source, "flowchart TD\n  A-->B\n");
        assert_eq!(
            &md[blocks[0].range.clone()],
            "```mermaid\nflowchart TD\n  A-->B\n```"
        );
    }

    #[test]
    fn finds_multiple_blocks_in_order_and_tilde_fences() {
        let md = "```mermaid\nA\n```\ntext\n~~~mermaid\nB\n~~~\n";
        let blocks = find_mermaid_blocks(md);
        let sources: Vec<_> = blocks.iter().map(|b| b.source.as_str()).collect();
        assert_eq!(sources, ["A\n", "B\n"]);
    }

    #[test]
    fn ignores_other_languages_and_prose_mentions() {
        let md =
            "```rust\nfn main() {}\n```\n\nUse the word mermaid here.\n\n```mermaidx\nA\n```\n";
        assert!(find_mermaid_blocks(md).is_empty());
    }

    #[test]
    fn ignores_mermaid_fence_nested_inside_longer_fence() {
        let md = "````markdown\n```mermaid\nA-->B\n```\n````\n";
        assert!(find_mermaid_blocks(md).is_empty());
    }

    #[test]
    fn info_string_is_case_insensitive_and_takes_first_word() {
        let md = "```Mermaid title=x\nA\n```\n";
        assert_eq!(find_mermaid_blocks(md).len(), 1);
    }

    #[test]
    fn finds_block_inside_list_item_with_indent_stripped() {
        let md = "- item\n\n  ```mermaid\n  A-->B\n  ```\n";
        let blocks = find_mermaid_blocks(md);
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].source, "A-->B\n");
    }

    #[test]
    fn empty_document_has_no_blocks() {
        assert!(find_mermaid_blocks("").is_empty());
    }

    // ---- extract_title (REQ-PDF-051) -------------------------------------

    fn title_of(source: &str) -> Option<String> {
        extract_title(source)
    }

    #[test]
    fn title_from_yaml_frontmatter() {
        let src = "---\ntitle: Login flow\n---\nflowchart TD\n  A-->B\n";
        assert_eq!(title_of(src).as_deref(), Some("Login flow"));
    }

    #[test]
    fn frontmatter_title_unquotes_values() {
        assert_eq!(
            title_of("---\ntitle: \"A: B\"\n---\nflowchart TD\n").as_deref(),
            Some("A: B")
        );
        assert_eq!(
            title_of("---\ntitle: 'It''s'\n---\nflowchart TD\n").as_deref(),
            Some("It's")
        );
    }

    #[test]
    fn frontmatter_title_ignores_trailing_yaml_comment() {
        let src = "---\ntitle: Plain # note\n---\nflowchart TD\n";
        assert_eq!(title_of(src).as_deref(), Some("Plain"));
    }

    #[test]
    fn frontmatter_may_follow_blank_lines() {
        let src = "\n\n---\ntitle: T\n---\nflowchart TD\n";
        assert_eq!(title_of(src).as_deref(), Some("T"));
    }

    #[test]
    fn title_inside_frontmatter_config_is_not_extracted() {
        let src = "---\nconfig:\n  title: nope\n  theme: dark\n---\nflowchart TD\n";
        assert_eq!(title_of(src), None);
        let src = "---\nconfig:\n  title: nope\ntitle: Real\n---\nflowchart TD\n";
        assert_eq!(title_of(src).as_deref(), Some("Real"));
    }

    #[test]
    fn title_from_diagram_specific_line() {
        assert_eq!(
            title_of("sequenceDiagram\n  title: Auth handshake\n  A->>B: hi\n").as_deref(),
            Some("Auth handshake")
        );
        assert_eq!(
            title_of("gantt\n  title Release plan\n  section S\n").as_deref(),
            Some("Release plan")
        );
        assert_eq!(
            title_of("pie\n  title Pets adopted\n  \"Dogs\" : 386\n").as_deref(),
            Some("Pets adopted")
        );
        assert_eq!(
            title_of("journey\n  title My day\n  section Work\n").as_deref(),
            Some("My day")
        );
        assert_eq!(
            title_of("timeline\n  title History\n  2002 : LinkedIn\n").as_deref(),
            Some("History")
        );
    }

    #[test]
    fn pie_header_inline_title() {
        assert_eq!(
            title_of("pie showData title Key elements\n  \"A\" : 1\n").as_deref(),
            Some("Key elements")
        );
        assert_eq!(
            title_of("pie title Pets\n  \"A\" : 1\n").as_deref(),
            Some("Pets")
        );
    }

    #[test]
    fn frontmatter_title_wins_over_diagram_title() {
        let src = "---\ntitle: Front\n---\ngantt\n  title Body\n";
        assert_eq!(title_of(src).as_deref(), Some("Front"));
    }

    #[test]
    fn no_title_yields_none() {
        assert_eq!(title_of("flowchart TD\n  A-->B\n"), None);
        assert_eq!(title_of(""), None);
    }

    #[test]
    fn empty_title_yields_none() {
        assert_eq!(title_of("---\ntitle:\n---\nflowchart TD\n"), None);
        assert_eq!(title_of("---\ntitle: \"  \"\n---\nflowchart TD\n"), None);
        assert_eq!(title_of("gantt\n  title:   \n  section S\n"), None);
    }

    #[test]
    fn empty_frontmatter_title_falls_back_to_diagram_title() {
        let src = "---\ntitle:\n---\ngantt\n  title Body\n";
        assert_eq!(title_of(src).as_deref(), Some("Body"));
    }

    #[test]
    fn commented_title_lines_are_ignored() {
        assert_eq!(title_of("gantt\n  %% title Hidden\n  section S\n"), None);
        let src = "gantt\n  %% title Hidden\n  title Shown\n";
        assert_eq!(title_of(src).as_deref(), Some("Shown"));
    }

    #[test]
    fn title_inside_directive_block_is_ignored() {
        let src = "%%{init: {\"title\": 1}}%%\ngantt\n  section S\n";
        assert_eq!(title_of(src), None);
        let src = "%%{\n  init: {\n  title: nope\n  }\n}%%\ngantt\n  title Yes\n";
        assert_eq!(title_of(src).as_deref(), Some("Yes"));
    }

    #[test]
    fn title_lines_of_diagrams_without_title_syntax_are_ignored() {
        // A flowchart node that happens to be called `title`.
        assert_eq!(title_of("flowchart TD\n  title --> B\n"), None);
        assert_eq!(title_of("flowchart TD\n  title: X\n"), None);
    }

    #[test]
    fn title_word_prefix_is_not_a_title_line() {
        assert_eq!(title_of("gantt\n  titled Foo\n  section S\n"), None);
    }

    #[test]
    fn title_whitespace_is_collapsed() {
        assert_eq!(
            title_of("gantt\n  title   A \t  B  \n").as_deref(),
            Some("A B")
        );
    }

    #[test]
    fn find_mermaid_blocks_sets_title() {
        let md = "```mermaid\n---\ntitle: First\n---\nflowchart TD\n```\n\n```mermaid\nflowchart TD\n```\n";
        let blocks = find_mermaid_blocks(md);
        assert_eq!(blocks[0].title.as_deref(), Some("First"));
        assert_eq!(blocks[1].title, None);
    }

    // ---- image_reference (REQ-PDF-051) -----------------------------------

    #[test]
    fn image_reference_uses_title_or_fallback() {
        let p = Path::new("/c/a.svg");
        assert_eq!(
            image_reference(2, Some("Login flow"), p).unwrap(),
            "![Login flow](/c/a.svg)"
        );
        assert_eq!(
            image_reference(2, None, p).unwrap(),
            "![Mermaid diagram 2](/c/a.svg)"
        );
    }

    #[test]
    fn image_reference_blank_title_falls_back() {
        let p = Path::new("/c/a.svg");
        assert_eq!(
            image_reference(3, Some("  "), p).unwrap(),
            "![Mermaid diagram 3](/c/a.svg)"
        );
    }

    #[test]
    fn image_reference_escapes_markdown_specials() {
        let p = Path::new("/c/a.svg");
        assert_eq!(
            image_reference(1, Some(r"A [b] *c* _d_ `e` <f> & g\h ![x](y)"), p).unwrap(),
            r"![A \[b\] \*c\* \_d\_ \`e\` \<f\> \& g\\h \!\[x\]\(y\)](/c/a.svg)"
        );
    }

    // ---- convert: mmdc missing -------------------------------------------

    #[test]
    fn missing_mmdc_keeps_markdown_and_warns_once() {
        let dir = tempfile::tempdir().unwrap();
        let md = "```mermaid\nA-->B\n```\n\n```mermaid\nC-->D\n```\n";
        let out = convert_mermaid_blocks_with(md, dir.path(), None, MMDC_TIMEOUT);
        assert_eq!(out.markdown, md);
        assert_eq!(
            out.warnings,
            vec![MermaidWarning::MmdcNotFound { blocks: 2 }]
        );
        assert!(out.generated_files().is_empty());
        let text = out.warnings[0].to_string();
        assert!(
            text.contains("mmdc") && text.contains("npm install"),
            "{text}"
        );
    }

    #[test]
    fn no_blocks_means_no_warning_even_without_mmdc() {
        let dir = tempfile::tempdir().unwrap();
        let md = "# just text\n";
        let out = convert_mermaid_blocks_with(md, dir.path(), None, MMDC_TIMEOUT);
        assert_eq!(out.markdown, md);
        assert!(out.warnings.is_empty());
    }

    // ---- convert with a fake mmdc ----------------------------------------

    #[cfg(unix)]
    mod fake_mmdc {
        use super::*;
        use std::os::unix::fs::PermissionsExt;

        /// Writes an executable shell script emulating `mmdc -i IN -o OUT -q`.
        fn script(dir: &Path, body: &str) -> PathBuf {
            let path = dir.join("fake-mmdc");
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }

        /// Succeeds unless the input contains `INVALID`; SVG embeds the source.
        const FAKE_OK: &str = r#"
in="$2"; out="$4"
if grep -q INVALID "$in"; then echo "Parse error on line 1" >&2; exit 1; fi
printf '<svg>%s</svg>' "$(cat "$in")" > "$out"
"#;

        fn run(md: &str, mmdc_body: &str) -> (tempfile::TempDir, PathBuf, MermaidOutput) {
            let tools = tempfile::tempdir().unwrap();
            let mmdc = script(tools.path(), mmdc_body);
            let cache = tools.path().join("cache");
            let out = convert_mermaid_blocks_with(md, &cache, Some(&mmdc), Duration::from_secs(5));
            (tools, cache, out)
        }

        #[test]
        fn renders_block_to_svg_and_replaces_with_image_reference() {
            let md = "before\n\n```mermaid\nflowchart TD\n```\n\nafter\n";
            let (_tools, cache, out) = run(md, FAKE_OK);

            assert!(out.warnings.is_empty(), "{:?}", out.warnings);
            assert_eq!(out.generated_files().len(), 1);
            let svg = &out.generated_files()[0];
            assert!(svg.starts_with(&cache) && svg.extension().unwrap() == "svg");
            assert!(std::fs::read_to_string(svg)
                .unwrap()
                .contains("flowchart TD"));
            assert_eq!(
                out.markdown,
                format!(
                    "before\n\n![Mermaid diagram 1]({})\n\nafter\n",
                    svg.display()
                )
            );
            assert!(!out.markdown.contains("```mermaid"));
        }

        /// Typst's SVG renderer ignores `<foreignObject>`, which is where
        /// mermaid puts HTML labels, so diagrams would embed with empty
        /// boxes. `mmdc` must be told to emit plain SVG `<text>` labels.
        #[test]
        fn mmdc_is_configured_for_svg_text_labels() {
            let body = r#"
in="$2"; out="$4"; cfg=""
while [ $# -gt 0 ]; do case "$1" in -c) cfg="$2"; shift 2;; *) shift;; esac; done
if [ -z "$cfg" ]; then echo "no -c config" >&2; exit 1; fi
cp "$cfg" "$out"
"#;
            let (_tools, cache, out) = run("```mermaid\nA\n```\n", body);
            assert!(out.warnings.is_empty(), "{:?}", out.warnings);
            let cfg: serde_json::Value =
                serde_json::from_str(&std::fs::read_to_string(&out.generated_files()[0]).unwrap())
                    .unwrap();
            assert_eq!(cfg["htmlLabels"], false);
            assert_eq!(cfg["flowchart"]["htmlLabels"], false);
            // The config file is scratch, like the .mmd input.
            assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
        }

        #[test]
        fn title_becomes_figure_caption_end_to_end() {
            let md = "```mermaid\n---\ntitle: \"Login *flow* [v2]\"\n---\nflowchart TD\n```\n\n```mermaid\nflowchart TD\n```\n";
            let (_tools, _cache, out) = run(md, FAKE_OK);
            assert!(out.warnings.is_empty(), "{:?}", out.warnings);
            let root = Path::new("/");
            let typst = crate::storage::docs::pdf::markdown_to_typst::markdown_to_typst(
                &out.markdown,
                &crate::storage::docs::pdf::markdown_to_typst::ConvertOptions::new(root),
            )
            .typst;
            assert!(
                typst.contains("caption: [Login \\*flow\\* \\[v2\\]]"),
                "{typst}"
            );
            assert!(typst.contains("caption: [Mermaid diagram 2]"), "{typst}");
        }

        #[test]
        fn missing_mmdc_leaves_titled_block_untouched() {
            let dir = tempfile::tempdir().unwrap();
            let md = "```mermaid\n---\ntitle: T\n---\nflowchart TD\n```\n";
            let out = convert_mermaid_blocks_with(md, dir.path(), None, MMDC_TIMEOUT);
            assert_eq!(out.markdown, md);
        }

        #[test]
        fn temporary_mmd_input_is_removed_after_render() {
            let (_tools, cache, out) = run("```mermaid\nA\n```\n", FAKE_OK);
            let names: Vec<_> = std::fs::read_dir(&cache)
                .unwrap()
                .map(|e| e.unwrap().path())
                .collect();
            assert_eq!(names, out.generated_files());
        }

        #[test]
        fn invalid_block_stays_code_block_others_convert() {
            let md = "```mermaid\nINVALID syntax\n```\n\n```mermaid\nOK\n```\n";
            let (_tools, cache, out) = run(md, FAKE_OK);

            assert_eq!(out.generated_files().len(), 1);
            assert!(out.markdown.contains("```mermaid\nINVALID syntax\n```\n"));
            assert!(out.markdown.contains("![Mermaid diagram 2]("));
            assert_eq!(out.warnings.len(), 1);
            match &out.warnings[0] {
                MermaidWarning::RenderFailed { index, reason } => {
                    assert_eq!(*index, 1);
                    assert!(reason.contains("Parse error"), "{reason}");
                }
                other => panic!("unexpected warning {other:?}"),
            }
            // Failed block leaves nothing behind (no .mmd, no partial svg).
            assert_eq!(std::fs::read_dir(&cache).unwrap().count(), 1);
        }

        #[test]
        fn empty_diagram_is_not_sent_to_mmdc() {
            let md = "```mermaid\n```\n";
            let (_tools, _cache, out) = run(md, "exit 99");
            assert_eq!(out.markdown, md);
            assert_eq!(
                out.warnings,
                vec![MermaidWarning::RenderFailed {
                    index: 1,
                    reason: "diagram is empty".to_string()
                }]
            );
        }

        #[test]
        fn success_without_output_file_is_a_failure() {
            let (_tools, _cache, out) = run("```mermaid\nA\n```\n", "exit 0");
            assert!(out.generated_files().is_empty());
            assert!(out.markdown.contains("```mermaid"));
            assert!(matches!(
                &out.warnings[0],
                MermaidWarning::RenderFailed { reason, .. } if reason.contains("no SVG")
            ));
        }

        #[test]
        fn unrunnable_mmdc_is_reported_not_panicked() {
            let dir = tempfile::tempdir().unwrap();
            let bogus = dir.path().join("does-not-exist");
            let out = convert_mermaid_blocks_with(
                "```mermaid\nA\n```\n",
                &dir.path().join("cache"),
                Some(&bogus),
                Duration::from_secs(5),
            );
            assert!(out.markdown.contains("```mermaid"));
            assert!(matches!(
                &out.warnings[0],
                MermaidWarning::RenderFailed { reason, .. } if reason.contains("cannot start")
            ));
        }

        #[test]
        fn hung_mmdc_is_killed_after_timeout() {
            let tools = tempfile::tempdir().unwrap();
            let mmdc = script(tools.path(), "exec sleep 30");
            let started = Instant::now();
            let out = convert_mermaid_blocks_with(
                "```mermaid\nA\n```\n",
                &tools.path().join("cache"),
                Some(&mmdc),
                Duration::from_millis(300),
            );
            assert!(started.elapsed() < Duration::from_secs(10));
            assert!(matches!(
                &out.warnings[0],
                MermaidWarning::RenderFailed { reason, .. } if reason.contains("timed out")
            ));
            assert!(out.markdown.contains("```mermaid"));
        }

        #[test]
        fn timeout_fires_near_deadline_even_when_grandchild_holds_stderr() {
            // The shell is killed at the deadline, but its `sleep` child keeps
            // the inherited stderr pipe open for 30s. The timeout must not
            // wait for that pipe to close.
            let tools = tempfile::tempdir().unwrap();
            let mmdc = script(tools.path(), "sleep 30 &\nwait");
            let started = Instant::now();
            let out = convert_mermaid_blocks_with(
                "```mermaid\nA\n```\n",
                &tools.path().join("cache"),
                Some(&mmdc),
                Duration::from_secs(1),
            );
            let elapsed = started.elapsed();
            assert!(
                elapsed >= Duration::from_secs(1) && elapsed < Duration::from_secs(4),
                "timed out after {elapsed:?}, expected about 1s"
            );
            assert!(matches!(
                &out.warnings[0],
                MermaidWarning::RenderFailed { reason, .. } if reason.contains("timed out")
            ));
            assert!(out.markdown.contains("```mermaid"));
        }

        #[test]
        fn stderr_in_warning_is_truncated() {
            let body = r#"yes ERRORLINE | head -n 5000 >&2; exit 1"#;
            let (_tools, _cache, out) = run("```mermaid\nA\n```\n", body);
            match &out.warnings[0] {
                MermaidWarning::RenderFailed { reason, .. } => {
                    assert!(
                        reason.chars().count() < MAX_REASON_LEN + 100,
                        "{}",
                        reason.len()
                    );
                    assert!(reason.ends_with('…'));
                }
                other => panic!("unexpected warning {other:?}"),
            }
        }

        #[test]
        fn cache_path_with_spaces_uses_angle_bracket_destination() {
            let tools = tempfile::tempdir().unwrap();
            let mmdc = script(tools.path(), FAKE_OK);
            let cache = tools.path().join("my cache");
            let out = convert_mermaid_blocks_with(
                "```mermaid\nA\n```\n",
                &cache,
                Some(&mmdc),
                Duration::from_secs(5),
            );
            assert!(out.warnings.is_empty(), "{:?}", out.warnings);
            let svg = out.generated_files()[0].display().to_string();
            assert_eq!(out.markdown, format!("![Mermaid diagram 1](<{svg}>)\n"));
        }

        #[test]
        fn unrepresentable_cache_path_falls_back_to_code_block_and_cleans_up() {
            let tools = tempfile::tempdir().unwrap();
            let mmdc = script(tools.path(), FAKE_OK);
            let cache = tools.path().join("bad<dir");
            let md = "```mermaid\nA\n```\n";
            let mut out =
                convert_mermaid_blocks_with(md, &cache, Some(&mmdc), Duration::from_secs(5));
            assert_eq!(out.markdown, md);
            assert!(matches!(
                &out.warnings[0],
                MermaidWarning::RenderFailed { .. }
            ));
            // The SVG was produced, so it must still be tracked for cleanup.
            assert_eq!(out.generated_files().len(), 1);
            out.cleanup().unwrap();
            assert!(!cache.exists());
        }

        #[test]
        fn block_in_list_item_keeps_surrounding_structure() {
            let md = "- item\n\n  ```mermaid\n  A\n  ```\n- next\n";
            let (_tools, _cache, out) = run(md, FAKE_OK);
            assert!(out.warnings.is_empty());
            assert!(out
                .markdown
                .starts_with("- item\n\n  ![Mermaid diagram 1]("));
            assert!(out.markdown.ends_with(")\n- next\n"));
        }

        #[test]
        fn block_without_trailing_newline_at_eof() {
            let (_tools, _cache, out) = run("```mermaid\nA\n```", FAKE_OK);
            assert!(out.warnings.is_empty());
            assert!(out.markdown.starts_with("![Mermaid diagram 1]("));
            assert!(!out.markdown.ends_with('\n'));
        }

        // ---- cleanup (AC3) -----------------------------------------------

        #[test]
        fn cleanup_removes_svgs_and_empty_cache_dir_and_is_idempotent() {
            let (_tools, cache, mut out) = run("```mermaid\nA\n```\n", FAKE_OK);
            let svg = out.generated_files()[0].clone();
            assert!(svg.exists());
            out.cleanup().unwrap();
            assert!(!svg.exists());
            assert!(!cache.exists());
            assert!(out.generated_files().is_empty());
            out.cleanup().unwrap();
        }

        #[test]
        fn cleanup_keeps_foreign_files_in_cache_dir() {
            let (_tools, cache, mut out) = run("```mermaid\nA\n```\n", FAKE_OK);
            let foreign = cache.join("other-export.svg");
            std::fs::write(&foreign, "x").unwrap();
            out.cleanup().unwrap();
            assert!(foreign.exists());
            assert!(!out.generated_files().iter().any(|p| p.exists()));
        }

        #[test]
        fn dropping_output_removes_generated_files() {
            let (_tools, _cache, out) = run("```mermaid\nA\n```\n", FAKE_OK);
            let svg = out.generated_files()[0].clone();
            assert!(svg.exists());
            drop(out);
            assert!(!svg.exists());
        }

        #[test]
        fn cleanup_tolerates_already_deleted_svg() {
            let (_tools, _cache, mut out) = run("```mermaid\nA\n```\n", FAKE_OK);
            std::fs::remove_file(&out.generated_files()[0]).unwrap();
            out.cleanup().unwrap();
        }
    }

    #[test]
    fn cache_dir_is_under_handoff_exports() {
        assert_eq!(
            mermaid_cache_dir(Path::new("/p/.handoff")),
            PathBuf::from("/p/.handoff/exports/.mermaid-cache")
        );
    }
}

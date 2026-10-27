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

use std::ffi::OsStr;
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

/// Executable name looked up on `PATH`.
pub const MMDC_BINARY: &str = "mmdc";

/// Upper bound for one `mmdc` invocation (it launches a headless browser).
pub const MMDC_TIMEOUT: Duration = Duration::from_secs(60);

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
                "mmdc (Mermaid CLI) not found on PATH: {blocks} mermaid block(s) left as code \
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
                    blocks.push(MermaidBlock { range, source });
                }
            }
            _ => {}
        }
    }
    blocks
}

/// Resolve `name` on a `PATH`-style variable (`which` semantics: first
/// executable regular file wins). `None` path variable means not found.
pub fn find_in_path(name: &str, path_var: Option<&OsStr>) -> Option<PathBuf> {
    let path_var = path_var?;
    std::env::split_paths(path_var)
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// Locate `mmdc` on the process `PATH`.
pub fn find_mmdc() -> Option<PathBuf> {
    find_in_path(MMDC_BINARY, std::env::var_os("PATH").as_deref())
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Convert every mermaid block in `markdown` using the `mmdc` found on
/// `PATH`. See [`convert_mermaid_blocks_with`].
pub fn convert_mermaid_blocks(markdown: &str, cache_dir: &Path) -> MermaidOutput {
    convert_mermaid_blocks_with(markdown, cache_dir, find_mmdc().as_deref(), MMDC_TIMEOUT)
}

/// Convert every mermaid block in `markdown`.
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
                match image_reference(index, &svg_path) {
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

/// Markdown image reference for a rendered diagram. Paths containing
/// whitespace or parentheses use the `<…>` destination form; paths that no
/// Markdown destination can express are rejected.
fn image_reference(index: usize, svg_path: &Path) -> Result<String, String> {
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
    Ok(format!("![Mermaid diagram {index}]({destination})"))
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
    let outcome = run_mmdc(mmdc, &input_path, svg_path, timeout);
    // Best effort: the input is a scratch file inside the cache dir; a
    // removal failure only leaves a stray .mmd and must not mask `outcome`.
    let _ = std::fs::remove_file(&input_path);
    if outcome.is_err() {
        // Same reasoning: drop a possibly half-written SVG, ignore if absent.
        let _ = std::fs::remove_file(svg_path);
    }
    outcome
}

fn run_mmdc(mmdc: &Path, input: &Path, output: &Path, timeout: Duration) -> Result<(), String> {
    let mut child = Command::new(mmdc)
        .arg("-i")
        .arg(input)
        .arg("-o")
        .arg(output)
        .arg("-q")
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
            Ok(Some(status)) => break Ok(status),
            Ok(None) if Instant::now() >= deadline => {
                // Kill/reap failures only matter if the process lingers; the
                // timeout error below is what the caller needs to see.
                let _ = child.kill();
                let _ = child.wait();
                break Err(format!("mmdc timed out after {}s", timeout.as_secs_f32()));
            }
            Ok(None) => std::thread::sleep(POLL_INTERVAL),
            Err(e) => break Err(format!("waiting for mmdc failed: {e}")),
        }
    };
    // The thread ends once the child (and any grandchildren) close stderr.
    let stderr_text = reader.join().unwrap_or_default();

    let status = status?;
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

    // ---- find_in_path ----------------------------------------------------

    #[cfg(unix)]
    #[test]
    fn find_in_path_returns_first_executable_match() {
        use std::os::unix::fs::PermissionsExt;
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        // Non-executable in a/, executable in b/ -> b/ wins.
        std::fs::write(a.path().join("mmdc"), "x").unwrap();
        let exe = b.path().join("mmdc");
        std::fs::write(&exe, "x").unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();

        let path = std::env::join_paths([a.path(), b.path()]).unwrap();
        assert_eq!(find_in_path("mmdc", Some(&path)), Some(exe));
    }

    #[test]
    fn find_in_path_none_when_missing_or_unset() {
        let dir = tempfile::tempdir().unwrap();
        let path = std::env::join_paths([dir.path()]).unwrap();
        assert_eq!(find_in_path("mmdc", Some(&path)), None);
        assert_eq!(find_in_path("mmdc", None), None);
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

//! YAML frontmatter <-> [`DocMetadata`] serialize/deserialize (frontmatter
//! migration, t123.1) plus single-file read/write helpers for
//! `_doc.<slug>.md` (frontmatter + body, replacing the old
//! `_doc.<slug>.json` + `_doc.<slug>.md` pair).
//!
//! `sections[]` is deliberately never part of the frontmatter shape: byte
//! offsets are computed fresh from the body on every read (t123.2), so they
//! can never go stale after a manual edit. `version` (schema version) and
//! `slug` (derived from the filename) are likewise omitted from the
//! frontmatter body — both are storage-layer bookkeeping, not document
//! content.

use std::collections::HashMap;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{DocMetadata, DocRelation, DocSource, Verification, DOC_SCHEMA_VERSION};

/// YAML-facing mirror of [`DocMetadata`], minus `version`, `slug`, and
/// `sections` (see module docs), plus alias support on the handful of
/// fields the frontmatter spec defines common aliases for. This is a
/// separate type from `DocMetadata` (rather than reusing it directly with
/// `#[serde(skip)]`) so YAML aliases don't leak into the JSON-era shape any
/// other code still round-trips through `serde_json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct FrontmatterDoc {
    id: String,
    #[serde(alias = "name")]
    title: String,
    doc_type: String,
    #[serde(default)]
    tags: Vec<String>,
    #[serde(default)]
    scope_paths: Vec<String>,
    #[serde(default)]
    parent_id: Option<String>,
    #[serde(default)]
    children: Vec<String>,
    #[serde(default)]
    related: Vec<DocRelation>,
    #[serde(default = "default_auto_inject")]
    auto_inject: String,
    #[serde(default)]
    task_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    trace_profile: Option<String>,
    #[serde(default)]
    source: FrontmatterSource,
    #[serde(default)]
    has_bom: bool,
    #[serde(default = "default_line_ending")]
    line_ending: String,
    #[serde(default = "default_split_level")]
    split_level: u8,
    #[serde(alias = "date", alias = "created", alias = "publishDate")]
    created_at: String,
    #[serde(alias = "lastmod", alias = "modified", alias = "last_update")]
    updated_at: String,
    #[serde(default)]
    content_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verification: Option<Verification>,
    /// `description` accepts several common frontmatter aliases on read;
    /// write always emits the canonical `description` key. Not a field on
    /// `DocMetadata` itself (no storage-layer concept of "description" yet)
    /// — captured here purely so a value under any alias round-trips into
    /// `extra["description"]` instead of being silently dropped.
    #[serde(
        default,
        alias = "excerpt",
        alias = "summary",
        alias = "abstract",
        skip_serializing_if = "Option::is_none"
    )]
    description: Option<String>,

    /// Every YAML key not covered by a named field above, preserved for
    /// round-trip fidelity.
    #[serde(flatten)]
    extra: HashMap<String, Value>,
}

fn default_auto_inject() -> String {
    "auto".to_string()
}

fn default_line_ending() -> String {
    "lf".to_string()
}

fn default_split_level() -> u8 {
    super::split::DEFAULT_SPLIT_LEVEL
}

/// `source:` sub-block in frontmatter. Deliberately excludes
/// `DocSource::frontmatter`/`frontmatter_trailing_eol` (see module docs
/// header and the `t123.1` design note: these two fields existed only to
/// stash a stripped *user* frontmatter block under the old 2-file format;
/// in the new format the frontmatter itself IS the metadata, so there's
/// nothing left to stash).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct FrontmatterSource {
    #[serde(default)]
    origin: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    original_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    canonical_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    body_raw_hash: Option<String>,
    /// M2 (wiki/260-vmodel-m2-design.md E7, M2-04) — see
    /// `DocSource::layer_sync_stamp`'s doc comment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    layer_sync_stamp: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    content_hash_scheme: Option<u32>,
}

impl TryFrom<&DocMetadata> for FrontmatterDoc {
    type Error = anyhow::Error;

    /// Fails when `doc.content_hash` is `None` — the on-disk frontmatter
    /// schema's `content_hash` is a plain, always-present `String` (P-M1,
    /// wiki/240-performance-design.md §4, t370.8: `DocMetadata.content_hash`
    /// is lazily `Option<String>` in memory, but a value must always be
    /// computed before it reaches disk). Callers write through
    /// `super::write_doc_with_body`, which fills the hash in if still
    /// missing — reaching this error means that invariant was bypassed,
    /// which must fail loudly rather than silently persist an empty hash.
    fn try_from(doc: &DocMetadata) -> Result<Self> {
        let content_hash = doc.content_hash.clone().ok_or_else(|| {
            anyhow::anyhow!(
                "cannot serialize frontmatter for document '{}': content_hash not computed \
                 (write through write_doc/write_doc_with_body, which compute it if missing)",
                doc.id
            )
        })?;
        let mut extra = doc.extra.clone();
        let description = extra
            .remove("description")
            .and_then(|v| v.as_str().map(str::to_string));
        Ok(FrontmatterDoc {
            id: doc.id.clone(),
            title: doc.title.clone(),
            doc_type: doc.doc_type.clone(),
            tags: doc.tags.clone(),
            scope_paths: doc.scope_paths.clone(),
            parent_id: doc.parent_id.clone(),
            children: doc.children.clone(),
            related: doc.related.clone(),
            auto_inject: doc.auto_inject.clone(),
            task_ids: doc.task_ids.clone(),
            layer: doc.layer.clone(),
            trace_profile: doc.trace_profile.clone(),
            source: FrontmatterSource {
                origin: doc.source.origin.clone(),
                original_path: doc.source.original_path.clone(),
                canonical_hash: doc.source.canonical_hash.clone(),
                body_raw_hash: doc.source.body_raw_hash.clone(),
                layer_sync_stamp: doc.source.layer_sync_stamp.clone(),
                content_hash_scheme: doc.source.content_hash_scheme,
            },
            has_bom: doc.has_bom,
            line_ending: doc.line_ending.clone(),
            split_level: doc.split_level,
            created_at: doc.created_at.clone(),
            updated_at: doc.updated_at.clone(),
            content_hash,
            verification: doc.verification.clone(),
            description,
            extra,
        })
    }
}

impl FrontmatterDoc {
    /// Converts back into a [`DocMetadata`], filling in the storage-layer
    /// fields (`version`, `slug`, `sections`) that don't live in
    /// frontmatter. `slug` is derived by the caller from the filename;
    /// `sections` is always computed fresh by [`super::split::compute_sections`]
    /// after this call.
    fn into_doc_metadata(mut self, slug: String) -> DocMetadata {
        if let Some(description) = self.description.take() {
            self.extra
                .insert("description".to_string(), Value::String(description));
        }
        DocMetadata {
            version: DOC_SCHEMA_VERSION,
            id: self.id,
            slug,
            title: self.title,
            doc_type: self.doc_type,
            tags: self.tags,
            scope_paths: self.scope_paths,
            parent_id: self.parent_id,
            children: self.children,
            related: self.related,
            auto_inject: self.auto_inject,
            task_ids: self.task_ids,
            layer: self.layer,
            trace_profile: self.trace_profile,
            source: DocSource {
                origin: self.source.origin,
                original_path: self.source.original_path,
                canonical_hash: self.source.canonical_hash,
                body_raw_hash: self.source.body_raw_hash,
                layer_sync_stamp: self.source.layer_sync_stamp,
                content_hash_scheme: self.source.content_hash_scheme,
                frontmatter: None,
                frontmatter_trailing_eol: true,
            },
            has_bom: self.has_bom,
            line_ending: self.line_ending,
            split_level: self.split_level,
            sections: Vec::new(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            // Trusted as-is here (this is the raw, low-level parse used
            // directly by `write_doc_body`'s "preserve existing frontmatter"
            // path) — `super::read_doc`/`read_doc_with_body` always
            // overwrite this immediately afterward via
            // `recompute_sections_and_hash` (P-M1, t370.8: `Some`/`None`
            // there depending on whether the caller asked for a hash), so
            // what this raw parse puts here doesn't matter for those paths.
            content_hash: Some(self.content_hash),
            verification: self.verification,
            extra: self.extra,
        }
    }
}

/// Converts `doc` into a YAML frontmatter string, **without** the enclosing
/// `---` fences (callers wrap it — see [`write_frontmatter_doc`]). Never
/// includes `sections[]`, `version`, or `slug` (see module docs). Fails if
/// `doc.content_hash` hasn't been computed yet (see
/// `FrontmatterDoc::try_from`'s doc comment).
pub fn serialize_frontmatter(doc: &DocMetadata) -> Result<String> {
    let fm = FrontmatterDoc::try_from(doc)?;
    let yaml = serde_yaml::to_string(&fm).context("Failed to serialize document frontmatter")?;
    let yaml = quote_yaml11_ambiguous_scalars(&yaml);

    // Self-check (§4.12, FR-804): confirm what was just generated can be
    // read back *before* it ever reaches disk. `write_frontmatter_doc` is
    // the only production writer of frontmatter, so a serializer bug here
    // (or a future field whose value serde_yaml can't round-trip) must fail
    // loudly at the point of writing — the whole point of E11 is that a
    // document silently becoming unreadable is a bug, not something to
    // discover later via `doc_list`'s `unreadable`.
    deserialize_frontmatter(&yaml, &doc.id).with_context(|| {
        format!(
            "self-check failed: frontmatter just generated for document '{}' does not parse \
             back (refusing to write it — this would silently produce an unreadable document)",
            doc.id
        )
    })?;

    Ok(yaml)
}

/// YAML 1.1 boolean spellings that YAML 1.2 (what `serde_yaml` itself
/// emits/reads) does **not** treat as booleans, but PyYAML — the reader
/// `handoff-vscode` and this crate's own conformance test use (§4.12) —
/// does. A plain (unquoted) scalar value that happens to spell one of these
/// verbatim would silently become a *different type* (bool, not string) the
/// instant a YAML-1.1 reader loads it — e.g. a tag literally named `no`.
/// Deliberately **excludes** `true`/`false` (and their case variants):
/// both YAML 1.1 and 1.2 agree those are booleans, so they are never
/// ambiguous between the two readers — and this crate does emit real `bool`
/// fields (e.g. `has_bom`) as bare `true`/`false`, which a blind text-level
/// quoting pass (see [`quote_line_if_ambiguous`]) cannot tell apart from a
/// *string* that happens to spell the same word; only truly divergent
/// spellings belong in this list. `serde_yaml` already quotes the other
/// YAML-1.2-ambiguous cases on its own (hex/octal-looking strings,
/// e-notation all-digit strings).
const YAML11_AMBIGUOUS_BOOLS: &[&str] = &[
    "y", "Y", "yes", "Yes", "YES", "n", "N", "no", "No", "NO", "on", "On", "ON", "off", "Off",
    "OFF",
];

/// Rewrites `yaml` (a `serde_yaml`-produced document) so every plain,
/// unquoted scalar value that exactly spells a [`YAML11_AMBIGUOUS_BOOLS`]
/// entry is single-quoted. Operates line-by-line because frontmatter is
/// always a flat, block-style document (mapping `key: value` lines and `-
/// value` sequence items, at most one level of nested `source:` mapping) —
/// a full YAML re-emit pass to handle arbitrary nesting/flow-style would be
/// needed for a general-purpose document, which frontmatter is not. A value
/// that is already quoted, or is only part of a larger scalar (e.g.
/// `description: it is no big deal`), is left untouched — only a line whose
/// *entire* value after the `key: `/`- ` marker matches one of these
/// spellings is rewritten.
fn quote_yaml11_ambiguous_scalars(yaml: &str) -> String {
    // `split('\n')` + `join("\n")` round-trips a string's exact newline
    // structure (including a trailing newline, which becomes a trailing
    // empty element that `join` reproduces as-is) — no separate
    // trailing-newline bookkeeping needed.
    yaml.split('\n')
        .map(quote_line_if_ambiguous)
        .collect::<Vec<_>>()
        .join("\n")
}

fn quote_line_if_ambiguous(line: &str) -> String {
    let trimmed = line.trim_start();
    let indent = &line[..line.len() - trimmed.len()];

    if let Some(rest) = trimmed.strip_prefix("- ") {
        if YAML11_AMBIGUOUS_BOOLS.contains(&rest) {
            return format!("{indent}- '{rest}'");
        }
        return line.to_string();
    }

    if let Some(colon) = trimmed.find(": ") {
        let (key, value_with_marker) = trimmed.split_at(colon);
        let value = &value_with_marker[2..];
        if YAML11_AMBIGUOUS_BOOLS.contains(&value) {
            return format!("{indent}{key}: '{value}'");
        }
    }
    line.to_string()
}

/// Structured detail behind a frontmatter YAML parse failure (FR-804, E11,
/// wiki/260-vmodel-m2-design.md §4.12) — carries the same information the
/// `anyhow::Error` returned by [`deserialize_frontmatter`] already displays
/// in its message, exposed as a distinct, downcast-able type so callers that
/// report `{slug, error, line}` (`doc_list`'s `unreadable`,
/// `handoff_doc_repair_frontmatter`) don't have to string-parse the display
/// text to recover the line number.
#[derive(Debug)]
pub struct FrontmatterParseError {
    pub message: String,
    pub line: Option<usize>,
}

impl std::fmt::Display for FrontmatterParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for FrontmatterParseError {}

/// Parses a YAML frontmatter block (the text between the `---` fences,
/// exclusive) back into a [`DocMetadata`]. `slug` is supplied by the caller
/// (derived from the filename, not stored in frontmatter itself).
///
/// On a YAML parse failure, the returned `anyhow::Error` wraps a
/// [`FrontmatterParseError`] (downcast-able via `error.downcast_ref`) that
/// carries the 1-based source line `serde_yaml` reported, when available —
/// this is what lets a corpus-wide scan (`read_all_docs_with_unreadable`,
/// `DocSet::load`) report *where* a document failed to parse, not just that
/// it did (FR-804, E11).
pub fn deserialize_frontmatter(yaml_str: &str, slug: &str) -> Result<DocMetadata> {
    let fm: FrontmatterDoc = match serde_yaml::from_str(yaml_str) {
        Ok(fm) => fm,
        Err(e) => {
            let line = e.location().map(|loc| loc.line());
            return Err(FrontmatterParseError {
                message: format!("Failed to parse document frontmatter as YAML: {e}"),
                line,
            }
            .into());
        }
    };
    Ok(fm.into_doc_metadata(slug.to_string()))
}

/// Splits a `_doc.<slug>.md` file's raw content into `(frontmatter_yaml,
/// body)`. Returns `None` for the frontmatter half when the content doesn't
/// start with a `---` fence (old-format body-only file, or a document that
/// somehow lost its frontmatter).
///
/// A leading UTF-8 BOM (`\u{feff}`) before the opening fence is stripped
/// first (§4.12/E11 "BOM 付きの開始フェンス"): some external editors/tools
/// write one, and without this the fence check below would never match at
/// all — a BOM-prefixed but otherwise perfectly standard document would be
/// silently treated as "no frontmatter" (a migration signal) rather than
/// read normally. This makes the read path itself tolerant of the BOM
/// (strictly more documents parse than before); it deliberately does *not*
/// change what gets written back — `write_frontmatter_doc` never emits one.
fn split_frontmatter_and_body(content: &str) -> (Option<&str>, &str) {
    let content = content.strip_prefix('\u{feff}').unwrap_or(content);
    let Some(after_open) = content.strip_prefix("---\n") else {
        return (None, content);
    };
    // Find the closing fence: a line that is exactly "---" on its own line.
    let mut search_from = 0usize;
    loop {
        let Some(rel_idx) = after_open[search_from..].find("\n---") else {
            return (None, content);
        };
        let idx = search_from + rel_idx;
        // `idx` points at the '\n' right before "---". The fence itself
        // starts at idx+1.
        let fence_start = idx + 1;
        let after_fence = &after_open[fence_start + 3..];
        // The closing fence line must end the line here: either end of
        // string, '\n', or '\r\n'.
        if after_fence.is_empty() {
            return (Some(&after_open[..idx]), "");
        }
        if let Some(rest) = after_fence.strip_prefix('\n') {
            return (Some(&after_open[..idx]), rest);
        }
        if let Some(rest) = after_fence.strip_prefix("\r\n") {
            return (Some(&after_open[..idx]), rest);
        }
        // Not actually a fence line (e.g. "----" or "--- foo") — keep
        // searching past it.
        search_from = fence_start + 3;
    }
}

/// Reads a `_doc.<slug>.md` file and splits it into `(metadata, body)`.
/// Returns `Ok(None)` when the file does not exist. Returns `Ok(Some((doc,
/// body)))` with `doc.sections` empty — callers must compute sections
/// on-demand from `body` (t123.2; see `super::read_doc`).
///
/// Returns `Err` when the file exists, starts with a `---` fence, but the
/// enclosed YAML fails to parse (corrupt frontmatter) — this is distinct
/// from "no frontmatter at all" (which is a migration signal handled by the
/// caller, not an error here).
pub fn read_frontmatter_doc(path: &Path, slug: &str) -> Result<Option<(DocMetadata, String)>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to read document: {}", path.display()))
        }
    };
    let (Some(fm_yaml), body) = split_frontmatter_and_body(&content) else {
        return Ok(None);
    };
    let doc = deserialize_frontmatter(fm_yaml, slug)?;
    Ok(Some((doc, body.to_string())))
}

/// Like [`read_frontmatter_doc`], but skips [`deserialize_frontmatter`]
/// entirely and returns only the body half of the split (`Ok(None)` when the
/// file doesn't exist; the whole file content when it doesn't start with a
/// `---` fence, matching [`read_frontmatter_doc`]'s own old-format fallback —
/// see [`super::read_doc_body`]'s doc comment).
///
/// For a hot-path batch caller that has *already* loaded this exact file's
/// `DocMetadata` successfully moments ago (t360.20.35: `trace.rs`'s
/// `resync_direct_edited_layer_docs`, whose `layer_docs` list is built from
/// `DocSet::load`'s own successfully-parsed `docs()` — a document whose
/// frontmatter fails to parse is routed to `DocSet::unreadable()` instead and
/// never appears there), re-running the full YAML `deserialize_frontmatter`
/// pass a second time per call just to throw the result away is pure waste:
/// profiling an M-scale `trace_suspect_clear` call (wiki/260-vmodel-m2-design.md
/// §6) found this was the single largest cost in that handler's hot path
/// (~1ms/doc x 33 layer docs ~= 33ms of a ~101ms call) — all spent
/// re-parsing YAML whose shape this caller already knows is valid. Callers
/// elsewhere that have *not* already validated the frontmatter (anything
/// that might read a corrupt or never-yet-read document) must keep using
/// [`super::read_doc_body`] — this function silently ignores a YAML parse
/// error in the frontmatter instead of surfacing it as `Err`, which is only
/// safe when the caller has that independent guarantee.
pub fn read_doc_body_only(path: &Path) -> Result<Option<String>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to read document: {}", path.display()))
        }
    };
    let (_fm_yaml, body) = split_frontmatter_and_body(&content);
    Ok(Some(body.to_string()))
}

/// Like [`read_frontmatter_doc`], but returns the raw frontmatter YAML text
/// **without** attempting to parse it (`Ok`, never `Err`, on a fenced-but-
/// unparseable file) — for repair tooling (`handoff_doc_repair_frontmatter`,
/// FR-804/E11) that needs the original, possibly-invalid text to attempt a
/// normalization pass (see [`repair_known_nonstandard_yaml`]) *before* ever
/// calling [`deserialize_frontmatter`] on it. Returns `Ok(None)` when the
/// file doesn't exist, or doesn't start with a `---` fence at all (nothing to
/// repair — same "no frontmatter" signal [`read_frontmatter_doc`] treats as a
/// migration case rather than a corrupt one).
pub fn read_raw_frontmatter_and_body(path: &Path) -> Result<Option<(String, String)>> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("Failed to read document: {}", path.display()))
        }
    };
    let (Some(fm_yaml), body) = split_frontmatter_and_body(&content) else {
        return Ok(None);
    };
    Ok(Some((fm_yaml.to_string(), body.to_string())))
}

/// Writes a single `_doc.<slug>.md` file: YAML frontmatter (fenced by
/// `---`) followed by `body` verbatim. `doc.sections` is never
/// serialized (see module docs) regardless of what it currently holds.
///
/// Returns the exact number of bytes written (M1 review N5 fix): callers
/// that cache a `content_hash` as "proven correct for this on-disk stamp"
/// (`storage::docs::write_doc_with_body`'s `TRUSTED_HASH_CACHE`) need this to
/// verify a *later* stat of the file actually describes the bytes this call
/// itself wrote, not a concurrent external writer's, before trusting it.
pub fn write_frontmatter_doc(path: &Path, doc: &DocMetadata, body: &str) -> Result<usize> {
    let fm_yaml = serialize_frontmatter(doc)?;
    let content = format!("---\n{fm_yaml}---\n{body}");
    crate::storage::atomic_write(path, content.as_bytes())
        .with_context(|| format!("Failed to write document: {}", path.display()))?;
    Ok(content.len())
}

/// One normalization [`repair_known_nonstandard_yaml`] applied — reported
/// back to `handoff_doc_repair_frontmatter`'s caller so a dry-run (or an
/// applied repair) can describe what would change/changed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FrontmatterFix {
    pub description: String,
}

/// Attempts to normalize `yaml` (the raw frontmatter text between the `---`
/// fences, not yet known to be valid YAML) into a form
/// [`deserialize_frontmatter`] can parse — recognizing only the specific
/// non-standard shapes wiki/260-vmodel-m2-design.md §4.12/E11 documents (the
/// real failure aelm's corpus exhibited, plus the other two named alongside
/// it). This is deliberately **not** a general YAML repair tool: an unknown
/// malformation returns `None` rather than guessing.
///
/// Returns `None` if none of the known shapes are present. Returns
/// `Some((repaired_text, fixes))` otherwise — `repaired_text` is *not*
/// guaranteed to parse even then (a document can combine a known shape with
/// an unrelated genuine error); the caller must still attempt
/// [`deserialize_frontmatter`] on the result and treat a further failure as
/// "could not repair", not silently give up before trying.
pub fn repair_known_nonstandard_yaml(yaml: &str) -> Option<(String, Vec<FrontmatterFix>)> {
    let mut fixes = Vec::new();
    let mut text = yaml.to_string();

    let detabbed = detab_yaml(&text);
    if detabbed != text {
        text = detabbed;
        fixes.push(FrontmatterFix {
            description: "converted tab indentation to spaces (YAML forbids tabs for \
                           indentation)"
                .to_string(),
        });
    }

    let (joined, joined_count) = join_key_then_flow_value_lines(&text);
    if joined_count > 0 {
        text = joined;
        fixes.push(FrontmatterFix {
            description: format!(
                "joined {joined_count} key(s) whose flow-style value (e.g. `[]`) was on its \
                 own line back onto the key's line (\"key:\\n[]\" -> \"key: []\")"
            ),
        });
    }

    if fixes.is_empty() {
        None
    } else {
        Some((text, fixes))
    }
}

/// Replaces every leading tab in each line's indentation with two spaces.
/// YAML forbids tabs for indentation entirely (a single leading tab anywhere
/// is a hard parse error, not just style) — this is the minimal fix that
/// preserves relative nesting for the common case of consistent tab-per-level
/// indentation, without attempting to infer the project's actual indent
/// width.
fn detab_yaml(text: &str) -> String {
    text.split('\n')
        .map(|line| {
            let indent_len = line.len() - line.trim_start_matches(['\t', ' ']).len();
            let (indent, rest) = line.split_at(indent_len);
            if indent.contains('\t') {
                format!("{}{rest}", indent.replace('\t', "  "))
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Finds `<key>:` lines with no value on their own line, immediately
/// followed by a line that is *only* a one-line flow collection (`[...]` or
/// `{...}`) at any indentation, and joins the two into a single standard
/// `<key>: [...]` mapping line — the exact non-standard shape aelm's corpus
/// exhibited (`scope_paths:` followed by a lone `[]` line). Returns the
/// rewritten text and how many joins were made (`0` when the shape wasn't
/// found, in which case the returned text equals the input).
///
/// Deliberately conservative: only triggers when the following line is
/// *entirely* a balanced flow collection (starts with `[`/`{`, ends with the
/// matching `]`/`}`, nothing else on the line) — a multi-line flow value, or
/// a line that merely starts with `[`, is left alone rather than guessed at.
fn join_key_then_flow_value_lines(text: &str) -> (String, usize) {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = Vec::with_capacity(lines.len());
    let mut count = 0usize;
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let trimmed_end = line.trim_end();
        let key_part = trimmed_end.strip_suffix(':').filter(|key| {
            let bare = key.trim_start();
            !bare.is_empty() && !bare.starts_with('-') && !bare.starts_with('#')
        });
        if let Some(key_part) = key_part {
            if let Some(next) = lines.get(i + 1) {
                let next_trimmed = next.trim();
                let is_flow_collection = (next_trimmed.starts_with('[')
                    && next_trimmed.ends_with(']'))
                    || (next_trimmed.starts_with('{') && next_trimmed.ends_with('}'));
                if is_flow_collection {
                    out.push(format!("{key_part}: {next_trimmed}"));
                    count += 1;
                    i += 2;
                    continue;
                }
            }
        }
        out.push(line.to_string());
        i += 1;
    }
    (out.join("\n"), count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::docs::model::{CodeRef, VerificationItem};

    fn sample_doc() -> DocMetadata {
        let mut doc = DocMetadata::new(
            "doc-20260718-120000-123456".to_string(),
            "my-slug".to_string(),
            "Document Title".to_string(),
            "spec".to_string(),
            "2026-07-18T12:00:00Z".to_string(),
        );
        doc.tags = vec!["auth".to_string(), "security".to_string()];
        doc.scope_paths = vec!["src/auth/".to_string()];
        doc.task_ids = vec!["t42".to_string(), "t43".to_string()];
        doc.parent_id = Some("doc-20260710-000000-000001".to_string());
        doc.related = vec![DocRelation {
            id: "doc-other".to_string(),
            rel: "supersedes".to_string(),
        }];
        doc.source.origin = "authored".to_string();
        doc.source.canonical_hash = Some("abc123".to_string());
        doc.content_hash = Some("def456".to_string());
        doc.updated_at = "2026-07-18T15:30:00Z".to_string();
        doc
    }

    #[test]
    fn serialize_frontmatter_roundtrips_core_fields() {
        let doc = sample_doc();
        let yaml = serialize_frontmatter(&doc).unwrap();
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();

        assert_eq!(back.id, doc.id);
        assert_eq!(back.slug, doc.slug);
        assert_eq!(back.title, doc.title);
        assert_eq!(back.doc_type, doc.doc_type);
        assert_eq!(back.tags, doc.tags);
        assert_eq!(back.scope_paths, doc.scope_paths);
        assert_eq!(back.task_ids, doc.task_ids);
        assert_eq!(back.parent_id, doc.parent_id);
        assert_eq!(back.related, doc.related);
        assert_eq!(back.source.origin, doc.source.origin);
        assert_eq!(back.source.canonical_hash, doc.source.canonical_hash);
        assert_eq!(back.content_hash, doc.content_hash);
        assert_eq!(back.created_at, doc.created_at);
        assert_eq!(back.updated_at, doc.updated_at);
    }

    /// wiki/220-vmodel-integration-design.md §2.4, M1 t360.6: `source.
    /// body_raw_hash` (the direct-edit-detection FNV-1a of the raw body
    /// bytes) round-trips through frontmatter like `canonical_hash`, and is
    /// absent from the serialized YAML when unset (NFR-004, no spurious
    /// diff on documents that predate this field).
    #[test]
    fn source_body_raw_hash_round_trips_and_is_absent_when_unset() {
        let mut doc = sample_doc();
        doc.source.body_raw_hash = Some("a1b2c3d4e5f6a7b8".to_string());
        let yaml = serialize_frontmatter(&doc).unwrap();
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(
            back.source.body_raw_hash.as_deref(),
            Some("a1b2c3d4e5f6a7b8")
        );

        let unset_yaml = serialize_frontmatter(&sample_doc()).unwrap();
        assert!(
            !unset_yaml.contains("body_raw_hash"),
            "unset body_raw_hash must not appear in serialized frontmatter: {unset_yaml}"
        );
    }

    /// Backward compat: a document written before `body_raw_hash` existed
    /// has no such key in its `source:` block and must still deserialize.
    #[test]
    fn deserializes_source_without_body_raw_hash() {
        let doc = sample_doc();
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(!yaml.contains("body_raw_hash"));
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert!(back.source.body_raw_hash.is_none());
    }

    /// t370.15 (PR-4, wiki/240-performance-design.md §6): `source.
    /// content_hash_scheme` round-trips like `body_raw_hash`, and is absent
    /// from the serialized YAML when unset — so a pre-t370.15 document (no
    /// such key at all) deserializes to `None` (the "legacy scheme" signal
    /// `handle_doc_reassemble`'s drift check relies on), not a spurious diff
    /// on re-save.
    #[test]
    fn source_content_hash_scheme_round_trips_and_is_absent_when_unset() {
        let mut doc = sample_doc();
        doc.source.content_hash_scheme =
            Some(crate::storage::docs::model::CONTENT_HASH_SCHEME_SECTION_COMPOSED);
        let yaml = serialize_frontmatter(&doc).unwrap();
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(
            back.source.content_hash_scheme,
            Some(crate::storage::docs::model::CONTENT_HASH_SCHEME_SECTION_COMPOSED)
        );

        let unset_yaml = serialize_frontmatter(&sample_doc()).unwrap();
        assert!(
            !unset_yaml.contains("content_hash_scheme"),
            "unset content_hash_scheme must not appear in serialized frontmatter: {unset_yaml}"
        );
    }

    /// Backward compat: a document written before `content_hash_scheme`
    /// existed has no such key in its `source:` block and must still
    /// deserialize, with the field defaulting to `None` (treated as "legacy
    /// scheme").
    #[test]
    fn deserializes_source_without_content_hash_scheme() {
        let doc = sample_doc();
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(!yaml.contains("content_hash_scheme"));
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert!(back.source.content_hash_scheme.is_none());
    }

    /// M1 t360.4 (wiki/220-vmodel-integration-design.md §2.1): `doc_save`'s
    /// `layer` argument is the only AI-facing way to set `DocMetadata.layer`;
    /// this test only exercises the frontmatter round-trip that argument
    /// ultimately persists through.
    #[test]
    fn layer_roundtrips_through_yaml_frontmatter_when_set() {
        let mut doc = sample_doc();
        doc.layer = Some("basic_spec".to_string());
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            yaml.contains("layer: basic_spec"),
            "layer must appear in frontmatter when set: {yaml}"
        );
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(back.layer, doc.layer);
    }

    /// NFR-001/002/004 (wiki/220 §5, §2.1 "未設定の文書は従来どおり"): a
    /// document that never had its layer set must not gain a `layer:` key —
    /// otherwise every pre-M1 document would show a spurious frontmatter
    /// diff the first time handoff-mcp re-saves it.
    #[test]
    fn layer_is_absent_from_frontmatter_when_unset() {
        let doc = sample_doc();
        assert_eq!(doc.layer, None, "sample_doc must start with no layer set");
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            !yaml.lines().any(|l| l.starts_with("layer:")),
            "layer key must be absent from frontmatter when unset: {yaml}"
        );
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(back.layer, None);
    }

    #[test]
    fn serialize_frontmatter_never_includes_sections_version_or_slug() {
        let mut doc = sample_doc();
        doc.sections = vec![super::super::model::SectionIndex {
            seq: 0,
            heading: String::new(),
            level: 0,
            byte_offset: 0,
            byte_length: 10,
            content_hash: Some("h".to_string()),
        }];
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            !yaml.contains("sections"),
            "sections[] must never be written to frontmatter: {yaml}"
        );
        assert!(
            !yaml.contains("version:"),
            "schema version must not be in frontmatter: {yaml}"
        );
        assert!(
            !yaml.lines().any(|l| l.starts_with("slug:")),
            "slug must not be in frontmatter (derived from filename): {yaml}"
        );
    }

    #[test]
    fn verification_round_trips_through_yaml_frontmatter() {
        let mut doc = sample_doc();
        doc.verification = Some(Verification {
            status: "in_progress".to_string(),
            created_at: "2026-07-18T12:00:00Z".to_string(),
            updated_at: "2026-07-18T12:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section Title".to_string(),
                status: "verified".to_string(),
                impl_refs: vec![CodeRef {
                    path: "src/foo.rs".to_string(),
                    lines: Some("10-50".to_string()),
                    label: None,
                }],
                test_refs: vec![CodeRef {
                    path: "tests/foo.rs".to_string(),
                    lines: None,
                    label: None,
                }],
                reviewer: None,
                verified_at: Some("2026-07-18T12:00:00Z".to_string()),
                notes: String::new(),
                content_hash_at_verify: Some("abc123".to_string()),
                category: "section".to_string(),
                sub_items: Vec::new(),
                label: None,
            }],
        });

        let yaml = serialize_frontmatter(&doc).unwrap();
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        let v = back.verification.expect("verification must round-trip");
        assert_eq!(v.status, "in_progress");
        assert_eq!(v.items.len(), 1);
        assert_eq!(v.items[0].fragment_seq, Some(1));
        assert_eq!(v.items[0].impl_refs[0].path, "src/foo.rs");
        assert_eq!(v.items[0].impl_refs[0].lines.as_deref(), Some("10-50"));
        assert_eq!(v.items[0].test_refs[0].path, "tests/foo.rs");
        assert_eq!(v.items[0].content_hash_at_verify.as_deref(), Some("abc123"));
    }

    #[test]
    fn sub_item_stable_id_round_trips_through_yaml() {
        use crate::storage::docs::model::SubItem;

        let mut doc = sample_doc();
        doc.verification = Some(Verification {
            status: "pending".to_string(),
            created_at: "2026-09-21T00:00:00Z".to_string(),
            updated_at: "2026-09-21T00:00:00Z".to_string(),
            items: vec![VerificationItem {
                fragment_seq: Some(1),
                heading: "Section".to_string(),
                status: "pending".to_string(),
                impl_refs: Vec::new(),
                test_refs: Vec::new(),
                reviewer: None,
                verified_at: None,
                notes: String::new(),
                content_hash_at_verify: None,
                category: "section".to_string(),
                sub_items: vec![SubItem {
                    index: 0,
                    description: "Test requirement".to_string(),
                    stable_id: Some("C01-1.1".to_string()),
                    dev_stage: Some("in_progress".to_string()),
                    priority: Some("P0".to_string()),
                    ..Default::default()
                }],
                label: None,
            }],
        });

        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            yaml.contains("stable_id"),
            "stable_id must appear in YAML output: {yaml}"
        );
        assert!(
            yaml.contains("C01-1.1"),
            "stable_id value must appear in YAML output: {yaml}"
        );
        assert!(
            yaml.contains("dev_stage"),
            "dev_stage must appear in YAML output: {yaml}"
        );
        assert!(
            yaml.contains("priority"),
            "priority must appear in YAML output: {yaml}"
        );

        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        let v = back.verification.expect("verification must round-trip");
        assert_eq!(v.items[0].sub_items.len(), 1);
        let si = &v.items[0].sub_items[0];
        assert_eq!(si.stable_id.as_deref(), Some("C01-1.1"));
        assert_eq!(si.dev_stage.as_deref(), Some("in_progress"));
        assert_eq!(si.priority.as_deref(), Some("P0"));
    }

    #[test]
    fn deserialize_frontmatter_accepts_documented_aliases() {
        let yaml = "id: doc-1\n\
                     title: T\n\
                     doc_type: note\n\
                     date: 2026-01-01T00:00:00Z\n\
                     lastmod: 2026-01-02T00:00:00Z\n\
                     excerpt: A short summary\n";
        let doc = deserialize_frontmatter(yaml, "slug-1").unwrap();
        assert_eq!(doc.created_at, "2026-01-01T00:00:00Z");
        assert_eq!(doc.updated_at, "2026-01-02T00:00:00Z");
        assert_eq!(
            doc.extra.get("description").and_then(|v| v.as_str()),
            Some("A short summary")
        );
    }

    #[test]
    fn deserialize_frontmatter_preserves_unknown_keys_in_extra() {
        let yaml = "id: doc-1\n\
                     title: T\n\
                     doc_type: note\n\
                     created_at: 2026-01-01T00:00:00Z\n\
                     updated_at: 2026-01-01T00:00:00Z\n\
                     custom_field: hello\n\
                     another: 42\n";
        let doc = deserialize_frontmatter(yaml, "slug-1").unwrap();
        assert_eq!(
            doc.extra.get("custom_field").and_then(|v| v.as_str()),
            Some("hello")
        );
        assert_eq!(doc.extra.get("another").and_then(|v| v.as_i64()), Some(42));
    }

    #[test]
    fn extra_fields_round_trip_through_write_then_read() {
        let mut doc = sample_doc();
        doc.extra.insert(
            "custom_field".to_string(),
            Value::String("hello".to_string()),
        );
        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(yaml.contains("custom_field"));
        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(
            back.extra.get("custom_field").and_then(|v| v.as_str()),
            Some("hello")
        );
    }

    #[test]
    fn write_then_read_frontmatter_doc_roundtrip() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.my-slug.md");
        let doc = sample_doc();
        let body = "# Document Title\n\n## Section 1\n\nBody text.\n";

        write_frontmatter_doc(&path, &doc, body).unwrap();
        let (back_doc, back_body) = read_frontmatter_doc(&path, &doc.slug)
            .unwrap()
            .expect("file must exist");

        assert_eq!(back_doc.id, doc.id);
        assert_eq!(back_doc.title, doc.title);
        assert_eq!(back_body, body);
        assert!(
            back_doc.sections.is_empty(),
            "sections must not be persisted/parsed from frontmatter"
        );
    }

    /// M1 review N5 fix: `write_frontmatter_doc` returns the exact byte
    /// length it wrote, so `storage::docs::write_doc_with_body` can verify a
    /// post-write stat actually describes *its own* write (matching byte
    /// length) before trusting it to cache a `content_hash` — guarding
    /// against a concurrent external writer landing between this function's
    /// `atomic_write` and the caller's later `stat`.
    #[test]
    fn write_frontmatter_doc_returns_exact_bytes_written() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.len-check.md");
        let doc = sample_doc();
        let body = "# Title\n\nBody.\n";

        let written_len = write_frontmatter_doc(&path, &doc, body).unwrap();
        let on_disk_len = std::fs::metadata(&path).unwrap().len() as usize;
        assert_eq!(
            written_len, on_disk_len,
            "returned length must equal the actual on-disk file size"
        );
    }

    #[test]
    fn read_frontmatter_doc_missing_file_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.nope.md");
        assert!(read_frontmatter_doc(&path, "nope").unwrap().is_none());
    }

    #[test]
    fn read_frontmatter_doc_without_frontmatter_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.old-format.md");
        std::fs::write(&path, "# Just a plain body\n\nNo frontmatter here.\n").unwrap();
        assert!(
            read_frontmatter_doc(&path, "old-format").unwrap().is_none(),
            "a body-only file (no leading '---' fence) must be treated as \
             'no frontmatter', not an error — the caller decides whether \
             that's an old-format migration or a genuine error case"
        );
    }

    #[test]
    fn read_frontmatter_doc_corrupt_yaml_is_error() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.corrupt.md");
        std::fs::write(&path, "---\nid: [unterminated\n---\nbody\n").unwrap();
        assert!(read_frontmatter_doc(&path, "corrupt").is_err());
    }

    #[test]
    fn read_doc_body_only_missing_file_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.nope.md");
        assert!(read_doc_body_only(&path).unwrap().is_none());
    }

    #[test]
    fn read_doc_body_only_matches_read_frontmatter_doc_body() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.match.md");
        let doc = sample_doc();
        let body = "# Title\n\nSome body text.\n";
        write_frontmatter_doc(&path, &doc, body).unwrap();

        let (_, from_full_read) = read_frontmatter_doc(&path, &doc.slug).unwrap().unwrap();
        let from_body_only = read_doc_body_only(&path).unwrap().unwrap();
        assert_eq!(from_body_only, from_full_read);
        assert_eq!(from_body_only, body);
    }

    #[test]
    fn read_doc_body_only_without_frontmatter_returns_whole_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.old-format.md");
        std::fs::write(&path, "# Just a plain body\n\nNo frontmatter here.\n").unwrap();
        assert_eq!(
            read_doc_body_only(&path).unwrap().unwrap(),
            "# Just a plain body\n\nNo frontmatter here.\n"
        );
    }

    /// Unlike [`read_frontmatter_doc`], corrupt YAML in the frontmatter must
    /// not become an `Err` here — this function never parses the frontmatter
    /// at all, by design (see its own doc comment on why that's only safe
    /// for a caller with an independent "this file already parsed cleanly"
    /// guarantee).
    #[test]
    fn read_doc_body_only_ignores_corrupt_yaml_frontmatter() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.corrupt.md");
        std::fs::write(&path, "---\nid: [unterminated\n---\nbody text\n").unwrap();
        assert_eq!(read_doc_body_only(&path).unwrap().unwrap(), "body text\n");
    }

    #[test]
    fn write_frontmatter_doc_body_survives_headings_that_look_like_yaml() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.tricky.md");
        let doc = sample_doc();
        // Body containing a line that looks like a closing fence prefix
        // ("---") must not confuse the frontmatter/body split.
        let body = "# Title\n\n---\n\nA horizontal rule inside the body.\n";

        write_frontmatter_doc(&path, &doc, body).unwrap();
        let (_, back_body) = read_frontmatter_doc(&path, &doc.slug).unwrap().unwrap();
        assert_eq!(back_body, body);
    }

    /// FR-804/E11 (wiki/260-vmodel-m2-design.md §4.12): the real aelm corpus
    /// shape (`scope_paths:` followed by a lone `[]` line at the same
    /// indentation — 9 of 209 documents) must fail as a *structured*, line-
    /// numbered error, not a generic message — `read_all_docs_with_unreadable`
    /// and `handoff_doc_repair_frontmatter` both need `FrontmatterParseError`
    /// to be downcast-able out of the returned `anyhow::Error`.
    #[test]
    fn deserialize_frontmatter_corrupt_yaml_reports_structured_line_number() {
        let yaml = "id: doc-1\n\
                     title: T\n\
                     doc_type: spec\n\
                     scope_paths:\n\
                     []\n\
                     parent_id: null\n";
        let err = deserialize_frontmatter(yaml, "slug-1").unwrap_err();
        let parse_err = err
            .downcast_ref::<FrontmatterParseError>()
            .expect("must downcast to FrontmatterParseError, not a generic anyhow::Error");
        assert!(
            parse_err.line.is_some(),
            "serde_yaml reported a location for this error; it must not be dropped"
        );
    }

    /// §4.12: a leading BOM before the opening `---` fence must not make an
    /// otherwise-standard document silently look like "no frontmatter"
    /// (previously: `split_frontmatter_and_body` returned `None`, so
    /// `read_doc_impl` fell into the legacy-migration branch and — with no
    /// `.json` sidecar to migrate from — the document was skipped outright).
    #[test]
    fn read_frontmatter_doc_tolerates_leading_bom_before_fence() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.bom.md");
        let doc = sample_doc();
        let fm_yaml = serialize_frontmatter(&doc).unwrap();
        let content = format!("\u{feff}---\n{fm_yaml}---\n# Body\n");
        std::fs::write(&path, content).unwrap();

        let (back_doc, back_body) = read_frontmatter_doc(&path, &doc.slug)
            .unwrap()
            .expect("BOM-prefixed but otherwise standard frontmatter must still be read");
        assert_eq!(back_doc.id, doc.id);
        assert_eq!(back_body, "# Body\n");
    }

    /// §4.12: PyYAML (YAML 1.1) reads a bare `no`/`yes`/`on`/`off`/`true`/
    /// `false` value as a boolean, not a string — a tag literally named `no`
    /// would silently change type for any reader that isn't `serde_yaml`
    /// (YAML 1.2). The writer must single-quote these so every reader agrees
    /// on the type.
    #[test]
    fn serialize_frontmatter_quotes_yaml11_ambiguous_tag_values() {
        let mut doc = sample_doc();
        doc.tags = vec![
            "yes".to_string(),
            "no".to_string(),
            "normal-tag".to_string(),
        ];

        let yaml = serialize_frontmatter(&doc).unwrap();
        assert!(
            yaml.contains("- 'yes'"),
            "bare 'yes' tag must be quoted: {yaml}"
        );
        assert!(
            yaml.contains("- 'no'"),
            "bare 'no' tag must be quoted: {yaml}"
        );
        assert!(
            yaml.contains("- normal-tag"),
            "an unambiguous tag must stay unquoted: {yaml}"
        );

        let back = deserialize_frontmatter(&yaml, &doc.slug).unwrap();
        assert_eq!(back.tags, doc.tags);
    }

    #[test]
    fn repair_known_nonstandard_yaml_joins_key_with_flow_value_on_next_line() {
        // The exact shape found in 9/209 aelm documents.
        let yaml = "id: doc-1\n\
                     title: T\n\
                     doc_type: spec\n\
                     tags:\n\
                     - specification\n\
                     scope_paths:\n\
                     []\n\
                     parent_id: null\n\
                     created_at: 2026-01-01T00:00:00Z\n\
                     updated_at: 2026-01-01T00:00:00Z\n";
        assert!(
            deserialize_frontmatter(yaml, "s").is_err(),
            "fixture must reproduce the real parse failure before repair"
        );

        let (repaired, fixes) =
            repair_known_nonstandard_yaml(yaml).expect("known shape must be recognized");
        assert!(!fixes.is_empty());
        let doc = deserialize_frontmatter(&repaired, "s").unwrap_or_else(|e| {
            panic!("repaired text must parse: {e}\n---\n{repaired}");
        });
        assert!(doc.scope_paths.is_empty());
        assert_eq!(doc.tags, vec!["specification".to_string()]);
    }

    #[test]
    fn repair_known_nonstandard_yaml_converts_tab_indentation() {
        let yaml = "id: doc-1\ntitle: T\ndoc_type: spec\nsource:\n\torigin: authored\n\
                     created_at: 2026-01-01T00:00:00Z\nupdated_at: 2026-01-01T00:00:00Z\n";
        assert!(deserialize_frontmatter(yaml, "s").is_err());

        let (repaired, fixes) =
            repair_known_nonstandard_yaml(yaml).expect("tab indentation must be recognized");
        assert!(!fixes.is_empty());
        deserialize_frontmatter(&repaired, "s").expect("repaired text must parse");
    }

    #[test]
    fn repair_known_nonstandard_yaml_returns_none_for_unrecognized_shapes() {
        // A YAML error with none of the known non-standard shapes present.
        let yaml = "id: [unterminated\n";
        assert!(deserialize_frontmatter(yaml, "s").is_err());
        assert!(repair_known_nonstandard_yaml(yaml).is_none());
    }

    #[test]
    fn read_raw_frontmatter_and_body_returns_text_even_when_unparseable() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.corrupt.md");
        std::fs::write(
            &path,
            "---\nid: doc-1\ntitle: T\ndoc_type: spec\nscope_paths:\n[]\nparent_id: null\n---\nbody\n",
        )
        .unwrap();

        let (raw_yaml, body) = read_raw_frontmatter_and_body(&path).unwrap().unwrap();
        assert!(raw_yaml.contains("scope_paths:"));
        assert_eq!(body, "body\n");
    }

    #[test]
    fn read_raw_frontmatter_and_body_missing_file_is_none() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join("_doc.nope.md");
        assert!(read_raw_frontmatter_and_body(&path).unwrap().is_none());
    }
}

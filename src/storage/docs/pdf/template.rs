//! Typst template management for PDF export (REQ-PDF-002 / 004 / 012).
//!
//! # Where templates live
//!
//! `.handoff/templates/` holds the project's templates, one `.typ` file each:
//!
//! | File         | Purpose                                              |
//! |--------------|------------------------------------------------------|
//! | `default.typ`| General purpose: cover, table of contents, running header, page numbers |
//! | `report.typ` | Reports: serif body, organization on the cover, a new page per chapter |
//! | `spec.typ`   | Specifications: document-info table on the cover, deeper numbering, shaded table headers |
//!
//! The first export creates whichever of these is missing
//! ([`ensure_templates`]). A file that already exists is **never**
//! overwritten, so edits made by a user or an AI survive every later export.
//! Deleting a file restores the built-in version on the next export.
//!
//! # How a template is applied
//!
//! The intermediate `.typ` file starts with the preamble produced by
//! [`render_preamble`]:
//!
//! ```typst
//! #import "/.handoff/templates/default.typ": conf
//! #show: conf.with(title: "...", author: "...", font-sans: ("Noto Sans CJK JP",))
//! ```
//!
//! so page layout is owned by the template (`conf` plus the cover / toc /
//! header / footer functions it calls) and the converted Markdown is only the
//! body. The path is root-absolute, so `typst compile` must be run with
//! `--root` set to a directory that contains the template
//! ([`typst_import_path`]).
//!
//! # Custom templates
//!
//! A custom template is any `.typ` file that exports a `conf` function with
//! the same signature as `default.typ`. Name one that lives in
//! `.handoff/templates/` by its stem (`report`), or point at any other file by
//! path ([`prepare_template`]).
//!
//! # Fonts
//!
//! [`detect_fonts`] asks `typst fonts` which families Typst can actually see
//! and [`select_fonts`] picks the Japanese-capable ones in preference order
//! (Noto CJK JP, IPA, then macOS / Windows system fonts). They are passed to
//! the template as `font-sans` / `font-serif`.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::storage::atomic_write;

/// Directory name, inside `.handoff/`, that holds the project's templates.
pub const TEMPLATES_DIR_NAME: &str = "templates";

/// File extension of Typst sources.
const TYPST_EXT: &str = "typ";

/// A template shipped inside the binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinTemplate {
    Default,
    Report,
    Spec,
}

impl BuiltinTemplate {
    pub const ALL: [BuiltinTemplate; 3] = [
        BuiltinTemplate::Default,
        BuiltinTemplate::Report,
        BuiltinTemplate::Spec,
    ];

    /// File stem, which is also the name callers pass to select it.
    pub fn name(self) -> &'static str {
        match self {
            BuiltinTemplate::Default => "default",
            BuiltinTemplate::Report => "report",
            BuiltinTemplate::Spec => "spec",
        }
    }

    pub fn file_name(self) -> &'static str {
        match self {
            BuiltinTemplate::Default => "default.typ",
            BuiltinTemplate::Report => "report.typ",
            BuiltinTemplate::Spec => "spec.typ",
        }
    }

    /// The embedded source text.
    pub fn source(self) -> &'static str {
        match self {
            BuiltinTemplate::Default => include_str!("templates/default.typ"),
            BuiltinTemplate::Report => include_str!("templates/report.typ"),
            BuiltinTemplate::Spec => include_str!("templates/spec.typ"),
        }
    }
}

/// `.handoff/templates/`.
pub fn templates_dir(handoff_dir: &Path) -> PathBuf {
    handoff_dir.join(TEMPLATES_DIR_NAME)
}

/// Create `.handoff/templates/` and any built-in template missing from it
/// (REQ-PDF-012 AC1/AC2). Existing files are left byte-for-byte untouched
/// (AC3). Returns the files that were created, in [`BuiltinTemplate::ALL`]
/// order; an empty list means nothing needed creating.
pub fn ensure_templates(handoff_dir: &Path) -> Result<Vec<PathBuf>> {
    let dir = templates_dir(handoff_dir);
    std::fs::create_dir_all(&dir)
        .with_context(|| format!("Failed to create templates dir: {}", dir.display()))?;
    let mut created = Vec::new();
    for template in BuiltinTemplate::ALL {
        let path = dir.join(template.file_name());
        if path.exists() {
            continue;
        }
        atomic_write(&path, template.source().as_bytes())
            .with_context(|| format!("Failed to write template: {}", path.display()))?;
        created.push(path);
    }
    Ok(created)
}

/// A template chosen for an export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedTemplate {
    /// Absolute-or-as-given path of the `.typ` file to import.
    pub path: PathBuf,
    /// `true` when the file lives in `.handoff/templates/` (selected by name
    /// or defaulted); `false` for a file selected by path.
    pub in_templates_dir: bool,
}

/// Make sure the built-in templates exist, then resolve `spec` to a file
/// (REQ-PDF-004 AC2).
///
/// `spec` is one of:
/// - `None` - `default.typ`;
/// - a bare name, with or without `.typ` (`report`, `spec.typ`) - a file in
///   `.handoff/templates/`;
/// - anything else - a path, absolute or relative to `project_dir`.
pub fn prepare_template(
    handoff_dir: &Path,
    project_dir: &Path,
    spec: Option<&str>,
) -> Result<ResolvedTemplate> {
    ensure_templates(handoff_dir)?;
    resolve_template(handoff_dir, project_dir, spec)
}

fn resolve_template(
    handoff_dir: &Path,
    project_dir: &Path,
    spec: Option<&str>,
) -> Result<ResolvedTemplate> {
    let spec = spec.map(str::trim).filter(|s| !s.is_empty());
    let dir = templates_dir(handoff_dir);
    let Some(spec) = spec else {
        return Ok(ResolvedTemplate {
            path: dir.join(BuiltinTemplate::Default.file_name()),
            in_templates_dir: true,
        });
    };

    if is_bare_template_name(spec) {
        let file_name = if spec.ends_with(".typ") {
            spec.to_string()
        } else {
            format!("{spec}.{TYPST_EXT}")
        };
        let path = dir.join(&file_name);
        if !path.is_file() {
            bail!(
                "Template '{spec}' not found in {}. Available templates: {}. \
                 Pass a path to use a template stored elsewhere.",
                dir.display(),
                list_templates(&dir).join(", ")
            );
        }
        return Ok(ResolvedTemplate {
            path,
            in_templates_dir: true,
        });
    }

    let expanded = crate::storage::expand_tilde(spec);
    let candidate = Path::new(&expanded);
    let path = if candidate.is_absolute() {
        candidate.to_path_buf()
    } else {
        project_dir.join(candidate)
    };
    if path.extension().and_then(|e| e.to_str()) != Some(TYPST_EXT) {
        bail!(
            "Template path '{}' must be a .{TYPST_EXT} file",
            path.display()
        );
    }
    if !path.is_file() {
        bail!("Template file not found: {}", path.display());
    }
    Ok(ResolvedTemplate {
        path,
        in_templates_dir: false,
    })
}

/// `report` and `spec.typ` are names; `docs/x.typ`, `/abs/x.typ` and `..\x.typ`
/// are paths.
fn is_bare_template_name(spec: &str) -> bool {
    !spec.is_empty()
        && !spec.contains(['/', '\\', '~'])
        && spec != "."
        && spec != ".."
        && !spec.starts_with('.')
}

fn list_templates(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter_map(|e| {
                    let p = e.path();
                    (p.extension().and_then(|x| x.to_str()) == Some(TYPST_EXT))
                        .then(|| p.file_stem().map(|s| s.to_string_lossy().into_owned()))
                        .flatten()
                })
                .collect()
        })
        // An unreadable directory only costs the "available templates" hint
        // in an error message that is already being produced.
        .unwrap_or_default();
    names.sort();
    names
}

/// Root-absolute Typst import path for `template` (`/.handoff/templates/x.typ`),
/// valid when `typst compile --root <root>` is used. Fails when the template is
/// outside `root`, because Typst refuses to read files outside its root.
pub fn typst_import_path(root: &Path, template: &Path) -> Result<String> {
    let rel = template.strip_prefix(root).with_context(|| {
        format!(
            "Template {} is outside the Typst root {}; move it inside the project or choose another template",
            template.display(),
            root.display()
        )
    })?;
    let mut out = String::new();
    for component in rel.components() {
        match component {
            std::path::Component::Normal(part) => {
                out.push('/');
                out.push_str(&part.to_string_lossy());
            }
            other => bail!(
                "Template path {} contains an unsupported component {other:?}",
                template.display()
            ),
        }
    }
    Ok(out)
}

// --- Fonts (REQ-PDF-002) ------------------------------------------------------

/// Japanese-capable sans families, most preferred first.
const SANS_CANDIDATES: &[&str] = &[
    "Noto Sans CJK JP",
    "Noto Sans JP",
    "IPAexGothic",
    "IPAGothic",
    "IPAPGothic",
    "Hiragino Sans",
    "Hiragino Kaku Gothic ProN",
    "Yu Gothic",
    "Meiryo",
    "BIZ UDPGothic",
];

/// Japanese-capable serif families, most preferred first.
const SERIF_CANDIDATES: &[&str] = &[
    "Noto Serif CJK JP",
    "Noto Serif JP",
    "IPAexMincho",
    "IPAMincho",
    "IPAPMincho",
    "Hiragino Mincho ProN",
    "Yu Mincho",
    "BIZ UDPMincho",
];

/// Japanese font families found on the system, in preference order. Either
/// list can be empty.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontSelection {
    pub sans: Vec<String>,
    pub serif: Vec<String>,
}

impl FontSelection {
    pub fn is_empty(&self) -> bool {
        self.sans.is_empty() && self.serif.is_empty()
    }

    /// Message for the export response when no Japanese font was found:
    /// Japanese text would render as missing-glyph boxes.
    pub fn missing_warning(&self) -> Option<String> {
        self.is_empty().then(|| {
            "No Japanese font was found by Typst; Japanese text will render as blank boxes. \
             Install Noto Sans CJK JP (e.g. `apt install fonts-noto-cjk`, `brew install --cask font-noto-sans-cjk-jp`) \
             or IPA fonts (`apt install fonts-ipafont`), or set TYPST_FONT_PATHS to a directory containing one."
                .to_string()
        })
    }
}

/// Pick the Japanese-capable families out of `available` (family names as
/// printed by `typst fonts`). Matching is case-insensitive; the result uses
/// the candidate's canonical spelling and preference order.
pub fn select_fonts<S: AsRef<str>>(available: &[S]) -> FontSelection {
    let pick = |candidates: &[&str]| -> Vec<String> {
        candidates
            .iter()
            .filter(|c| available.iter().any(|a| a.as_ref().eq_ignore_ascii_case(c)))
            .map(|c| (*c).to_string())
            .collect()
    };
    FontSelection {
        sans: pick(SANS_CANDIDATES),
        serif: pick(SERIF_CANDIDATES),
    }
}

/// Parse `typst fonts` output: one family name per line.
pub fn parse_typst_fonts(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// Ask the Typst binary which font families it can see and select the
/// Japanese ones. Honors `TYPST_FONT_PATHS` because Typst itself does.
pub fn detect_fonts(typst: &Path) -> Result<FontSelection> {
    let output = Command::new(typst)
        .arg("fonts")
        .output()
        .with_context(|| format!("Failed to run `{} fonts`", typst.display()))?;
    if !output.status.success() {
        bail!(
            "`{} fonts` failed ({}): {}",
            typst.display(),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let families = parse_typst_fonts(&String::from_utf8_lossy(&output.stdout));
    Ok(select_fonts(&families))
}

// --- Preamble (REQ-PDF-004 AC1/AC2) ----------------------------------------------

/// Cover-page metadata handed to the template's `conf` function.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoverMeta {
    pub title: String,
    pub subtitle: Option<String>,
    pub author: Option<String>,
    /// Display date; the template falls back to today when `None`.
    pub date: Option<String>,
    /// `Some(b)` forces the template's `show-toc` switch; `None` leaves the
    /// template's own default (REQ-PDF-006 `include_toc`).
    pub show_toc: Option<bool>,
}

/// Quote `s` as a Typst string literal.
pub fn typst_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                let _ = write!(out, "\\u{{{:x}}}", c as u32);
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn typst_array(items: &[String]) -> String {
    let mut out = String::from("(");
    for item in items {
        out.push_str(&typst_string(item));
        out.push_str(", ");
    }
    out.push(')');
    out
}

/// The `#import` + `#show: conf.with(...)` header that applies `import_path`
/// (from [`typst_import_path`]) to the body that follows. Fonts the system
/// lacks are omitted so the template's own defaults stay in force.
pub fn render_preamble(import_path: &str, meta: &CoverMeta, fonts: &FontSelection) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "#import {}: conf", typst_string(import_path));
    out.push_str("#show: conf.with(\n");
    let _ = writeln!(out, "  title: {},", typst_string(&meta.title));
    for (key, value) in [
        ("subtitle", &meta.subtitle),
        ("author", &meta.author),
        ("date", &meta.date),
    ] {
        if let Some(v) = value {
            let _ = writeln!(out, "  {key}: {},", typst_string(v));
        }
    }
    if let Some(show_toc) = meta.show_toc {
        let _ = writeln!(out, "  show-toc: {show_toc},");
    }
    if !fonts.sans.is_empty() {
        let _ = writeln!(out, "  font-sans: {},", typst_array(&fonts.sans));
    }
    if !fonts.serif.is_empty() {
        let _ = writeln!(out, "  font-serif: {},", typst_array(&fonts.serif));
    }
    out.push_str(")\n\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn handoff() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    // --- ensure_templates ---

    #[test]
    fn ensure_creates_all_three_templates_when_dir_is_absent() {
        let h = handoff();
        let created = ensure_templates(h.path()).unwrap();
        assert_eq!(created.len(), 3);
        for t in BuiltinTemplate::ALL {
            let p = h.path().join("templates").join(t.file_name());
            assert_eq!(fs::read_to_string(&p).unwrap(), t.source(), "{p:?}");
        }
    }

    #[test]
    fn ensure_never_overwrites_a_user_edited_template() {
        let h = handoff();
        ensure_templates(h.path()).unwrap();
        let edited = h.path().join("templates/default.typ");
        fs::write(&edited, "// my custom layout\n").unwrap();

        let created = ensure_templates(h.path()).unwrap();

        assert!(created.is_empty(), "nothing is missing: {created:?}");
        assert_eq!(
            fs::read_to_string(&edited).unwrap(),
            "// my custom layout\n"
        );
    }

    #[test]
    fn ensure_recreates_only_the_deleted_template() {
        let h = handoff();
        ensure_templates(h.path()).unwrap();
        fs::write(h.path().join("templates/report.typ"), "// edited\n").unwrap();
        fs::remove_file(h.path().join("templates/spec.typ")).unwrap();

        let created = ensure_templates(h.path()).unwrap();

        assert_eq!(created, vec![h.path().join("templates/spec.typ")]);
        assert_eq!(
            fs::read_to_string(h.path().join("templates/report.typ")).unwrap(),
            "// edited\n"
        );
    }

    #[test]
    fn ensure_keeps_unrelated_files_in_templates_dir() {
        let h = handoff();
        fs::create_dir_all(h.path().join("templates")).unwrap();
        fs::write(h.path().join("templates/company.typ"), "// mine").unwrap();
        ensure_templates(h.path()).unwrap();
        assert_eq!(
            fs::read_to_string(h.path().join("templates/company.typ")).unwrap(),
            "// mine"
        );
    }

    // --- embedded template contract ---

    #[test]
    fn every_builtin_template_exports_conf_with_the_shared_signature() {
        for t in BuiltinTemplate::ALL {
            let src = t.source();
            assert!(src.contains("#let conf("), "{}: no conf()", t.file_name());
            for param in [
                "title:",
                "subtitle:",
                "author:",
                "date:",
                "font-sans:",
                "font-serif:",
                "font-mono:",
                "show-cover:",
                "show-toc:",
            ] {
                assert!(src.contains(param), "{} lacks {param}", t.file_name());
            }
        }
    }

    #[test]
    fn default_template_has_cover_toc_header_footer_and_page_numbers() {
        let src = BuiltinTemplate::Default.source();
        for needle in [
            "#let cover-page(",
            "outline(",
            "#let page-header(",
            "#let page-footer(",
            "counter(page)",
            "set heading(numbering",
            "datetime.today()",
        ] {
            assert!(src.contains(needle), "default.typ lacks {needle}");
        }
    }

    #[test]
    fn report_and_spec_import_shared_parts_from_default() {
        for t in [BuiltinTemplate::Report, BuiltinTemplate::Spec] {
            assert!(
                t.source().contains("#import \"default.typ\":"),
                "{}",
                t.file_name()
            );
        }
    }

    #[test]
    fn builtin_names_are_stems_of_their_file_names() {
        for t in BuiltinTemplate::ALL {
            assert_eq!(format!("{}.typ", t.name()), t.file_name());
        }
    }

    // --- prepare_template / resolve ---

    #[test]
    fn prepare_without_spec_generates_and_returns_default() {
        let h = handoff();
        let project = handoff();
        let r = prepare_template(h.path(), project.path(), None).unwrap();
        assert_eq!(r.path, h.path().join("templates/default.typ"));
        assert!(r.in_templates_dir);
        assert!(r.path.is_file(), "REQ-PDF-012 AC1: generated on first use");
    }

    #[test]
    fn prepare_blank_spec_is_default() {
        let h = handoff();
        let r = prepare_template(h.path(), h.path(), Some("  ")).unwrap();
        assert_eq!(r.path, h.path().join("templates/default.typ"));
    }

    #[test]
    fn prepare_selects_builtin_by_stem_or_file_name() {
        let h = handoff();
        for spec in ["report", "report.typ"] {
            let r = prepare_template(h.path(), h.path(), Some(spec)).unwrap();
            assert_eq!(r.path, h.path().join("templates/report.typ"), "{spec}");
            assert!(r.in_templates_dir);
        }
    }

    #[test]
    fn prepare_selects_a_user_template_added_to_the_templates_dir() {
        let h = handoff();
        fs::create_dir_all(h.path().join("templates")).unwrap();
        fs::write(
            h.path().join("templates/company.typ"),
            "#let conf(doc) = doc",
        )
        .unwrap();
        let r = prepare_template(h.path(), h.path(), Some("company")).unwrap();
        assert_eq!(r.path, h.path().join("templates/company.typ"));
    }

    #[test]
    fn prepare_unknown_name_lists_available_templates() {
        let h = handoff();
        let err = prepare_template(h.path(), h.path(), Some("nope"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("'nope'"), "{err}");
        assert!(err.contains("default, report, spec"), "{err}");
    }

    #[test]
    fn prepare_accepts_a_custom_path_relative_to_project() {
        let h = handoff();
        let project = handoff();
        fs::create_dir_all(project.path().join("design")).unwrap();
        fs::write(
            project.path().join("design/mine.typ"),
            "#let conf(doc) = doc",
        )
        .unwrap();
        let r = prepare_template(h.path(), project.path(), Some("design/mine.typ")).unwrap();
        assert_eq!(r.path, project.path().join("design/mine.typ"));
        assert!(!r.in_templates_dir);
    }

    #[test]
    fn prepare_accepts_an_absolute_custom_path() {
        let h = handoff();
        let elsewhere = handoff();
        let file = elsewhere.path().join("abs.typ");
        fs::write(&file, "#let conf(doc) = doc").unwrap();
        let r = prepare_template(h.path(), h.path(), Some(file.to_str().unwrap())).unwrap();
        assert_eq!(r.path, file);
    }

    #[test]
    fn prepare_rejects_missing_custom_path() {
        let h = handoff();
        let err = prepare_template(h.path(), h.path(), Some("design/none.typ"))
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"), "{err}");
    }

    #[test]
    fn prepare_rejects_non_typ_custom_path() {
        let h = handoff();
        fs::write(h.path().join("t.txt"), "x").unwrap();
        let err = prepare_template(h.path(), h.path(), Some("./t.txt"))
            .unwrap_err()
            .to_string();
        assert!(err.contains(".typ"), "{err}");
    }

    #[test]
    fn dot_prefixed_names_are_paths_not_template_names() {
        // `..` must never be read as a template name joined onto templates/.
        let h = handoff();
        assert!(prepare_template(h.path(), h.path(), Some("..")).is_err());
        assert!(prepare_template(h.path(), h.path(), Some(".hidden")).is_err());
    }

    // --- typst_import_path ---

    #[test]
    fn import_path_is_root_absolute_with_forward_slashes() {
        let root = Path::new("/proj");
        let t = Path::new("/proj/.handoff/templates/default.typ");
        assert_eq!(
            typst_import_path(root, t).unwrap(),
            "/.handoff/templates/default.typ"
        );
    }

    #[test]
    fn import_path_outside_root_is_an_error_naming_both() {
        let err = typst_import_path(Path::new("/proj"), Path::new("/elsewhere/t.typ"))
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("/elsewhere/t.typ") && err.contains("/proj"),
            "{err}"
        );
    }

    #[test]
    fn import_path_rejects_parent_components() {
        let t = Path::new("/proj/../etc/t.typ");
        assert!(typst_import_path(Path::new("/proj"), t).is_err());
    }

    // --- fonts ---

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn select_fonts_prefers_noto_then_ipa_in_order() {
        let available = names(&[
            "IPAMincho",
            "DejaVu Sans Mono",
            "IPAexGothic",
            "Noto Sans CJK JP",
            "Noto Serif CJK JP",
            "Noto Sans CJK KR",
        ]);
        let sel = select_fonts(&available);
        assert_eq!(sel.sans, names(&["Noto Sans CJK JP", "IPAexGothic"]));
        assert_eq!(sel.serif, names(&["Noto Serif CJK JP", "IPAMincho"]));
    }

    #[test]
    fn select_fonts_is_case_insensitive_and_returns_canonical_names() {
        let sel = select_fonts(&["noto sans cjk jp"]);
        assert_eq!(sel.sans, names(&["Noto Sans CJK JP"]));
        assert!(sel.serif.is_empty());
    }

    #[test]
    fn select_fonts_ignores_non_japanese_cjk_families() {
        let sel = select_fonts(&["Noto Sans CJK SC", "Noto Serif CJK KR", "Libertinus Serif"]);
        assert!(sel.is_empty());
    }

    #[test]
    fn missing_warning_only_when_no_japanese_font() {
        assert!(FontSelection::default().missing_warning().is_some());
        let sel = select_fonts(&["IPAGothic"]);
        assert!(sel.missing_warning().is_none());
        let msg = FontSelection::default().missing_warning().unwrap();
        assert!(msg.contains("fonts-noto-cjk"), "{msg}");
    }

    #[test]
    fn parse_typst_fonts_trims_and_skips_blank_lines() {
        let out = "DejaVu Sans Mono\n  Noto Sans CJK JP  \n\nIPAMincho\n";
        assert_eq!(
            parse_typst_fonts(out),
            names(&["DejaVu Sans Mono", "Noto Sans CJK JP", "IPAMincho"])
        );
    }

    #[cfg(unix)]
    #[test]
    fn detect_fonts_runs_typst_fonts_and_selects() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("typst");
        fs::write(
            &fake,
            "#!/bin/sh\n[ \"$1\" = fonts ] || exit 9\nprintf 'Noto Sans CJK JP\\nIPAMincho\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();

        let sel = detect_fonts(&fake).unwrap();

        assert_eq!(sel.sans, names(&["Noto Sans CJK JP"]));
        assert_eq!(sel.serif, names(&["IPAMincho"]));
    }

    #[cfg(unix)]
    #[test]
    fn detect_fonts_surfaces_a_failing_typst() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("typst");
        fs::write(&fake, "#!/bin/sh\necho boom >&2\nexit 2\n").unwrap();
        fs::set_permissions(&fake, fs::Permissions::from_mode(0o755)).unwrap();
        let err = detect_fonts(&fake).unwrap_err().to_string();
        assert!(err.contains("boom"), "{err}");
    }

    // --- preamble ---

    #[test]
    fn typst_string_escapes_quotes_backslashes_and_control_chars() {
        assert_eq!(typst_string(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(typst_string("line1\nline2\ttab"), r#""line1\nline2\ttab""#);
        assert_eq!(typst_string("\u{7}"), r#""\u{7}""#);
        assert_eq!(typst_string("日本語"), "\"日本語\"");
    }

    #[test]
    fn preamble_imports_template_and_passes_metadata_and_fonts() {
        let meta = CoverMeta {
            title: "設計書".into(),
            subtitle: None,
            author: Some("Ryoma".into()),
            date: Some("2026-10-27".into()),
            ..CoverMeta::default()
        };
        let fonts = select_fonts(&["Noto Sans CJK JP", "IPAMincho"]);
        let out = render_preamble("/.handoff/templates/default.typ", &meta, &fonts);

        assert!(out.starts_with("#import \"/.handoff/templates/default.typ\": conf\n"));
        assert!(out.contains("#show: conf.with("));
        assert!(out.contains("title: \"設計書\","));
        assert!(out.contains("author: \"Ryoma\","));
        assert!(out.contains("date: \"2026-10-27\","));
        assert!(out.contains("font-sans: (\"Noto Sans CJK JP\", ),"));
        assert!(out.contains("font-serif: (\"IPAMincho\", ),"));
        assert!(!out.contains("subtitle"));
        assert!(out.ends_with(")\n\n"));
    }

    #[test]
    fn preamble_omits_fonts_when_none_detected() {
        let out = render_preamble(
            "/t.typ",
            &CoverMeta {
                title: "T".into(),
                ..CoverMeta::default()
            },
            &FontSelection::default(),
        );
        assert!(!out.contains("font-sans") && !out.contains("font-serif"));
    }

    #[test]
    fn preamble_passes_show_toc_only_when_set() {
        let render = |show_toc| {
            render_preamble(
                "/t.typ",
                &CoverMeta {
                    title: "T".into(),
                    show_toc,
                    ..CoverMeta::default()
                },
                &FontSelection::default(),
            )
        };
        assert!(!render(None).contains("show-toc"));
        assert!(render(Some(false)).contains("  show-toc: false,\n"));
        assert!(render(Some(true)).contains("  show-toc: true,\n"));
    }

    #[test]
    fn preamble_cannot_be_broken_out_of_by_metadata() {
        let meta = CoverMeta {
            title: "x\"); #panic(\"pwn".into(),
            ..CoverMeta::default()
        };
        let out = render_preamble("/t.typ", &meta, &FontSelection::default());
        assert!(out.contains(r#"title: "x\"); #panic(\"pwn","#), "{out}");
    }
}

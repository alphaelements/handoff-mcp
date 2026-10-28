//! The Typst compiler built into the binary (cargo feature `bundled-typst`,
//! REQ-PDF-040, wiki/390-typst-library-bundling.md).
//!
//! Replaces the external `typst` CLI: the generated `.typ` is compiled by
//! `typst` through `typst-as-lib` and exported by `typst-pdf`, in this
//! process. The project root plays the role of `typst compile --root`: every
//! file the document references (the template, `.handoff/templates/` copies,
//! images) is read from the file system below it, so user-customised templates
//! keep working unchanged.
//!
//! Fonts come from the system plus `TYPST_FONT_PATHS` (like the CLI) and from
//! the Latin / math / monospace fonts the CLI embeds as well — without a math
//! font an equation in a passthrough block cannot compile. No CJK font is
//! embedded: a missing Japanese font stays a warning
//! ([`super::template::FontSelection`]).
//!
//! Packages (`@preview/...`) are not available: there is no downloader in this
//! build, so a passthrough block importing one fails to compile.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Result};
use typst::diag::{Severity, SourceDiagnostic, Warned};
use typst::{World, WorldExt};
use typst_as_lib::typst_kit_options::TypstKitFontOptions;
use typst_as_lib::TypstEngine;
use typst_layout::PagedDocument;

use super::deps::Version;

/// Environment variable listing extra font directories; the Typst CLI reads
/// the same one, so existing setups keep working.
pub const FONT_PATHS_ENV: &str = "TYPST_FONT_PATHS";

/// Version of the embedded compiler (fixed by `Cargo.toml` / `Cargo.lock`).
pub fn version() -> Version {
    let v = typst::utils::version();
    Version::new(v.major(), v.minor(), v.patch())
}

/// The directories named by a `TYPST_FONT_PATHS`-style value (a `PATH`-style
/// list; empty entries are dropped).
pub fn font_dirs(var: Option<&OsStr>) -> Vec<PathBuf> {
    var.map(|v| {
        std::env::split_paths(v)
            .filter(|p| !p.as_os_str().is_empty())
            .collect()
    })
    .unwrap_or_default()
}

fn font_options() -> TypstKitFontOptions {
    TypstKitFontOptions::default()
        .include_dirs(font_dirs(std::env::var_os(FONT_PATHS_ENV).as_deref()))
}

/// Every font family the compiler can see (system fonts, `TYPST_FONT_PATHS`
/// and the embedded fonts), sorted and de-duplicated. Replaces parsing
/// `typst fonts`.
pub fn font_families() -> Result<Vec<String>> {
    families_with(font_options())
}

fn families_with(options: TypstKitFontOptions) -> Result<Vec<String>> {
    let engine = TypstEngine::builder()
        .main_file("")
        .search_fonts_with(options)
        .build();
    let mut families: Vec<String> = engine
        .with_world(|world| {
            world
                .book()
                .families()
                .map(|(name, _)| name.to_string())
                .collect()
        })
        .map_err(|e| anyhow!("Failed to search fonts: {e}"))?;
    families.sort();
    families.dedup();
    Ok(families)
}

/// Compiles `source` — the main file, living at the root-absolute path
/// `main_path` (`/.handoff/exports/<slug>.typ`) so its relative references
/// resolve as they did for the CLI — to PDF bytes. `root` is the project
/// root. On failure the error text lists Typst's diagnostics (file, line,
/// message), the same information `typst compile` printed (REQ-PDF-013 AC2).
pub fn compile_pdf(source: &str, main_path: &str, root: &Path) -> Result<Vec<u8>> {
    let engine = TypstEngine::builder()
        .main_file((main_path, source))
        .search_fonts_with(font_options())
        .with_file_system_resolver(root)
        .build();
    let compiled = engine
        .with_world(|world| -> std::result::Result<Vec<u8>, String> {
            let Warned { output, .. } = typst::compile::<PagedDocument>(world);
            let document = output.map_err(|errors| format_diagnostics(world, &errors))?;
            typst_pdf::pdf(&document, &pdf_options())
                .map_err(|errors| format_diagnostics(world, &errors))
        })
        .map_err(|e| anyhow!("Failed to set up the Typst compiler: {e}"))?;
    compiled.map_err(|diagnostics| anyhow!(diagnostics))
}

fn pdf_options() -> typst_pdf::PdfOptions {
    typst_pdf::PdfOptions::default()
}

/// `error: <message>` plus `at <file>:<line>:<column>` and the hints, one block
/// per diagnostic.
fn format_diagnostics(world: &dyn World, diagnostics: &[SourceDiagnostic]) -> String {
    let mut out = String::new();
    for diagnostic in diagnostics {
        let label = match diagnostic.severity {
            Severity::Error => "error",
            Severity::Warning => "warning",
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(&format!("{label}: {}", diagnostic.message));
        if let Some(location) = location(world, diagnostic) {
            out.push_str(&format!("\n  at {location}"));
        }
        for hint in &diagnostic.hints {
            out.push_str(&format!("\n  hint: {}", hint.v));
        }
    }
    out
}

/// `<path>:<line>:<column>` (1-based) of a diagnostic, when it points into a
/// file.
fn location(world: &dyn World, diagnostic: &SourceDiagnostic) -> Option<String> {
    let id = diagnostic.span.id()?;
    let range = world.range(diagnostic.span)?;
    let source = world.source(id).ok()?;
    let (line, column) = source.lines().byte_to_line_column(range.start)?;
    Some(format!(
        "{}:{}:{}",
        id.vpath().get_with_slash(),
        line + 1,
        column + 1
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAIN: &str = "/main.typ";

    #[test]
    fn font_dirs_splits_a_path_style_list_and_drops_empty_entries() {
        let joined = std::env::join_paths(["/a/fonts", "", "/b/fonts"]).unwrap();
        assert_eq!(
            font_dirs(Some(&joined)),
            vec![PathBuf::from("/a/fonts"), PathBuf::from("/b/fonts")]
        );
        assert!(font_dirs(None).is_empty());
        assert!(font_dirs(Some(OsStr::new(""))).is_empty());
    }

    /// Some font file on this machine, to feed the directory search.
    fn any_system_font() -> Option<PathBuf> {
        let roots = [
            "/usr/share/fonts",
            "/usr/local/share/fonts",
            "/System/Library/Fonts",
            "/Library/Fonts",
            "C:\\Windows\\Fonts",
        ];
        let mut pending: Vec<PathBuf> = roots.iter().map(PathBuf::from).collect();
        while let Some(path) = pending.pop() {
            if path.is_dir() {
                pending.extend(std::fs::read_dir(&path).ok()?.flatten().map(|e| e.path()));
            } else if matches!(
                path.extension().and_then(OsStr::to_str),
                Some("ttf" | "otf")
            ) {
                return Some(path);
            }
        }
        None
    }

    #[test]
    fn embedded_fonts_are_visible_without_any_system_font() {
        // Headless machines have no system fonts; the embedded set (Latin,
        // math, monospace) must still be there so equations compile.
        let none = TypstKitFontOptions::default().include_system_fonts(false);
        let families = families_with(none).unwrap();
        assert!(
            families.iter().any(|f| f == "Libertinus Serif"),
            "{families:?}"
        );
        assert!(
            families.iter().any(|f| f == "New Computer Modern Math"),
            "{families:?}"
        );
    }

    #[test]
    fn font_families_come_from_the_searched_directories_sorted_and_unique() {
        let Some(font) = any_system_font() else {
            eprintln!("SKIPPED: no .ttf/.otf font on this machine");
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        std::fs::copy(&font, dir.path().join(font.file_name().unwrap())).unwrap();
        std::fs::copy(&font, dir.path().join("copy-of-the-same-font.ttf")).unwrap();
        let options = TypstKitFontOptions::default()
            .include_system_fonts(false)
            .include_dirs([dir.path()]);

        let families = families_with(options).unwrap();

        assert!(!families.is_empty(), "the directory's font was not found");
        let mut sorted = families.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(families, sorted, "sorted and unique");
    }

    #[test]
    fn compiles_a_document_to_pdf_bytes() {
        let root = tempfile::tempdir().unwrap();
        let pdf = compile_pdf("= Hello\n\nWorld", MAIN, root.path()).unwrap();
        assert!(pdf.starts_with(b"%PDF-"), "not a PDF");
    }

    #[test]
    fn an_equation_compiles_thanks_to_the_embedded_math_font() {
        let root = tempfile::tempdir().unwrap();
        let pdf = compile_pdf("$ x^2 + y^2 = z^2 $", MAIN, root.path()).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn imports_resolve_below_the_project_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("lib")).unwrap();
        std::fs::write(
            root.path().join("lib/greet.typ"),
            "#let greet(who) = [Hello #who]",
        )
        .unwrap();
        let source = "#import \"/lib/greet.typ\": greet\n#greet(\"root\")";
        let pdf = compile_pdf(source, MAIN, root.path()).unwrap();
        assert!(pdf.starts_with(b"%PDF-"));
    }

    #[test]
    fn a_missing_import_is_an_error_naming_the_file() {
        let root = tempfile::tempdir().unwrap();
        let err = compile_pdf("#import \"/lib/none.typ\": x", MAIN, root.path())
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("error:"), "{err}");
        assert!(err.contains("none.typ"), "{err}");
    }

    #[test]
    fn errors_carry_file_line_column_and_message() {
        let root = tempfile::tempdir().unwrap();
        let err = compile_pdf("= Title\n\n#no-such-function()", MAIN, root.path())
            .unwrap_err()
            .to_string();
        assert!(err.contains("error: unknown variable"), "{err}");
        assert!(err.contains("at /main.typ:3:2"), "{err}");
    }

    #[test]
    fn version_is_the_embedded_compiler_release() {
        let v = version();
        assert!(v >= Version::new(0, 12, 0), "{v}");
    }
}

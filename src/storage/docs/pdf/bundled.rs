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
//! ([`super::template::FontSelection`]). All of them are read once per process
//! into a [`FontStore`] that font detection and every compile share
//! (REQ-PDF-050, wiki/410-font-cache-and-mermaid-caption.md).
//!
//! Packages (`@preview/...`) are not available: there is no downloader in this
//! build, so a passthrough block importing one fails to compile.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use anyhow::{anyhow, Result};
use typst::diag::{Severity, SourceDiagnostic, Warned};
use typst::foundations::Bytes;
use typst::text::{Font, FontBook};
use typst::{World, WorldExt};
use typst_as_lib::TypstEngine;
use typst_kit::fonts::FontPath;
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

/// Where [`FontStore`] looks for fonts.
#[derive(Debug, Clone)]
struct FontSources {
    /// The operating system's font directories.
    system: bool,
    /// Extra directories, searched recursively (`TYPST_FONT_PATHS`).
    dirs: Vec<PathBuf>,
    /// The Latin / math / monospace fonts the Typst CLI embeds.
    embedded: bool,
}

impl FontSources {
    /// What the Typst CLI sees: system fonts, `TYPST_FONT_PATHS`, embedded
    /// fonts.
    fn from_environment() -> Self {
        Self {
            system: true,
            dirs: font_dirs(std::env::var_os(FONT_PATHS_ENV).as_deref()),
            embedded: true,
        }
    }
}

/// Every font the compiler can use, read from disk once.
///
/// `typst-as-lib` keeps a font found by a search (`search_fonts_with`) as a
/// bare path and re-reads the file every time the layout asks for a glyph
/// (a 19 MB CJK collection was read ~1,000 times per export). Handing it
/// already loaded [`Font`]s — their data is reference counted, so a clone is
/// O(1) — means the files are read here, once (wiki/410, REQ-PDF-050).
pub struct FontStore {
    fonts: Vec<Font>,
    book: FontBook,
}

impl FontStore {
    fn load(sources: &FontSources) -> Self {
        let mut fonts = Vec::new();
        if sources.embedded {
            fonts.extend(typst_kit::fonts::embedded().map(|(font, _)| font));
        }
        let mut faces: Vec<FontPath> = Vec::new();
        if sources.system {
            faces.extend(typst_kit::fonts::system().map(|(face, _)| face));
        }
        for dir in &sources.dirs {
            faces.extend(typst_kit::fonts::scan(dir).map(|(face, _)| face));
        }
        fonts.extend(load_faces(faces));

        let mut book = FontBook::new();
        for font in &fonts {
            book.push(font.info().clone());
        }
        Self { fonts, book }
    }

    /// The loaded fonts, in the order of [`FontStore::book`].
    pub fn fonts(&self) -> &[Font] {
        &self.fonts
    }

    pub fn book(&self) -> &FontBook {
        &self.book
    }

    /// Family names, sorted and de-duplicated.
    pub fn families(&self) -> Vec<String> {
        let mut families: Vec<String> = self
            .book
            .families()
            .map(|(name, _)| name.to_string())
            .collect();
        families.sort();
        families.dedup();
        families
    }
}

/// Reads each font file once (a collection holds many faces) and cuts the
/// faces out of the shared bytes. A face that does not parse is skipped: the
/// font database listed it from the file's directory entry, and a file that
/// cannot be read or parsed now cannot be used by the compiler either way.
fn load_faces(faces: Vec<FontPath>) -> Vec<Font> {
    let mut files: HashMap<PathBuf, Option<Bytes>> = HashMap::new();
    let mut fonts = Vec::with_capacity(faces.len());
    for face in faces {
        let data = files
            .entry(face.path.clone())
            .or_insert_with(|| std::fs::read(&face.path).ok().map(Bytes::new));
        if let Some(font) = data
            .as_ref()
            .and_then(|data| Font::new(data.clone(), face.index))
        {
            fonts.push(font);
        }
    }
    fonts
}

#[cfg(test)]
static GLOBAL_LOADS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The process-wide store. Built by the first caller; concurrent callers wait
/// for that one load instead of starting their own. Fonts installed or removed
/// later are not seen until the process restarts (the CLI never sees them
/// mid-run either).
pub fn font_store() -> &'static FontStore {
    static STORE: OnceLock<FontStore> = OnceLock::new();
    STORE.get_or_init(|| {
        #[cfg(test)]
        GLOBAL_LOADS.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        FontStore::load(&FontSources::from_environment())
    })
}

/// Every font family the compiler can see (system fonts, `TYPST_FONT_PATHS`
/// and the embedded fonts), sorted and de-duplicated. Replaces parsing
/// `typst fonts`.
pub fn font_families() -> Result<Vec<String>> {
    Ok(font_store().families())
}

/// Compiles `source` — the main file, living at the root-absolute path
/// `main_path` (`/.handoff/exports/<slug>.typ`) so its relative references
/// resolve as they did for the CLI — to PDF bytes. `root` is the project
/// root. On failure the error text lists Typst's diagnostics (file, line,
/// message), the same information `typst compile` printed (REQ-PDF-013 AC2).
pub fn compile_pdf(source: &str, main_path: &str, root: &Path) -> Result<Vec<u8>> {
    let engine = TypstEngine::builder()
        .main_file((main_path, source))
        .fonts(font_store().fonts().iter().cloned())
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
        let none = FontSources {
            system: false,
            dirs: Vec::new(),
            embedded: true,
        };
        let families = FontStore::load(&none).families();
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
        let sources = FontSources {
            system: false,
            dirs: vec![dir.path().to_path_buf()],
            embedded: false,
        };

        let families = FontStore::load(&sources).families();

        assert!(!families.is_empty(), "the directory's font was not found");
        let mut sorted = families.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(families, sorted, "sorted and unique");
    }

    #[test]
    fn the_store_holds_the_loaded_fonts_and_a_matching_book() {
        let store = FontStore::load(&FontSources {
            system: false,
            dirs: Vec::new(),
            embedded: true,
        });
        assert!(!store.fonts().is_empty());
        for (index, font) in store.fonts().iter().enumerate() {
            assert_eq!(store.book().info(index), Some(font.info()));
        }
    }

    #[test]
    fn without_any_source_the_store_is_empty() {
        let store = FontStore::load(&FontSources {
            system: false,
            dirs: Vec::new(),
            embedded: false,
        });
        assert!(store.fonts().is_empty());
        assert!(store.families().is_empty());
    }

    #[test]
    fn a_missing_font_directory_is_not_an_error() {
        let store = FontStore::load(&FontSources {
            system: false,
            dirs: vec![PathBuf::from("/nonexistent/handoff-fonts")],
            embedded: true,
        });
        assert!(!store.fonts().is_empty());
    }

    #[test]
    fn concurrent_first_use_loads_the_global_store_exactly_once() {
        let addresses: Vec<usize> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..8)
                .map(|_| scope.spawn(|| font_store() as *const FontStore as usize))
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });
        assert!(addresses.windows(2).all(|w| w[0] == w[1]), "{addresses:?}");
        assert_eq!(GLOBAL_LOADS.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn font_families_and_compile_share_the_one_store() {
        let before = GLOBAL_LOADS.load(std::sync::atomic::Ordering::SeqCst);
        let families = font_families().unwrap();
        let root = tempfile::tempdir().unwrap();
        compile_pdf("= Hello", MAIN, root.path()).unwrap();
        assert_eq!(families, font_families().unwrap());
        assert!(
            GLOBAL_LOADS.load(std::sync::atomic::Ordering::SeqCst) <= before.max(1),
            "the store was built more than once"
        );
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

//! REQ-PDF-040: with the `bundled-typst` feature the Japanese-font detection
//! (`template::detect_fonts`) is fed by the compiler's own font search
//! (`TYPST_FONT_PATHS` included), not by parsing `typst fonts`.
//!
//! This is its own test binary, with a single test, because it sets
//! `TYPST_FONT_PATHS`: no other test may read the environment concurrently.
#![cfg(feature = "bundled-typst")]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use handoff_mcp::storage::docs::pdf::template::detect_fonts;

/// A Japanese candidate family name from `template.rs` (`SANS_CANDIDATES`).
const CANDIDATE_FAMILY: &str = "IPAexGothic";

const NAME_ID_FAMILY: u16 = 1;
const NAME_ID_SUBFAMILY: u16 = 2;
const NAME_ID_POSTSCRIPT: u16 = 6;
const NAME_ID_TYPOGRAPHIC_FAMILY: u16 = 16;

/// Some single-font `.ttf` / `.otf` file on this machine.
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
            if let Ok(entries) = std::fs::read_dir(&path) {
                pending.extend(entries.flatten().map(|e| e.path()));
            }
        } else if matches!(
            path.extension().and_then(OsStr::to_str),
            Some("ttf" | "otf")
        ) {
            return Some(path);
        }
    }
    None
}

fn be16(value: u16) -> [u8; 2] {
    value.to_be_bytes()
}

/// A format-0 `name` table holding `family` as family and typographic family
/// plus a subfamily and a PostScript name, all Windows / Unicode BMP / en-US.
fn name_table(family: &str) -> Vec<u8> {
    let utf16 = |s: &str| -> Vec<u8> { s.encode_utf16().flat_map(be16).collect() };
    let records = [
        (NAME_ID_FAMILY, utf16(family)),
        (NAME_ID_SUBFAMILY, utf16("Regular")),
        // The font database drops a font without a PostScript name.
        (
            NAME_ID_POSTSCRIPT,
            utf16(&format!("{}-Regular", family.replace(' ', ""))),
        ),
        (NAME_ID_TYPOGRAPHIC_FAMILY, utf16(family)),
    ];
    let storage_offset = 6 + 12 * records.len();
    let mut table = Vec::new();
    table.extend(be16(0)); // format
    table.extend(be16(records.len() as u16));
    table.extend(be16(storage_offset as u16));
    let mut storage: Vec<u8> = Vec::new();
    for (id, string) in &records {
        for field in [3, 1, 0x409, *id, string.len() as u16, storage.len() as u16] {
            table.extend(be16(field));
        }
        storage.extend(string.iter().copied());
    }
    table.extend(storage);
    table
}

/// `font` with its `name` table replaced by one naming the family `family`.
/// The new table is appended to the file and the table directory repointed;
/// glyph data is untouched.
fn rename_family(font: &[u8], family: &str) -> Vec<u8> {
    let u16_at = |at: usize| u16::from_be_bytes([font[at], font[at + 1]]) as usize;
    let tables = u16_at(4);
    let entry = (0..tables)
        .map(|i| 12 + 16 * i)
        .find(|&at| &font[at..at + 4] == b"name")
        .expect("font has a name table");
    let mut out = font.to_vec();
    while out.len() % 4 != 0 {
        out.push(0);
    }
    let table = name_table(family);
    let offset = out.len() as u32;
    out[entry + 8..entry + 12].copy_from_slice(&offset.to_be_bytes());
    out[entry + 12..entry + 16].copy_from_slice(&(table.len() as u32).to_be_bytes());
    out.extend(table);
    out
}

#[test]
fn detect_fonts_sees_a_japanese_family_in_typst_font_paths() {
    let Some(font) = any_system_font() else {
        eprintln!("SKIPPED: no .ttf/.otf font on this machine");
        return;
    };
    let original = std::fs::read(&font).unwrap();
    if original.len() < 12 || !matches!(&original[..4], b"\0\x01\0\0" | b"OTTO" | b"true") {
        eprintln!("SKIPPED: {} is not a single-font sfnt", font.display());
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("renamed.ttf"),
        rename_family(&original, CANDIDATE_FAMILY),
    )
    .unwrap();
    std::env::set_var("TYPST_FONT_PATHS", dir.path());

    let selection = detect_fonts(Path::new("/nonexistent/typst")).expect("font search");

    assert!(
        selection.sans.iter().any(|f| f == CANDIDATE_FAMILY),
        "the family from TYPST_FONT_PATHS must reach the selection: {selection:?}"
    );
    assert!(selection.missing_warning().is_none());
}

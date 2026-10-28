#![cfg(feature = "bundled-typst")]
//! PoC benchmark: font pre-loading vs search_fonts_with.
//!
//! Run: cargo test --features bundled-typst --release bench_font_cache_poc -- --nocapture

use std::path::Path;
use std::time::Instant;

use typst::text::Font;
use typst_as_lib::typst_kit_options::TypstKitFontOptions;
use typst_as_lib::TypstEngine;
use typst_kit::fonts::FontSource;

const MAIN_PATH: &str = "/main.typ";

const CJK_SOURCE: &str = "\
= Hello

こんにちは世界。日本語テストドキュメント。

これはフォントキャッシュのベンチマーク用テキストです。
漢字、ひらがな、カタカナ、英数字を含みます。
ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789

== セクション2

テーブルも含めてテスト:

#table(
  columns: 3,
  [項目], [値], [備考],
  [テスト1], [100], [正常],
  [テスト2], [200], [警告],
)

$ x^2 + y^2 = z^2 $

== セクション3

#lorem(200)
";

fn font_options() -> TypstKitFontOptions {
    TypstKitFontOptions::default()
}

fn preload_fonts() -> Vec<Font> {
    let mut fonts: Vec<Font> = Vec::new();

    for (font, _info) in typst_kit::fonts::embedded() {
        fonts.push(font);
    }

    for (font_path, _info) in typst_kit::fonts::system() {
        if let Some(font) = font_path.load() {
            fonts.push(font);
        }
    }

    if let Some(paths) = std::env::var_os("TYPST_FONT_PATHS") {
        for dir in std::env::split_paths(&paths).filter(|p| !p.as_os_str().is_empty()) {
            for (font_path, _info) in typst_kit::fonts::scan(&dir) {
                if let Some(font) = font_path.load() {
                    fonts.push(font);
                }
            }
        }
    }

    fonts
}

fn compile_with_search_fonts(source: &str, root: &Path) -> Vec<u8> {
    let engine = TypstEngine::builder()
        .main_file((MAIN_PATH, source))
        .search_fonts_with(font_options())
        .with_file_system_resolver(root)
        .build();

    engine
        .with_world(|world| -> Result<Vec<u8>, String> {
            let warned = typst::compile::<typst_layout::PagedDocument>(world);
            let doc = warned.output.map_err(|e| format!("{e:?}"))?;
            typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()).map_err(|e| format!("{e:?}"))
        })
        .unwrap()
        .unwrap()
}

fn compile_with_preloaded(source: &str, root: &Path, fonts: Vec<Font>) -> Vec<u8> {
    let engine = TypstEngine::builder()
        .main_file((MAIN_PATH, source))
        .fonts(fonts)
        .with_file_system_resolver(root)
        .build();

    engine
        .with_world(|world| -> Result<Vec<u8>, String> {
            let warned = typst::compile::<typst_layout::PagedDocument>(world);
            let doc = warned.output.map_err(|e| format!("{e:?}"))?;
            typst_pdf::pdf(&doc, &typst_pdf::PdfOptions::default()).map_err(|e| format!("{e:?}"))
        })
        .unwrap()
        .unwrap()
}

#[test]
fn bench_font_cache_poc() {
    let root = tempfile::tempdir().unwrap();

    // --- Pattern A: search_fonts_with (current) ---
    println!("\n=== Pattern A: search_fonts_with (current, uncached) ===");
    let start = Instant::now();
    let pdf_a = compile_with_search_fonts(CJK_SOURCE, root.path());
    let elapsed_a = start.elapsed();
    println!("  Time: {:.3}s", elapsed_a.as_secs_f64());
    println!("  PDF size: {} bytes", pdf_a.len());

    // --- Pattern B: pre-loaded fonts ---
    println!("\n=== Pattern B: pre-loaded fonts ===");

    // Step 1: measure font loading time (one-time cost)
    let start_load = Instant::now();
    let fonts = preload_fonts();
    let load_time = start_load.elapsed();
    println!(
        "  Font pre-load: {:.3}s ({} fonts)",
        load_time.as_secs_f64(),
        fonts.len()
    );

    // Step 2: compile with pre-loaded fonts (first call)
    let start = Instant::now();
    let pdf_b1 = compile_with_preloaded(CJK_SOURCE, root.path(), fonts.clone());
    let elapsed_b1 = start.elapsed();
    println!("  Compile (1st): {:.3}s", elapsed_b1.as_secs_f64());
    println!("  PDF size: {} bytes", pdf_b1.len());

    // Step 3: compile again (simulates 2nd export — fonts already loaded)
    let start = Instant::now();
    let pdf_b2 = compile_with_preloaded(CJK_SOURCE, root.path(), fonts.clone());
    let elapsed_b2 = start.elapsed();
    println!("  Compile (2nd): {:.3}s", elapsed_b2.as_secs_f64());
    println!("  PDF size: {} bytes", pdf_b2.len());

    // Step 4: compile a third time
    let start = Instant::now();
    let _pdf_b3 = compile_with_preloaded(CJK_SOURCE, root.path(), fonts);
    let elapsed_b3 = start.elapsed();
    println!("  Compile (3rd): {:.3}s", elapsed_b3.as_secs_f64());

    // --- Summary ---
    println!("\n=== Summary ===");
    println!(
        "  Pattern A (search_fonts_with): {:.3}s",
        elapsed_a.as_secs_f64()
    );
    println!(
        "  Pattern B font load (one-time): {:.3}s",
        load_time.as_secs_f64()
    );
    println!(
        "  Pattern B compile (1st):        {:.3}s",
        elapsed_b1.as_secs_f64()
    );
    println!(
        "  Pattern B compile (2nd):        {:.3}s",
        elapsed_b2.as_secs_f64()
    );
    println!(
        "  Pattern B compile (3rd):        {:.3}s",
        elapsed_b3.as_secs_f64()
    );
    let speedup = elapsed_a.as_secs_f64() / elapsed_b1.as_secs_f64();
    println!("  Speedup (A / B-1st):            {:.1}x", speedup);
    if elapsed_b2.as_secs_f64() > 0.0 {
        let speedup2 = elapsed_a.as_secs_f64() / elapsed_b2.as_secs_f64();
        println!("  Speedup (A / B-2nd):            {:.1}x", speedup2);
    }

    assert!(
        pdf_a.starts_with(b"%PDF-"),
        "Pattern A did not produce a PDF"
    );
    assert!(
        pdf_b1.starts_with(b"%PDF-"),
        "Pattern B did not produce a PDF"
    );
}

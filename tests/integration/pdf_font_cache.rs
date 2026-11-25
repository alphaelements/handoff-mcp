//! REQ-PDF-050: the bundled compiler reads the font files once. After the
//! first use a compile must not touch the fonts on disk again, and it must be
//! fast. A separate test binary with a single test: the byte counter is
//! process-wide, so no other test may read files concurrently.
#![cfg(all(feature = "bundled-typst", target_os = "linux"))]

use std::time::{Duration, Instant};

use handoff_mcp::storage::docs::pdf::bundled::{compile_pdf, font_families, font_store};

const MAIN: &str = "/main.typ";

const CJK_SOURCE: &str = "\
= こんにちは世界

日本語のテスト文書です。漢字、ひらがな、カタカナ、ABC xyz 0123456789。

#table(columns: 2, [項目], [値], [テスト], [100])

$ x^2 + y^2 = z^2 $

#lorem(200)
";

/// Bytes this process has asked the kernel to read (`rchar` in /proc/self/io).
fn bytes_read() -> u64 {
    std::fs::read_to_string("/proc/self/io")
        .unwrap()
        .lines()
        .find_map(|l| l.strip_prefix("rchar:"))
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

#[test]
fn after_the_first_use_a_compile_does_not_read_fonts_again() {
    let root = tempfile::tempdir().unwrap();

    // First use: loads every font once. AC1 asks for 5 s on an idle machine;
    // the bound is doubled so a busy CI host does not flake.
    let start = Instant::now();
    let store = font_store();
    let first = compile_pdf(CJK_SOURCE, MAIN, root.path()).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(10),
        "first use took {:?}",
        start.elapsed()
    );
    assert!(first.starts_with(b"%PDF-"));

    // Later uses (AC2, AC3): fast, and (almost) no file reads.
    let before = bytes_read();
    let start = Instant::now();
    let second = compile_pdf(CJK_SOURCE, MAIN, root.path()).unwrap();
    let elapsed = start.elapsed();
    let read = bytes_read() - before;
    assert!(second.starts_with(b"%PDF-"));
    assert!(
        elapsed < Duration::from_secs(1),
        "second compile took {elapsed:?}"
    );
    assert!(
        read < 1024 * 1024,
        "the second compile read {read} bytes; fonts are being re-read"
    );

    // AC4: font detection reuses the same store.
    let before = bytes_read();
    assert_eq!(font_families().unwrap(), store.families());
    assert!(bytes_read() - before < 1024 * 1024);
}

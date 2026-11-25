//! Sanity checks for the PDF export E2E fixtures in `tests/fixtures/pdf/`
//! (t-pdf-export.8, REQ-PDF-001/003/010), plus the Mermaid pre-conversion
//! (REQ-PDF-003) run against them with and without `mmdc`.

use std::path::{Path, PathBuf};
use std::time::Duration;

use handoff_mcp::storage::docs::pdf::mermaid::{
    convert_mermaid_blocks_with, find_mermaid_blocks, MermaidWarning,
};

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/pdf")
}

fn read(name: &str) -> String {
    let path = fixtures_dir().join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn basic_japanese_covers_headings_lists_tables_emphasis_links() {
    let md = read("basic_japanese.md");
    for level in 1..=6 {
        let prefix = format!("\n{} ", "#".repeat(level));
        assert!(md.contains(&prefix), "missing h{level}");
    }
    assert!(md.contains("\n- ") && md.contains("\n1. "), "missing ul/ol");
    assert!(md.contains("|---"), "missing table");
    assert!(
        md.contains("**") && md.contains("]("),
        "missing emphasis/link"
    );
    assert!(
        md.chars().any(|c| ('\u{3040}'..='\u{30ff}').contains(&c)),
        "no kana"
    );
}

#[test]
fn mermaid_diagrams_has_flowchart_sequence_and_state() {
    let blocks = find_mermaid_blocks(&read("mermaid_diagrams.md"));
    let heads: Vec<_> = blocks
        .iter()
        .map(|b| b.source.split_whitespace().next().unwrap().to_string())
        .collect();
    assert_eq!(heads, ["flowchart", "sequenceDiagram", "stateDiagram-v2"]);
}

#[test]
fn typst_passthrough_has_math_table_and_figure() {
    let md = read("typst_passthrough.md");
    assert!(md.contains("```typst"));
    assert!(md.contains("$ f_c = 1 / (2 pi R C) $"));
    assert!(md.contains("#table(") && md.contains("#figure("));
    assert!(find_mermaid_blocks(&md).is_empty());
}

#[test]
fn images_fixture_references_existing_png_and_svg() {
    let md = read("images.md");
    for asset in ["assets/test.png", "assets/test.svg"] {
        assert!(
            md.contains(&format!("]({asset})")),
            "{asset} not referenced"
        );
        assert!(fixtures_dir().join(asset).is_file(), "{asset} missing");
    }
    let png = std::fs::read(fixtures_dir().join("assets/test.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    let svg = read("assets/test.svg");
    assert!(svg.trim_start().starts_with("<svg") && svg.trim_end().ends_with("</svg>"));
}

#[test]
fn edge_cases_has_missing_image_invalid_and_empty_mermaid() {
    let md = read("edge_cases.md");
    assert!(md.contains("assets/does-not-exist.png"));
    assert!(!fixtures_dir().join("assets/does-not-exist.png").exists());
    let blocks = find_mermaid_blocks(&md);
    assert_eq!(blocks.len(), 2);
    assert!(blocks[0].source.contains("not a valid mermaid"));
    assert!(blocks[1].source.trim().is_empty());
}

#[test]
fn empty_document_fixture_is_empty() {
    assert_eq!(read("empty.md"), "");
}

#[test]
fn without_mmdc_every_fixture_survives_unchanged_with_a_warning() {
    let cache = tempfile::tempdir().unwrap();
    let md = read("mermaid_diagrams.md");
    let out = convert_mermaid_blocks_with(&md, cache.path(), None, Duration::from_secs(1));
    assert_eq!(out.markdown, md);
    assert_eq!(
        out.warnings,
        vec![MermaidWarning::MmdcNotFound { blocks: 3 }]
    );

    for name in [
        "basic_japanese.md",
        "typst_passthrough.md",
        "images.md",
        "empty.md",
    ] {
        let md = read(name);
        let out = convert_mermaid_blocks_with(&md, cache.path(), None, Duration::from_secs(1));
        assert_eq!(out.markdown, md, "{name}");
        assert!(out.warnings.is_empty(), "{name}: {:?}", out.warnings);
    }
}

#[cfg(unix)]
#[test]
fn invalid_and_empty_mermaid_in_edge_cases_fall_back_with_per_block_warnings() {
    use std::os::unix::fs::PermissionsExt;
    let tools = tempfile::tempdir().unwrap();
    let mmdc = tools.path().join("mmdc");
    // Stand-in mmdc: rejects anything that is not a recognised diagram header.
    std::fs::write(
        &mmdc,
        "#!/bin/sh\nif grep -qE '^(flowchart|sequenceDiagram|stateDiagram)' \"$2\"; then \
         echo '<svg/>' > \"$4\"; else echo 'Parse error' >&2; exit 1; fi\n",
    )
    .unwrap();
    std::fs::set_permissions(&mmdc, std::fs::Permissions::from_mode(0o755)).unwrap();

    let cache = tools.path().join("cache");
    let md = read("edge_cases.md");
    let mut out = convert_mermaid_blocks_with(&md, &cache, Some(&mmdc), Duration::from_secs(5));
    assert_eq!(out.markdown, md);
    assert_eq!(out.warnings.len(), 2);

    // Valid diagrams convert and clean up fully.
    let md = read("mermaid_diagrams.md");
    out = convert_mermaid_blocks_with(&md, &cache, Some(&mmdc), Duration::from_secs(5));
    assert!(out.warnings.is_empty(), "{:?}", out.warnings);
    assert_eq!(out.generated_files().len(), 3);
    assert!(!out.markdown.contains("```mermaid"));
    out.cleanup().unwrap();
    assert!(!cache.exists());
}

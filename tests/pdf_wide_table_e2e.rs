//! End-to-end: a wide traceability / verification-results table is laid out on
//! a landscape page by the real Typst CLI (REQ-PDF-029).
//!
//! A plain `cargo test` works without Typst: the test prints a notice and
//! passes. Set `HANDOFF_REQUIRE_TYPST=1` (as CI should) to turn "typst
//! missing" into a failure.

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
use handoff_mcp::storage::docs::pdf::export::{export_pdf, ExportRequest};
use handoff_mcp::storage::docs::pdf::template::CoverMeta;

const ROWS: usize = 60;

fn typst_available() -> bool {
    match detect(Tool::Typst) {
        ToolStatus::Found { .. } => true,
        other => {
            assert!(
                std::env::var_os("HANDOFF_REQUIRE_TYPST").is_none(),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            false
        }
    }
}

/// The shape of the `## Verification Results` table the verification report
/// template produces, with `rows` data rows.
fn verification_report(rows: usize) -> String {
    let mut md = String::from(
        "# Verification Report\n\n## Verification Results\n\n\
         | ID | Title | Layer | Result | Date | Verifier | Evidence | Note |\n\
         |----|-------|-------|--------|------|----------|----------|------|\n",
    );
    for i in 0..rows {
        md.push_str(&format!(
            "| REQ-PDF-{i:03} | トレーサビリティマトリクスの検証項目 {i} {} | requirements | pass | 2026-10-27T11:35:54Z | alice | [run {i}](https://example.com/runs/{i}) | 手動確認済み |\n",
            "とても長いタイトル".repeat(12),
        ));
    }
    md.push_str("\n## Summary\n\nAll rows pass.\n");
    md
}

/// Counts landscape (`/MediaBox [0 0 W H]` with W > H) and portrait pages.
fn page_orientations(pdf: &[u8]) -> (usize, usize) {
    let text = String::from_utf8_lossy(pdf);
    let (mut landscape, mut portrait) = (0, 0);
    for chunk in text.split("/MediaBox").skip(1) {
        let Some(open) = chunk.find('[') else {
            continue;
        };
        let Some(close) = chunk.find(']') else {
            continue;
        };
        let dims: Vec<f64> = chunk[open + 1..close]
            .split_whitespace()
            .filter_map(|n| n.parse().ok())
            .collect();
        if let [_, _, w, h] = dims[..] {
            if w > h {
                landscape += 1;
            } else {
                portrait += 1;
            }
        }
    }
    (landscape, portrait)
}

#[test]
fn sixty_row_matrix_renders_on_landscape_pages() {
    if !typst_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let handoff = root.join(".handoff");
    std::fs::create_dir_all(&handoff).unwrap();
    let md = verification_report(ROWS);

    let out = export_pdf(&ExportRequest {
        project_root: root,
        handoff_dir: &handoff,
        doc_slug: "matrix",
        markdown: &md,
        template: Some("report"),
        meta: CoverMeta {
            title: "Verification Report".into(),
            date: Some("2026-10-27".into()),
            ..CoverMeta::default()
        },
    })
    .expect("a 60-row matrix must compile");

    let typ = std::fs::read_to_string(&out.typ_path).unwrap();
    assert!(typ.contains("#page(flipped: true)["), "{typ}");
    assert_eq!(typ.matches("[REQ-PDF-").count(), ROWS);

    let pdf = std::fs::read(&out.pdf_path).unwrap();
    assert!(pdf.starts_with(b"%PDF"));
    let (landscape, portrait) = page_orientations(&pdf);
    assert!(
        landscape >= 2,
        "60 rows need several landscape pages (landscape={landscape}, portrait={portrait})"
    );
    assert!(portrait >= 1, "cover / contents stay portrait");
}

#[test]
fn narrow_table_report_stays_portrait() {
    if !typst_available() {
        return;
    }
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path();
    let handoff = root.join(".handoff");
    std::fs::create_dir_all(&handoff).unwrap();

    let out = export_pdf(&ExportRequest {
        project_root: root,
        handoff_dir: &handoff,
        doc_slug: "narrow",
        markdown: "# R\n\n| A | B |\n|---|---|\n| 1 | 2 |\n",
        template: Some("report"),
        meta: CoverMeta {
            title: "R".into(),
            date: Some("2026-10-27".into()),
            ..CoverMeta::default()
        },
    })
    .unwrap();
    let (landscape, _) = page_orientations(&std::fs::read(&out.pdf_path).unwrap());
    assert_eq!(landscape, 0);
}

//! End-to-end: compile every built-in Typst template with the real `typst`
//! binary on a Japanese document (REQ-PDF-002 / 004 / 012).
//!
//! These tests need a Typst CLI (>= 0.12) and at least one Japanese font. When
//! `typst` is not installed they print a notice and pass, so a plain
//! `cargo test` works on machines without it; set `HANDOFF_REQUIRE_TYPST=1`
//! (as CI should) to turn "typst missing" into a failure instead.

use std::path::Path;
use std::process::Command;

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
use handoff_mcp::storage::docs::pdf::template::{
    detect_fonts, prepare_template, render_preamble, typst_import_path, CoverMeta,
};

const BODY: &str = "= 概要\n\
これは日本語の本文です。ひらがな、カタカナ、漢字が正しく表示されること。\n\n\
== 詳細\n\
- 箇条書き一\n- 箇条書き二\n\n\
#table(columns: 2, [項目], [内容], [名前], [ハンドオフ])\n\n\
```rust\nfn main() { println!(\"日本語\"); }\n```\n";

/// Returns the usable `typst` path, or `None` when the test should be skipped.
fn typst_or_skip() -> Option<std::path::PathBuf> {
    match detect(Tool::Typst) {
        ToolStatus::Found { path, .. } => Some(path),
        other => {
            assert!(
                std::env::var_os("HANDOFF_REQUIRE_TYPST").is_none(),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            None
        }
    }
}

fn compile(typst: &Path, root: &Path, input: &Path, output: &Path) -> std::process::Output {
    Command::new(typst)
        .arg("compile")
        .arg("--root")
        .arg(root)
        .arg(input)
        .arg(output)
        .output()
        .expect("spawn typst")
}

fn export_with(template_spec: Option<&str>) {
    let Some(typst) = typst_or_skip() else { return };
    let project = tempfile::tempdir().unwrap();
    let handoff = project.path().join(".handoff");
    std::fs::create_dir_all(handoff.join("exports")).unwrap();

    let template = prepare_template(&handoff, project.path(), template_spec).unwrap();
    let import = typst_import_path(project.path(), &template.path).unwrap();
    let fonts = detect_fonts(&typst).unwrap();
    assert!(
        !fonts.is_empty(),
        "no Japanese font visible to Typst: {:?}",
        fonts.missing_warning()
    );
    let meta = CoverMeta {
        title: "日本語テスト文書".into(),
        subtitle: Some("テンプレート検証".into()),
        author: Some("handoff-mcp".into()),
        date: Some("2026-10-27".into()),
    };
    let source = format!("{}{BODY}", render_preamble(&import, &meta, &fonts));
    let typ = handoff.join("exports/doc.typ");
    std::fs::write(&typ, source).unwrap();
    let pdf = handoff.join("exports/doc.pdf");

    let out = compile(&typst, project.path(), &typ, &pdf);
    assert!(
        out.status.success(),
        "typst compile failed for {template_spec:?}:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let bytes = std::fs::read(&pdf).unwrap();
    assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
    assert!(
        bytes.len() > 2_000,
        "suspiciously small PDF: {}",
        bytes.len()
    );

    // When poppler is around, check the Japanese text really made it into the
    // PDF (a CJK font missing at compile time yields no extractable text).
    if let Ok(o) = Command::new("pdftotext").arg(&pdf).arg("-").output() {
        if o.status.success() {
            let text = String::from_utf8_lossy(&o.stdout);
            assert!(text.contains("日本語の本文"), "text missing:\n{text}");
            assert!(
                text.contains("日本語テスト文書"),
                "cover title missing:\n{text}"
            );
        }
    }
}

#[test]
fn default_template_renders_japanese_pdf() {
    export_with(None);
}

#[test]
fn report_template_renders_japanese_pdf() {
    export_with(Some("report"));
}

#[test]
fn spec_template_renders_japanese_pdf() {
    export_with(Some("spec"));
}

#[test]
fn custom_template_path_is_applied() {
    let Some(typst) = typst_or_skip() else { return };
    let project = tempfile::tempdir().unwrap();
    let handoff = project.path().join(".handoff");
    std::fs::create_dir_all(handoff.join("exports")).unwrap();
    std::fs::create_dir_all(project.path().join("design")).unwrap();
    std::fs::write(
        project.path().join("design/mine.typ"),
        "#let conf(title: none, doc, ..args) = { [CUSTOM-MARKER #title]; doc }\n",
    )
    .unwrap();

    let template = prepare_template(&handoff, project.path(), Some("design/mine.typ")).unwrap();
    let import = typst_import_path(project.path(), &template.path).unwrap();
    let meta = CoverMeta {
        title: "T".into(),
        ..CoverMeta::default()
    };
    let typ = handoff.join("exports/doc.typ");
    std::fs::write(
        &typ,
        format!(
            "{}body\n",
            render_preamble(&import, &meta, &Default::default())
        ),
    )
    .unwrap();
    let pdf = handoff.join("exports/doc.pdf");

    let out = compile(&typst, project.path(), &typ, &pdf);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if let Ok(o) = Command::new("pdftotext").arg(&pdf).arg("-").output() {
        if o.status.success() {
            assert!(String::from_utf8_lossy(&o.stdout).contains("CUSTOM-MARKER"));
        }
    }
}

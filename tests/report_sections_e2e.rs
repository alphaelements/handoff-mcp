//! REQ-PDF-034 / REQ-PDF-035: `handoff_report generate` composes a report
//! from `sections` (order, content, charts / trace diagram / AI sections),
//! and renders it with a custom `.handoff/templates/<name>.md.hbs` selected by
//! `report_template`.
//!
//! The PDF test needs the Typst CLI; without it the test prints `SKIPPED` and
//! passes, unless `HANDOFF_REQUIRE_TYPST` is set.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
use handoff_mcp::storage::tasks::{write_task, TaskData};
use serde_json::{json, Value};
use tempfile::TempDir;

fn call(name: &str, arguments: Value) -> Value {
    let req = json!({
        "jsonrpc": "2.0", "id": 1,
        "method": "tools/call",
        "params": { "name": name, "arguments": arguments }
    });
    let out = handoff_mcp::mcp::protocol::process_line(&req.to_string()).expect("response");
    serde_json::from_str(&out).expect("valid JSON response")
}

fn text(resp: &Value) -> String {
    resp["result"]["content"][0]["text"]
        .as_str()
        .expect("text content")
        .to_string()
}

fn is_error(resp: &Value) -> bool {
    resp["result"]["isError"].as_bool().unwrap_or(false)
}

fn ok(name: &str, arguments: Value) -> Value {
    let resp = call(name, arguments.clone());
    assert!(
        !is_error(&resp),
        "{name} {arguments} failed: {}",
        text(&resp)
    );
    serde_json::from_str(&text(&resp)).unwrap_or(Value::Null)
}

fn err(arguments: Value) -> String {
    let resp = call("handoff_report", arguments.clone());
    assert!(is_error(&resp), "expected an error for {arguments}");
    text(&resp)
}

fn setup() -> (TempDir, String) {
    let dir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    ok(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "sections" }),
    );
    (dir, pd)
}

fn handoff(dir: &TempDir) -> PathBuf {
    dir.path().join(".handoff")
}

fn put_task(handoff: &Path, status: &str, id: &str, f: impl FnOnce(&mut TaskData)) {
    let mut data = TaskData {
        id: id.into(),
        title: format!("Title of {id}"),
        notes: None,
        priority: None,
        created_at: Some("2026-10-01T09:00:00+00:00".into()),
        updated_at: None,
        completed_at: None,
        labels: vec![],
        links: vec![],
        task_links: vec![],
        done_criteria: vec![],
        schedule: None,
        dependencies: vec![],
        order: None,
        assignee: None,
        lock: None,
        scope_paths: vec![],
        extra: HashMap::new(),
    };
    f(&mut data);
    let dir = handoff.join("tasks").join(id);
    std::fs::create_dir_all(&dir).unwrap();
    write_task(&dir, status, &data).unwrap();
}

fn seed_progress(dir: &TempDir) {
    let h = handoff(dir);
    put_task(&h, "done", "t1", |d| {
        d.completed_at = Some("2026-10-06T09:00:00+00:00".into());
    });
    put_task(&h, "in_progress", "t3", |_| {});
}

/// Generates a weekly report with `extra` arguments; returns (response, body).
fn weekly(dir: &TempDir, pd: &str, extra: Value) -> (Value, String) {
    let mut args = json!({ "project_dir": pd, "action": "generate",
        "report_type": "weekly", "scope": { "period": "2026-W41" } });
    for (k, v) in extra.as_object().unwrap() {
        args[k] = v.clone();
    }
    let out = ok("handoff_report", args);
    let path = out["report"]["output_path"].as_str().unwrap();
    let body = std::fs::read_to_string(handoff(dir).join(path)).unwrap();
    (out, body)
}

fn pos(md: &str, needle: &str) -> usize {
    md.find(needle)
        .unwrap_or_else(|| panic!("missing {needle:?}:\n{md}"))
}

fn write_template(dir: &TempDir, name: &str, source: &str) {
    let templates = handoff(dir).join("templates");
    std::fs::create_dir_all(&templates).unwrap();
    std::fs::write(templates.join(format!("{name}.md.hbs")), source).unwrap();
}

// ---- sections (REQ-PDF-034) -----------------------------------------------

#[test]
fn without_sections_the_full_report_is_unchanged() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (out, md) = weekly(&dir, &pd, json!({}));
    assert!(md.contains("## Time Log"), "{md}");
    assert!(md.contains("## Next Week"), "{md}");
    assert!(out.get("section_warnings").is_none(), "{out}");
}

#[test]
fn sections_select_and_order_the_report_content() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (out, md) = weekly(
        &dir,
        &pd,
        json!({
            "data": { "ai_summary": "All good." },
            "sections": [
                { "type": "task_list" },
                { "type": "ai_summary" },
                { "type": "metrics" },
            ],
        }),
    );
    assert!(md.starts_with("# Weekly Report"), "title block kept:\n{md}");
    assert!(md.contains("- Report ID: "), "{md}");
    let order = [
        pos(&md, "## Completed Tasks"),
        pos(&md, "## In Progress"),
        pos(&md, "## Blockers"),
        pos(&md, "## AI Summary\n\nAll good."),
        pos(&md, "## Summary"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{md}");
    for gone in ["## Time Log", "## Milestones", "## Next Week", "## Charts"] {
        assert!(!md.contains(gone), "{gone} should be left out:\n{md}");
    }
    assert!(out.get("section_warnings").is_none(), "{out}");
}

#[test]
fn any_builtin_section_can_be_picked_by_heading() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (_, md) = weekly(
        &dir,
        &pd,
        json!({ "sections": [
            { "type": "section", "heading": "next week" },
            { "type": "section", "heading": "Time Log" },
        ] }),
    );
    assert!(pos(&md, "## Next Week") < pos(&md, "## Time Log"), "{md}");
    assert!(!md.contains("## Summary"), "{md}");
}

#[test]
fn sections_control_whether_charts_are_generated() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (_, with) = weekly(
        &dir,
        &pd,
        json!({ "sections": [{ "type": "metrics" }, { "type": "charts" }] }),
    );
    assert!(with.contains("```mermaid"), "{with}");
    assert!(with.contains("## Charts"), "{with}");

    let (_, without) = weekly(&dir, &pd, json!({ "sections": [{ "type": "metrics" }] }));
    assert!(!without.contains("```mermaid"), "{without}");
    assert!(!without.contains("## Charts"), "{without}");
}

#[test]
fn include_charts_false_contradicts_a_charts_section() {
    let (_dir, pd) = setup();
    let msg = err(json!({ "project_dir": pd, "action": "generate",
        "report_type": "weekly", "scope": { "period": "2026-W41" },
        "include_charts": false, "sections": [{ "type": "charts" }] }));
    assert!(msg.contains("include_charts"), "{msg}");
    assert!(msg.contains("charts"), "{msg}");
}

#[test]
fn trace_diagram_section_controls_the_diagram() {
    let (dir, pd) = setup();
    ok(
        "handoff_doc_save",
        json!({ "project_dir": pd, "slug": "reqs", "title": "reqs", "layer": "requirement",
                "body": "# R\n\n## SubItems\n\n- [FR-1] first\n" }),
    );
    let gen = |sections: Value| -> String {
        let out = ok(
            "handoff_report",
            json!({ "project_dir": pd, "action": "generate",
                    "report_type": "verification", "sections": sections }),
        );
        let path = out["report"]["output_path"].as_str().unwrap();
        std::fs::read_to_string(handoff(&dir).join(path)).unwrap()
    };
    let with = gen(json!([{ "type": "metrics" }, { "type": "trace_diagram" }]));
    let without = gen(json!([{ "type": "metrics" }]));
    assert!(without.contains("## Summary"), "{without}");
    assert!(!without.contains("## Traceability Diagram"), "{without}");
    assert!(!without.contains("graph TD"), "{without}");
    // The diagram appears whenever the project has trace items to draw.
    if with.contains("## Traceability Diagram") {
        assert!(with.contains("graph TD"), "{with}");
    }
}

#[test]
fn custom_section_adds_literal_markdown_with_a_title() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (_, md) = weekly(
        &dir,
        &pd,
        json!({ "sections": [
            { "type": "custom", "title": "Notes", "body": "Literal {{not_a_template}} text." },
            { "type": "metrics" },
        ] }),
    );
    assert!(
        md.contains("## Notes\n\nLiteral {{not_a_template}} text.\n"),
        "{md}"
    );
    assert!(pos(&md, "## Notes") < pos(&md, "## Summary"), "{md}");
}

#[test]
fn custom_section_can_render_a_template_with_the_report_data() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    write_template(&dir, "highlight", "## Highlight\n\nTop: {{data.top}}\n");
    let (_, md) = weekly(
        &dir,
        &pd,
        json!({ "data": { "top": "ship it" },
                "sections": [{ "type": "custom", "template": "highlight" }] }),
    );
    assert!(md.contains("## Highlight\n\nTop: ship it\n"), "{md}");
}

#[test]
fn a_section_that_is_not_in_the_report_is_reported_not_silently_dropped() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (out, md) = weekly(
        &dir,
        &pd,
        json!({ "sections": [
            { "type": "metrics" },
            { "type": "section", "heading": "Nonexistent" },
            { "type": "ai_risks" },
        ] }),
    );
    assert!(!md.contains("Nonexistent"), "{md}");
    let warnings = out["section_warnings"].as_array().expect("warnings");
    assert_eq!(warnings.len(), 2, "{out}");
    let joined = warnings
        .iter()
        .map(|w| w.as_str().unwrap())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("Nonexistent"), "{joined}");
    assert!(
        joined.contains("Completed Tasks"),
        "lists available: {joined}"
    );
    assert!(joined.contains("ai_risks"), "{joined}");
}

#[test]
fn warnings_section_is_never_dropped_by_sections() {
    let (dir, pd) = setup();
    // A trace graph too large to draw puts a note in data.warnings.
    let (_, md) = weekly(
        &dir,
        &pd,
        json!({ "data": { "warnings": ["careful"] },
                "sections": [{ "type": "metrics" }] }),
    );
    assert!(md.contains("## Warnings\n\n- careful"), "{md}");
}

#[test]
fn invalid_sections_are_rejected() {
    let (_dir, pd) = setup();
    let base = |sections: Value| {
        json!({ "project_dir": pd, "action": "generate", "report_type": "weekly",
                "scope": { "period": "2026-W41" }, "sections": sections })
    };
    let msg = err(base(json!("metrics")));
    assert!(msg.contains("'sections' must be an array"), "{msg}");
    let msg = err(base(json!([])));
    assert!(msg.contains("must not be empty"), "{msg}");
    let msg = err(base(json!([{ "type": "metrics" }, { "type": "bogus" }])));
    assert!(msg.contains("sections[1]"), "{msg}");
    assert!(msg.contains("bogus"), "{msg}");
    let msg = err(base(json!([{ "type": "section" }])));
    assert!(msg.contains("sections[0]"), "{msg}");
    assert!(msg.contains("heading"), "{msg}");
    let msg = err(base(json!([{ "type": "custom" }])));
    assert!(msg.contains("sections[0]"), "{msg}");
    let msg = err(base(json!([{ "type": "metrics", "extra": 1 }])));
    assert!(msg.contains("sections[0]"), "{msg}");
}

#[test]
fn sections_only_apply_to_generate() {
    let (_dir, pd) = setup();
    let msg = err(json!({ "project_dir": pd, "action": "list",
                          "sections": [{ "type": "metrics" }] }));
    assert!(msg.contains("sections"), "{msg}");
    assert!(msg.contains("generate"), "{msg}");
}

// ---- custom templates (REQ-PDF-035) -------------------------------------------

#[test]
fn report_template_renders_with_a_project_template() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    write_template(
        &dir,
        "my-custom",
        "# Custom for {{report.report_type}}\n\n## Mine\n\nPeriod {{data.period.start}}\n\n## Summary\n\nDone {{data.stats.completed_total}}\n",
    );
    let (out, md) = weekly(&dir, &pd, json!({ "report_template": "my-custom" }));
    assert!(md.starts_with("# Custom for weekly\n"), "{md}");
    assert!(md.contains("Period 2026-10-05"), "{md}");
    assert!(!md.contains("## Time Log"), "{md}");
    assert_eq!(out["report"]["template"], "my-custom", "{out}");
    assert_eq!(out["report"]["report_type"], "weekly", "{out}");

    // Persisted metadata keeps the template name.
    let id = out["report"]["report_id"].as_str().unwrap();
    let got = ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "get", "report_id": id }),
    );
    assert_eq!(got["report"]["template"], "my-custom", "{got}");
}

#[test]
fn report_template_can_be_combined_with_sections() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    write_template(
        &dir,
        "my-custom",
        "# Custom\n\n## First\n\nA\n\n## Second\n\nB\n",
    );
    let (_, md) = weekly(
        &dir,
        &pd,
        json!({ "report_template": "my-custom",
                "sections": [
                    { "type": "section", "heading": "Second" },
                    { "type": "section", "heading": "First" },
                ] }),
    );
    assert!(md.starts_with("# Custom\n"), "{md}");
    assert!(pos(&md, "## Second") < pos(&md, "## First"), "{md}");
}

#[test]
fn a_project_template_named_like_a_builtin_overrides_it_by_default() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    write_template(&dir, "weekly", "OVERRIDDEN");
    let (_, md) = weekly(&dir, &pd, json!({}));
    assert_eq!(md, "OVERRIDDEN");
}

#[test]
fn builtin_template_can_be_named_explicitly() {
    let (dir, pd) = setup();
    seed_progress(&dir);
    let (out, md) = weekly(&dir, &pd, json!({ "report_template": "monthly" }));
    assert!(md.starts_with("# Monthly Report"), "{md}");
    assert_eq!(out["report"]["report_type"], "weekly", "{out}");
}

#[test]
fn unknown_report_template_is_a_clear_error_and_writes_nothing() {
    let (dir, pd) = setup();
    write_template(&dir, "present", "x");
    let msg = err(json!({ "project_dir": pd, "action": "generate",
        "report_type": "weekly", "scope": { "period": "2026-W41" },
        "report_template": "no-such" }));
    assert!(msg.contains("no-such"), "{msg}");
    assert!(msg.contains(".handoff/templates/no-such.md.hbs"), "{msg}");
    assert!(msg.contains("weekly"), "lists built-ins: {msg}");
    assert!(msg.contains("present"), "lists custom: {msg}");
    assert!(
        !handoff(&dir).join("reports").exists(),
        "report was written"
    );
}

#[test]
fn report_template_must_be_a_plain_name_and_only_applies_to_generate() {
    let (_dir, pd) = setup();
    for bad in ["../x", "a/b", ""] {
        let msg = err(json!({ "project_dir": pd, "action": "generate",
            "report_type": "weekly", "report_template": bad }));
        assert!(msg.contains("report_template"), "{bad}: {msg}");
    }
    let msg = err(json!({ "project_dir": pd, "action": "list", "report_template": "x" }));
    assert!(msg.contains("report_template"), "{msg}");
    assert!(msg.contains("generate"), "{msg}");
}

#[test]
fn templates_action_lists_builtin_and_project_templates() {
    let (dir, pd) = setup();
    write_template(&dir, "my-custom", "x");
    write_template(&dir, "weekly", "y");
    let out = ok(
        "handoff_report",
        json!({ "project_dir": pd, "action": "templates" }),
    );
    let names = |key: &str| -> Vec<String> {
        out[key]
            .as_array()
            .unwrap_or_else(|| panic!("{key} missing: {out}"))
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect()
    };
    let builtin = names("builtin");
    for t in [
        "verification",
        "weekly",
        "inspection",
        "effort",
        "monthly",
        "milestone",
        "defect",
        "completion",
    ] {
        assert!(builtin.contains(&t.to_string()), "{builtin:?}");
    }
    assert!(!builtin.contains(&"ai_sections".to_string()), "{builtin:?}");
    assert_eq!(names("partials"), vec!["ai_sections"], "{out}");
    let custom = out["custom"].as_array().unwrap();
    assert_eq!(custom.len(), 2, "{out}");
    let weekly = custom.iter().find(|c| c["name"] == "weekly").unwrap();
    assert_eq!(weekly["overrides_builtin"], true, "{out}");
    let mine = custom.iter().find(|c| c["name"] == "my-custom").unwrap();
    assert_eq!(mine["overrides_builtin"], false, "{out}");
    assert_eq!(mine["path"], "templates/my-custom.md.hbs", "{out}");
}

#[test]
fn custom_template_renders_to_a_pdf() {
    match detect(Tool::Typst) {
        ToolStatus::Found { .. } => {}
        other => {
            assert!(
                std::env::var_os("HANDOFF_REQUIRE_TYPST").is_none(),
                "HANDOFF_REQUIRE_TYPST is set but Typst is unusable: {other:?}"
            );
            eprintln!("SKIPPED: typst not available ({other:?})");
            return;
        }
    }
    let (dir, pd) = setup();
    seed_progress(&dir);
    write_template(
        &dir,
        "my-custom",
        "# Custom report\n\n## Overview\n\nDone tasks: {{data.stats.completed_total}}\n",
    );
    let (out, md) = weekly(
        &dir,
        &pd,
        json!({ "report_template": "my-custom", "format": "pdf",
                "sections": [{ "type": "section", "heading": "Overview" }] }),
    );
    assert!(md.contains("Done tasks: 1"), "{md}");
    let pdf = out["pdf"]["pdf_path"].as_str().expect("pdf_path");
    let bytes = std::fs::read(pdf).unwrap_or_else(|e| panic!("read {pdf}: {e}"));
    assert!(bytes.starts_with(b"%PDF-"), "{pdf} is not a PDF");
}

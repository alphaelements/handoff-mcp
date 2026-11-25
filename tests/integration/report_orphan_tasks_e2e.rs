//! The "Orphan Tasks" section of the periodic reports (REQ-TRC-010,
//! SPEC-TRC-009 / SPEC-TRC-010), through the real `handoff-mcp` binary: the
//! server over stdio JSON-RPC and the `report` CLI.
//!
//! The daily, weekly, monthly, quarterly and yearly reports end with the
//! count and the list (id, title, status) of the tasks the shared orphan
//! predicate reports, the same set as the `task_orphan` lint rule. With no
//! orphan task the section is absent altogether, and the other report types
//! never carry it.

use crate::support::onboarding_e2e as support;

use serde_json::{json, Value};
use support::{Sandbox, Server};

const REQUIREMENT_BODY: &str = "# Requirements\n\n## Section\n\n### REQ-001 The first requirement\n\n- priority: P1\n\nThe system does the first thing.\n\n受入基準:\n- AC1: it does\n";

const SECTION_HEADING: &str = "## Orphan Tasks";

/// The report types that carry the section.
const PERIODIC: [&str; 5] = ["daily", "weekly", "monthly", "quarterly", "yearly"];

fn project_with(layer_document: bool) -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    // No estimate rule: the fixtures create bare tasks in every status.
    server.ok(
        "handoff_update_config",
        json!({ "updates": { "settings.require_estimate_hours": false } }),
    );
    if layer_document {
        server.ok(
            "handoff_doc_save",
            json!({
                "slug": "req-report",
                "title": "Requirements",
                "doc_type": "spec",
                "layer": "requirement",
                "body": REQUIREMENT_BODY,
            }),
        );
    }
    (sandbox, server)
}

fn create(server: &mut Server, id: &str, title: &str, extra: Value) {
    let mut task = json!({ "id": id, "title": title });
    for (key, value) in extra.as_object().expect("object") {
        task[key] = value.clone();
    }
    server.ok("handoff_update_task", json!({ "task": task }));
}

fn linked() -> Value {
    json!({ "requirement_ids": ["REQ-001"] })
}

/// Orphans, in natural id order: `finished` (done), `leaf` (todo), `partial`
/// (todo), `partial.2` (in_progress), `t2` and `t10` (todo).
fn build_tasks(server: &mut Server) {
    create(server, "leaf", "An unlinked | leaf", json!({}));
    create(server, "traced", "A linked leaf", linked());
    create(
        server,
        "dropped",
        "A skipped leaf",
        json!({ "status": "skipped" }),
    );
    create(
        server,
        "exempt",
        "A no-trace leaf",
        json!({ "labels": ["no-trace"] }),
    );
    create(
        server,
        "finished",
        "A done leaf",
        json!({ "status": "done" }),
    );
    create(
        server,
        "covered",
        "A parent traced through its children",
        json!({}),
    );
    create(server, "covered.1", "linked child", linked());
    create(
        server,
        "covered.2",
        "no-trace child",
        json!({ "labels": ["no-trace"] }),
    );
    create(
        server,
        "partial",
        "A parent with one unlinked child",
        json!({}),
    );
    create(server, "partial.1", "linked child", linked());
    create(
        server,
        "partial.2",
        "unlinked child",
        json!({ "status": "in_progress" }),
    );
    create(server, "t10", "Tenth", json!({}));
    create(server, "t2", "Second", json!({}));
}

/// The section a report of the fixture project ends with.
const EXPECTED_SECTION: &str = "## Orphan Tasks\n\n\
6 task(s) have no requirement link. Link each one with `requirement_ids`, or label it `no-trace`.\n\n\
| ID | Title | Status |\n\
| --- | --- | --- |\n\
| finished | A done leaf | done |\n\
| leaf | An unlinked \\| leaf | todo |\n\
| partial | A parent with one unlinked child | todo |\n\
| partial.2 | unlinked child | in_progress |\n\
| t2 | Second | todo |\n\
| t10 | Tenth | todo |\n";

/// Generates a report and returns `(generate result, stored Markdown body)`.
fn generate(server: &mut Server, arguments: Value) -> (Value, String) {
    let mut arguments = arguments;
    arguments["action"] = json!("generate");
    let generated = server.call("handoff_report", arguments);
    let report_id = generated["report"]["report_id"]
        .as_str()
        .expect("report_id")
        .to_string();
    let got = server.call(
        "handoff_report",
        json!({ "action": "get", "report_id": report_id }),
    );
    let body = got["body"].as_str().expect("body").to_string();
    (generated, body)
}

fn body_of(server: &mut Server, report_type: &str) -> String {
    generate(server, json!({ "report_type": report_type })).1
}

// --- REQ-TRC-010 AC1 / AC2, SPEC-TRC-009 AC5 ---

#[test]
fn weekly_and_monthly_reports_end_with_the_orphan_count_and_list() {
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);

    for report_type in ["weekly", "monthly"] {
        let body = body_of(&mut server, report_type);
        assert!(
            body.ends_with(EXPECTED_SECTION),
            "{report_type} does not end with the section:\n{body}"
        );
        // The no-trace task, the skipped task and the parent traced through
        // its children are in neither the count nor the list.
        let section = &body[body.find(SECTION_HEADING).unwrap()..];
        for absent in ["exempt", "dropped", "covered", "traced"] {
            assert!(!section.contains(absent), "{report_type}: {absent} listed");
        }
        assert_eq!(
            body.matches(SECTION_HEADING).count(),
            1,
            "{report_type}: one section"
        );
    }
}

/// SPEC-TRC-010: the other periodic reports carry the same section.
#[test]
fn every_periodic_report_type_carries_the_section() {
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);
    for report_type in PERIODIC {
        let body = body_of(&mut server, report_type);
        assert!(
            body.ends_with(EXPECTED_SECTION),
            "{report_type} does not end with the section:\n{body}"
        );
    }
}

/// SPEC-TRC-002 AC3: the report lists exactly the tasks the lint reports.
#[test]
fn the_report_lists_exactly_the_tasks_the_lint_reports() {
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);

    let lint = server.call("handoff_trace_lint", json!({ "rules": ["task_orphan"] }));
    let lint_ids: Vec<String> = lint["findings"]
        .as_array()
        .expect("findings")
        .iter()
        .map(|f| f["task_id"].as_str().expect("task_id").to_string())
        .collect();
    assert_eq!(lint_ids.len(), 6, "{lint}");

    let body = body_of(&mut server, "weekly");
    let section = &body[body.find(SECTION_HEADING).expect("section")..];
    let report_ids: Vec<String> = section
        .lines()
        .filter(|l| l.starts_with("| ") && !l.starts_with("| ID") && !l.starts_with("| ---"))
        .map(|l| l.split('|').nth(1).unwrap().trim().to_string())
        .collect();
    assert_eq!(report_ids, lint_ids);
}

// --- AC3: no orphan, no section ---

#[test]
fn a_project_without_orphan_tasks_has_no_section_at_all() {
    let (_sandbox, mut server) = project_with(true);
    create(&mut server, "traced", "A linked leaf", linked());
    create(
        &mut server,
        "exempt",
        "A no-trace leaf",
        json!({ "labels": ["no-trace"] }),
    );
    create(
        &mut server,
        "dropped",
        "A skipped leaf",
        json!({ "status": "skipped" }),
    );

    for report_type in PERIODIC {
        let body = body_of(&mut server, report_type);
        assert!(!body.contains("Orphan"), "{report_type}:\n{body}");
        assert!(
            !body.contains("no requirement link"),
            "{report_type}:\n{body}"
        );
    }
}

/// Scope guard: without a layer document nothing is an orphan, so the
/// report of such a project is what it always was.
#[test]
fn a_project_without_a_layer_document_has_no_section() {
    let (_sandbox, mut server) = project_with(false);
    build_tasks(&mut server);
    for report_type in PERIODIC {
        let body = body_of(&mut server, report_type);
        assert!(!body.contains("Orphan"), "{report_type}:\n{body}");
    }
}

/// The section appears with the first orphan and goes away with the last.
#[test]
fn the_section_follows_the_current_state_of_the_tasks() {
    let (_sandbox, mut server) = project_with(true);
    create(&mut server, "a", "A", json!({}));
    let body = body_of(&mut server, "weekly");
    assert!(
        body.ends_with(
            "## Orphan Tasks\n\n1 task(s) have no requirement link. Link each one with \
             `requirement_ids`, or label it `no-trace`.\n\n| ID | Title | Status |\n| --- | --- | --- |\n\
             | a | A | todo |\n"
        ),
        "{body}"
    );

    server.ok(
        "handoff_update_task",
        json!({ "task": { "id": "a", "requirement_ids": ["REQ-001"] } }),
    );
    assert!(!body_of(&mut server, "weekly").contains("Orphan"));
}

// --- SPEC-TRC-010 AC3: the other report types ---

#[test]
fn the_other_report_types_never_carry_the_section() {
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);
    server.ok(
        "handoff_add_milestone",
        json!({ "name": "m1", "date": "2026-12-31" }),
    );

    for arguments in [
        json!({ "report_type": "effort" }),
        json!({ "report_type": "defect" }),
        json!({ "report_type": "milestone", "scope": { "milestone": "m1" } }),
        json!({ "report_type": "verification" }),
    ] {
        let (_, body) = generate(&mut server, arguments.clone());
        assert!(!body.contains("Orphan"), "{arguments}:\n{body}");
    }
}

// --- composition with the rest of `generate` ---

/// Like `charts` and `trace_diagram`: a key the caller passes in `data` is
/// left alone, so a caller can suppress the section or supply its own.
#[test]
fn an_orphan_tasks_key_passed_in_data_is_left_alone() {
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);

    let (_, suppressed) = generate(
        &mut server,
        json!({ "report_type": "weekly", "data": { "orphan_tasks": null } }),
    );
    assert!(!suppressed.contains("Orphan"), "{suppressed}");

    let (_, supplied) = generate(
        &mut server,
        json!({
            "report_type": "weekly",
            "data": { "orphan_tasks": {
                "count": 1,
                "tasks": [{ "id": "x1", "title": "Mine", "status": "review" }],
            } },
        }),
    );
    assert!(supplied.ends_with("| x1 | Mine | review |\n"), "{supplied}");
    assert!(!supplied.contains("| leaf |"), "{supplied}");
}

/// `sections` picks the section by its heading like any other one; a
/// composition that does not name it leaves it out.
#[test]
fn the_section_is_selectable_with_the_sections_argument() {
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);

    let (_, picked) = generate(
        &mut server,
        json!({
            "report_type": "weekly",
            "sections": [{ "type": "section", "heading": "Orphan Tasks" }],
        }),
    );
    assert!(picked.contains(EXPECTED_SECTION.trim_end()), "{picked}");
    assert!(!picked.contains("## Completed Tasks"), "{picked}");

    let (_, without) = generate(
        &mut server,
        json!({ "report_type": "weekly", "sections": [{ "type": "metrics" }] }),
    );
    assert!(!without.contains("Orphan"), "{without}");
}

/// The built-in partial is listed, so a project template can include it.
#[test]
fn the_template_catalog_lists_the_orphan_tasks_partial() {
    let (_sandbox, mut server) = project_with(true);
    let catalog = server.call("handoff_report", json!({ "action": "templates" }));
    let partials: Vec<&str> = catalog["partials"]
        .as_array()
        .expect("partials")
        .iter()
        .map(|p| p.as_str().unwrap())
        .collect();
    assert!(partials.contains(&"orphan_tasks"), "{catalog}");
}

/// A project template that overrides a periodic report keeps working, and
/// gets the section by including the partial.
#[test]
fn a_project_template_gets_the_section_by_including_the_partial() {
    let (sandbox, mut server) = project_with(true);
    build_tasks(&mut server);
    sandbox.write(
        ".handoff/templates/weekly.md.hbs",
        "# Mine\n\n## Body\n\nx\n{{> orphan_tasks}}\n",
    );
    let body = body_of(&mut server, "weekly");
    assert!(body.starts_with("# Mine\n\n## Body\n\nx\n"), "{body}");
    assert!(body.contains(EXPECTED_SECTION.trim_end()), "{body}");

    sandbox.write(
        ".handoff/templates/weekly.md.hbs",
        "# Mine\n\nNo partial.\n",
    );
    assert_eq!(body_of(&mut server, "weekly"), "# Mine\n\nNo partial.\n");
}

// --- CLI ---

#[test]
fn the_report_cli_generates_the_same_section() {
    let (sandbox, mut server) = project_with(true);
    build_tasks(&mut server);
    drop(server);

    let generated = sandbox.cli(&["report", "generate", "--report-type", "monthly"]);
    assert_eq!(
        generated.code, 0,
        "{}{}",
        generated.stdout, generated.stderr
    );
    let report_id = generated.json()["report"]["report_id"]
        .as_str()
        .expect("report_id")
        .to_string();
    let got = sandbox.cli(&["report", "get", "--report-id", &report_id]);
    assert_eq!(got.code, 0, "{}{}", got.stdout, got.stderr);
    let body = got.json()["body"].as_str().expect("body").to_string();
    assert!(body.ends_with(EXPECTED_SECTION), "{body}");
}

/// The orphan check only reads: the tasks and the layer document are the
/// same after a report was generated.
#[test]
fn the_orphan_check_leaves_tasks_and_the_layer_document_untouched() {
    let (sandbox, mut server) = project_with(true);
    build_tasks(&mut server);
    let layer_document = sandbox.handoff().join("docs/_doc.req-report.md");
    let before_tasks = support::snapshot(&sandbox.handoff().join("tasks"));
    let before_document = std::fs::read(&layer_document).expect("layer document");

    assert!(body_of(&mut server, "weekly").contains(SECTION_HEADING));

    assert_eq!(
        support::snapshot(&sandbox.handoff().join("tasks")),
        before_tasks
    );
    assert_eq!(std::fs::read(&layer_document).unwrap(), before_document);
}

// --- documentation contract (SPEC-TRC-010 AC1) ---

/// The tool description, the README and the report reference each say which
/// report types carry the section, name it, and say when it is absent.
#[test]
fn the_tool_description_the_readme_and_the_reference_document_the_section() {
    let (_sandbox, mut server) = project_with(false);
    let definition = server.tool_definition("handoff_report");
    let description = definition["description"]
        .as_str()
        .expect("description")
        .to_string();

    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let read = |path: &str| {
        std::fs::read_to_string(root.join(path)).unwrap_or_else(|e| panic!("{path}: {e}"))
    };
    for (name, text) in [
        ("handoff_report description", description),
        ("README.md", read("README.md")),
        (
            "skills/handoff-trace/references/tools.md",
            read("skills/handoff-trace/references/tools.md"),
        ),
    ] {
        let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
        for needle in ["Orphan Tasks", "orphan_tasks", "task_orphan", "no-trace"] {
            assert!(flat.contains(needle), "{name} lacks {needle:?}");
        }
        // The five types that carry the section, and the six that do not.
        for report_type in PERIODIC {
            assert!(flat.contains(report_type), "{name} lacks {report_type:?}");
        }
        for other in [
            "verification",
            "inspection",
            "effort",
            "milestone",
            "defect",
            "completion",
        ] {
            assert!(flat.contains(other), "{name} lacks {other:?}");
        }
    }
}

// --- PDF (SPEC-TRC-009: the same section in the PDF) ---

/// Like the other PDF tests: without the Typst CLI the test prints `SKIPPED`
/// and passes, unless `HANDOFF_REQUIRE_TYPST` is set.
fn typst_available() -> bool {
    use handoff_mcp::storage::docs::pdf::deps::{detect, Tool, ToolStatus};
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

/// The PDF is rendered from the stored Markdown, so it carries the section
/// with the same count and rows.
#[test]
fn the_pdf_of_a_report_carries_the_same_section() {
    if !typst_available() {
        return;
    }
    let (_sandbox, mut server) = project_with(true);
    build_tasks(&mut server);

    let generated = server.call(
        "handoff_report",
        json!({
            "action": "generate",
            "report_type": "weekly",
            "format": "pdf",
            // No chart: the test must not depend on a Mermaid renderer.
            "include_charts": false,
        }),
    );
    let pdf_path = generated["pdf"]["pdf_path"].as_str().expect("pdf_path");
    let pdf = std::fs::read(pdf_path).expect("the PDF");
    assert!(pdf.starts_with(b"%PDF-"), "not a PDF: {pdf_path}");

    let typ_path = generated["pdf"]["typ_path"].as_str().expect("typ_path");
    let typ = std::fs::read_to_string(typ_path).expect("the Typst source");
    let section = &typ[typ.find("== Orphan Tasks").expect("the section heading")..];
    assert!(
        section.contains("6 task(s) have no requirement link."),
        "{section}"
    );
    assert!(
        section.contains("table.header([ID], [Title], [Status])"),
        "{section}"
    );
    for row in [
        "[finished], [A done leaf], [done]",
        "[partial.2], [unlinked child], [in\\_progress]",
        "[t10], [Tenth], [todo]",
    ] {
        assert!(section.contains(row), "missing row {row}:\n{section}");
    }
    assert!(!section.contains("[exempt]"), "{section}");
}

// --- a layer document that cannot be read ---

/// When the only layer document cannot be read, the report cannot tell
/// whether the project uses requirement traceability: it is still generated,
/// and says so in its Warnings section instead of passing for "no orphan".
#[test]
fn an_unreadable_layer_document_is_reported_in_the_warnings_section() {
    let (sandbox, mut server) = project_with(true);
    build_tasks(&mut server);
    let path = sandbox.handoff().join("docs/_doc.req-report.md");
    let text = std::fs::read_to_string(&path).expect("the layer document");
    std::fs::write(&path, text.replacen("---\n", "---\n: : broken [\n", 1)).unwrap();

    let body = body_of(&mut server, "weekly");
    assert!(!body.contains(SECTION_HEADING), "{body}");
    let warnings = &body[body.find("## Warnings").expect("a Warnings section")..];
    assert!(
        warnings.contains("- Orphan tasks could not be listed: "),
        "{body}"
    );
    assert!(warnings.contains("req-report"), "{body}");
}

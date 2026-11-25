//! REQ-PDF-032: `handoff_report generate` renders `data.ai_*` keys as AI
//! sections (summary, analysis, risks, recommendations) at the top of the
//! report; reports without those keys are unchanged.

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

fn ok(name: &str, arguments: Value) -> Value {
    let resp = call(name, arguments.clone());
    let text = resp["result"]["content"][0]["text"].as_str().unwrap_or("");
    assert!(
        !resp["result"]["isError"].as_bool().unwrap_or(false),
        "{name} {arguments} failed: {text}"
    );
    serde_json::from_str(text).unwrap_or(Value::Null)
}

fn generate(report_type: &str, scope: Value, data: Option<Value>) -> String {
    let dir: TempDir = tempfile::tempdir().unwrap();
    let pd = dir.path().to_string_lossy().to_string();
    ok(
        "handoff_init",
        json!({ "project_dir": pd, "project_name": "ai" }),
    );
    let mut args = json!({ "project_dir": pd, "action": "generate",
                           "report_type": report_type, "scope": scope });
    if let Some(data) = data {
        args["data"] = data;
    }
    let out = ok("handoff_report", args);
    let path = out["report"]["output_path"].as_str().unwrap();
    std::fs::read_to_string(dir.path().join(".handoff").join(path)).unwrap()
}

#[test]
fn weekly_report_has_all_ai_sections_in_order_before_the_body() {
    let md = generate(
        "weekly",
        json!({ "period": "2026-W41" }),
        Some(json!({
            "ai_recommendations": "Ship it.",
            "ai_risks": "- Schedule slip",
            "ai_analysis": "Velocity is flat.",
            "ai_summary": "Week went well.",
        })),
    );
    let pos = |needle: &str| {
        md.find(needle)
            .unwrap_or_else(|| panic!("missing {needle:?}:\n{md}"))
    };
    let order = [
        pos("## AI Summary\n\nWeek went well.\n"),
        pos("## AI Analysis\n\nVelocity is flat.\n"),
        pos("## AI Risks\n\n- Schedule slip\n"),
        pos("## AI Recommendations\n\nShip it.\n"),
        pos("## Summary\n"),
    ];
    assert!(order.windows(2).all(|w| w[0] < w[1]), "{md}");
    assert!(md.starts_with("# Weekly Report"), "{md}");
}

#[test]
fn only_the_ai_keys_passed_are_rendered() {
    let md = generate(
        "weekly",
        json!({ "period": "2026-W41" }),
        Some(json!({ "ai_risks": "Only risks." })),
    );
    assert!(md.contains("## AI Risks\n\nOnly risks.\n"), "{md}");
    for absent in ["## AI Summary", "## AI Analysis", "## AI Recommendations"] {
        assert!(!md.contains(absent), "{absent} shown:\n{md}");
    }
}

#[test]
fn no_ai_keys_means_no_ai_sections() {
    for data in [None, Some(json!({ "summary": "plain" }))] {
        let md = generate("weekly", json!({ "period": "2026-W41" }), data);
        assert!(!md.contains("## AI "), "{md}");
    }
}

#[test]
fn ai_sections_work_for_other_report_types() {
    for (report_type, scope) in [
        ("monthly", json!({ "period": "2026-10" })),
        ("effort", json!({})),
        ("defect", json!({})),
    ] {
        let md = generate(report_type, scope, Some(json!({ "ai_summary": "S-TEXT" })));
        assert!(
            md.contains("## AI Summary\n\nS-TEXT\n"),
            "{report_type}:\n{md}"
        );
    }
}

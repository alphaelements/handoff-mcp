//! Review Board auto-sync (FR-RB-001, FR-RB-002) through the real
//! `handoff-mcp` binary over stdio JSON-RPC: `handoff_update_task` moving a
//! task into `review` adds a board item, moving it out resolves it.

#[path = "support/onboarding_e2e.rs"]
mod support;

use serde_json::json;
use support::{Sandbox, Server};

fn project() -> (Sandbox, Server) {
    let sandbox = Sandbox::initialized("");
    let mut server = sandbox.server();
    server.ok(
        "handoff_update_config",
        json!({ "updates": { "settings.require_estimate_hours": false } }),
    );
    (sandbox, server)
}

fn task(server: &mut Server, patch: serde_json::Value) -> String {
    server.ok(
        "handoff_update_task",
        json!({ "task": patch, "force": true }),
    )
}

fn board(sandbox: &Sandbox) -> String {
    std::fs::read_to_string(sandbox.handoff().join("review_board.md")).unwrap_or_default()
}

fn decisions(sandbox: &Sandbox) -> String {
    std::fs::read_to_string(sandbox.handoff().join("review_board_decisions.md")).unwrap_or_default()
}

fn create_task(server: &mut Server) {
    task(
        server,
        json!({
            "id": "t1",
            "title": "Ship the thing",
            "status": "in_progress",
            "done_criteria": [
                { "item": "first check", "checked": false },
                { "item": "second check", "checked": false },
                { "item": "already verified", "checked": true },
            ],
        }),
    );
}

#[test]
fn review_creates_the_board_with_one_item_carrying_task_id_and_criteria() {
    let (sandbox, mut server) = project();
    create_task(&mut server);
    assert!(!sandbox.handoff().join("review_board.md").exists());

    let reply = task(&mut server, json!({ "id": "t1", "status": "review" }));
    assert!(
        !reply.contains("review_board"),
        "success is silent: {reply}"
    );

    let text = board(&sandbox);
    assert!(text.contains("## Task Review"), "{text}");
    assert!(
        text.contains("### Ship the thing <!-- rb-id: R-001 -->"),
        "{text}"
    );
    assert!(text.contains("- task_id: t1"));
    assert!(text.contains("- first check") && text.contains("- second check"));
    assert!(!text.contains("already verified"), "{text}");

    // The tools the user and the VSCode extension use see the same item.
    let list = server.call("handoff_review_list", json!({}));
    assert_eq!(list["items"][0]["id"], "R-001", "{list}");
    assert_eq!(list["items"][0]["category"], "Task Review", "{list}");
}

#[test]
fn repeated_review_updates_never_duplicate_the_item() {
    let (sandbox, mut server) = project();
    create_task(&mut server);
    task(&mut server, json!({ "id": "t1", "status": "review" }));
    task(&mut server, json!({ "id": "t1", "status": "review" }));
    task(
        &mut server,
        json!({ "id": "t1", "title": "Renamed", "status": "review" }),
    );
    assert_eq!(board(&sandbox).matches("task_id: t1").count(), 1);
}

#[test]
fn review_to_done_resolves_as_approved() {
    let (sandbox, mut server) = project();
    create_task(&mut server);
    task(&mut server, json!({ "id": "t1", "status": "review" }));
    let all_checked = json!([
        { "item": "first check", "checked": true },
        { "item": "second check", "checked": true },
    ]);
    let reply = task(
        &mut server,
        json!({ "id": "t1", "status": "done", "done_criteria": all_checked }),
    );
    assert!(
        !reply.contains("review_board"),
        "success is silent: {reply}"
    );
    assert!(!board(&sandbox).contains("task_id: t1"));
    let dec = decisions(&sandbox);
    assert!(
        dec.contains("original_id: R-001") && dec.contains("resolution: approved"),
        "{dec}"
    );
}

#[test]
fn review_back_to_in_progress_resolves_as_rework_and_a_new_review_adds_a_new_item() {
    let (sandbox, mut server) = project();
    create_task(&mut server);
    task(&mut server, json!({ "id": "t1", "status": "review" }));
    task(&mut server, json!({ "id": "t1", "status": "in_progress" }));
    assert!(decisions(&sandbox).contains("resolution: rework"));
    assert!(!board(&sandbox).contains("task_id: t1"));

    task(&mut server, json!({ "id": "t1", "status": "review" }));
    assert!(board(&sandbox).contains("### Ship the thing <!-- rb-id: R-002 -->"));
}

#[test]
fn a_manual_resolve_is_kept_when_the_task_leaves_review() {
    let (sandbox, mut server) = project();
    create_task(&mut server);
    task(&mut server, json!({ "id": "t1", "status": "review" }));
    server.ok(
        "handoff_review_resolve",
        json!({ "id": "R-001", "decision": "Manual verdict: looks fine" }),
    );
    let before = decisions(&sandbox);
    let reply = task(&mut server, json!({ "id": "t1", "status": "in_progress" }));
    assert!(!reply.contains("review_board"), "{reply}");
    assert_eq!(decisions(&sandbox), before);
    assert!(!before.contains("resolution: rework"));
}

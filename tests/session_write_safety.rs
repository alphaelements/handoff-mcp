//! Storage-level tests for the session write paths (t442-A): a write re-checks
//! the owner under the sessions lock, a foreign overwrite merges instead of
//! replacing, auto-parenting never picks another agent's primary session, and
//! unreadable closed files do not block lineage reads.

use std::path::Path;
use std::sync::{Arc, Barrier};

use serde_json::json;

use handoff_mcp::storage::session_target::OWNER_V;
use handoff_mcp::storage::sessions::{
    adopt_session, find_active_primary_session_id, read_active_sessions,
    read_recent_closed_sessions, read_session_entries, transition_session_checked,
    update_session_at, write_session_with_status, OwnerStamp, SeenSession, SessionData,
    SessionWriteError, WriteMode,
};

const ID: &str = "s-20260101-000001-000001";

fn owned(id: &str, owner: Option<&str>, summary: &str) -> SessionData {
    SessionData {
        version: 2,
        id: Some(id.to_string()),
        ended_at: Some("2026-01-01T00:00:01+00:00".to_string()),
        summary: summary.to_string(),
        agent_id: owner.map(String::from),
        owner_v: owner.map(|_| OWNER_V),
        worktree: Some("/work/main".to_string()),
        ..Default::default()
    }
}

fn setup() -> (tempfile::TempDir, std::path::PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let sessions_dir = dir.path().join(".handoff/sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    (dir, sessions_dir)
}

fn seen(id: &str, owner: Option<&str>) -> SeenSession {
    SeenSession {
        id: id.to_string(),
        owner: owner.map(String::from),
    }
}

fn updates(summary: &str) -> SessionData {
    SessionData {
        version: 2,
        summary: summary.to_string(),
        ended_at: Some("2026-02-02T00:00:00+00:00".to_string()),
        ..Default::default()
    }
}

fn adopt(sessions_dir: &Path, expected: &str, new_owner: &str) -> anyhow::Result<()> {
    adopt_session(
        sessions_dir,
        ID,
        Some(expected),
        &OwnerStamp {
            agent_id: new_owner,
            worktree: "/new/wt",
            scope: "primary",
        },
        "active",
    )
    .map(|_| ())
}

fn file_count(sessions_dir: &Path, suffix: &str) -> usize {
    std::fs::read_dir(sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(suffix))
        .count()
}

// ---- stale read: the owner is re-checked at write time -------------------

#[test]
fn close_by_the_old_owner_after_an_adopt_is_refused_and_changes_nothing() {
    let (_dir, sessions_dir) = setup();
    write_session_with_status(
        &sessions_dir,
        &owned(ID, Some("agent-old"), "old work"),
        "active",
    )
    .unwrap();
    adopt(&sessions_dir, "agent-old", "agent-new").unwrap();

    let err = update_session_at(
        &sessions_dir,
        &seen(ID, Some("agent-old")),
        &updates("old owner closes"),
        None,
        Some("closed"),
        WriteMode::Replace,
    )
    .unwrap_err();

    match err.downcast_ref::<SessionWriteError>() {
        Some(SessionWriteError::OwnerChanged { id, owner }) => {
            assert_eq!(id, ID);
            assert_eq!(owner.as_deref(), Some("agent-new"));
        }
        other => panic!("expected OwnerChanged, got {other:?} ({err})"),
    }
    let active = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(active.len(), 1, "the adopted session is still active");
    assert_eq!(active[0].summary, "old work", "nothing was written");
    assert_eq!(active[0].agent_id.as_deref(), Some("agent-new"));
    assert_eq!(file_count(&sessions_dir, ".closed.json"), 0);
}

#[test]
fn transition_by_the_old_owner_after_an_adopt_is_refused() {
    let (_dir, sessions_dir) = setup();
    write_session_with_status(&sessions_dir, &owned(ID, Some("agent-old"), "w"), "active").unwrap();
    adopt(&sessions_dir, "agent-old", "agent-new").unwrap();

    let err = transition_session_checked(&sessions_dir, &seen(ID, Some("agent-old")), "paused")
        .unwrap_err();

    assert!(matches!(
        err.downcast_ref::<SessionWriteError>(),
        Some(SessionWriteError::OwnerChanged { .. })
    ));
    assert_eq!(file_count(&sessions_dir, ".active.json"), 1);
    assert_eq!(file_count(&sessions_dir, ".paused.json"), 0);
}

#[test]
fn write_to_a_session_that_has_gone_reports_gone() {
    let (_dir, sessions_dir) = setup();
    let err = update_session_at(
        &sessions_dir,
        &seen(ID, None),
        &updates("x"),
        None,
        None,
        WriteMode::Replace,
    )
    .unwrap_err();
    assert!(matches!(
        err.downcast_ref::<SessionWriteError>(),
        Some(SessionWriteError::Gone { .. })
    ));
}

#[test]
fn ownerless_session_write_with_matching_expectation_succeeds_and_follows_the_rename() {
    let (_dir, sessions_dir) = setup();
    write_session_with_status(&sessions_dir, &owned(ID, None, "legacy"), "active").unwrap();
    let path = update_session_at(
        &sessions_dir,
        &seen(ID, None),
        &updates("closed now"),
        None,
        Some("closed"),
        WriteMode::Replace,
    )
    .unwrap();
    assert!(path.to_string_lossy().ends_with(".closed.json"));
    assert_eq!(file_count(&sessions_dir, ".active.json"), 0);
}

/// The reproduced race: A decided to close what it saw as its own session while
/// B adopted it. Whatever the interleaving, a session B adopted must never end
/// up closed by A.
#[test]
fn adopt_racing_with_the_old_owners_close_never_loses_the_adopted_session() {
    for round in 0..60 {
        let (_dir, sessions_dir) = setup();
        write_session_with_status(&sessions_dir, &owned(ID, Some("agent-old"), "w"), "active")
            .unwrap();
        let barrier = Arc::new(Barrier::new(2));

        let closer = {
            let (barrier, sessions_dir) = (barrier.clone(), sessions_dir.clone());
            std::thread::spawn(move || {
                barrier.wait();
                update_session_at(
                    &sessions_dir,
                    &seen(ID, Some("agent-old")),
                    &updates("closed by old owner"),
                    None,
                    Some("closed"),
                    WriteMode::Replace,
                )
                .is_ok()
            })
        };
        let adopter = {
            let (barrier, sessions_dir) = (barrier.clone(), sessions_dir.clone());
            std::thread::spawn(move || {
                barrier.wait();
                adopt(&sessions_dir, "agent-old", "agent-new").is_ok()
            })
        };
        let closed = closer.join().unwrap();
        let adopted = adopter.join().unwrap();

        assert!(
            closed ^ adopted,
            "round {round}: exactly one side wins (closed={closed}, adopted={adopted})"
        );
        if adopted {
            let active = read_active_sessions(&sessions_dir).unwrap();
            assert_eq!(
                active.len(),
                1,
                "round {round}: adopted session stays active"
            );
            assert_eq!(active[0].agent_id.as_deref(), Some("agent-new"));
            assert_eq!(file_count(&sessions_dir, ".closed.json"), 0);
        } else {
            assert_eq!(file_count(&sessions_dir, ".closed.json"), 1);
        }
    }
}

// ---- a foreign overwrite merges, it does not replace ---------------------

#[test]
fn foreign_overwrite_appends_lists_and_keeps_the_owners_summary() {
    let (_dir, sessions_dir) = setup();
    let mut a = owned(ID, Some("agent-a"), "A's summary");
    a.decisions = vec![json!({"decision": "A1"}), json!({"decision": "A2"})];
    a.checklist = vec![json!({"item": "step one", "checked": true, "owner": "ai"})];
    a.handoff_notes = vec![json!({"note": "A note"})];
    a.context_pointers = vec![json!({"path": "src/a.rs"})];
    a.blockers = vec!["A blocked".to_string()];
    write_session_with_status(&sessions_dir, &a, "active").unwrap();

    let mut b = updates("B overwrites");
    b.decisions = vec![json!({"decision": "B1"}), json!({"decision": "A1"})];
    b.checklist = vec![
        json!({"item": "step one", "checked": false, "owner": "ai"}),
        json!({"item": "step two", "checked": false, "owner": "ai"}),
    ];
    b.handoff_notes = vec![json!({"note": "B note"})];
    b.context_pointers = vec![json!({"path": "src/b.rs"})];
    b.blockers = vec!["B blocked".to_string()];
    let provided = json!({
        "decisions": [], "checklist": [], "handoff_notes": [],
        "context_pointers": [], "blockers": []
    });

    update_session_at(
        &sessions_dir,
        &seen(ID, Some("agent-a")),
        &b,
        Some(&provided),
        None,
        WriteMode::MergeForeign,
    )
    .unwrap();

    let got = &read_session_entries(&sessions_dir).unwrap()[0].data;
    assert_eq!(got.summary, "A's summary", "the owner's summary is kept");
    assert_eq!(got.agent_id.as_deref(), Some("agent-a"));
    let decisions: Vec<&str> = got
        .decisions
        .iter()
        .filter_map(|d| d["decision"].as_str())
        .collect();
    assert_eq!(decisions, vec!["A1", "A2", "B1"], "appended, no duplicate");
    assert_eq!(got.checklist.len(), 2, "same item text is not duplicated");
    assert_eq!(
        got.checklist[0]["checked"], true,
        "the owner's checked state wins"
    );
    assert_eq!(got.checklist[1]["item"], "step two");
    assert_eq!(got.handoff_notes.len(), 2);
    assert_eq!(got.context_pointers.len(), 2);
    assert_eq!(got.blockers, vec!["A blocked", "B blocked"]);
}

#[test]
fn replace_mode_still_replaces_provided_fields_of_an_own_session() {
    let (_dir, sessions_dir) = setup();
    let mut a = owned(ID, Some("agent-a"), "v1");
    a.decisions = vec![json!({"decision": "old"})];
    write_session_with_status(&sessions_dir, &a, "active").unwrap();
    let mut u = updates("v2");
    u.decisions = vec![json!({"decision": "new"})];

    update_session_at(
        &sessions_dir,
        &seen(ID, Some("agent-a")),
        &u,
        Some(&json!({"decisions": []})),
        None,
        WriteMode::Replace,
    )
    .unwrap();

    let got = &read_session_entries(&sessions_dir).unwrap()[0].data;
    assert_eq!(got.summary, "v2");
    assert_eq!(got.decisions, vec![json!({"decision": "new"})]);
}

// ---- auto-parenting never picks another agent's primary ------------------

fn primary(id: &str, owner: Option<&str>) -> SessionData {
    let mut s = owned(id, owner, &format!("primary work {id}"));
    s.scope = Some("primary".to_string());
    s
}

#[test]
fn primary_lookup_prefers_the_callers_own_then_ownerless_never_another_agents() {
    let (_dir, sessions_dir) = setup();
    write_session_with_status(
        &sessions_dir,
        &primary("s-20260101-000001-aaaaaa", Some("agent-a")),
        "active",
    )
    .unwrap();
    write_session_with_status(
        &sessions_dir,
        &primary("s-20260101-000002-bbbbbb", Some("agent-b")),
        "active",
    )
    .unwrap();

    assert_eq!(
        find_active_primary_session_id(&sessions_dir, Some("agent-a")).unwrap(),
        Some("s-20260101-000001-aaaaaa".to_string()),
        "own primary beats a newer one owned by someone else"
    );
    assert_eq!(
        find_active_primary_session_id(&sessions_dir, Some("agent-c")).unwrap(),
        None,
        "another agent's primary is never used"
    );
    assert_eq!(
        find_active_primary_session_id(&sessions_dir, None).unwrap(),
        None
    );

    write_session_with_status(
        &sessions_dir,
        &primary("s-20260101-000003-cccccc", None),
        "active",
    )
    .unwrap();
    assert_eq!(
        find_active_primary_session_id(&sessions_dir, Some("agent-c")).unwrap(),
        Some("s-20260101-000003-cccccc".to_string()),
        "an ownerless primary is the fallback"
    );
    assert_eq!(
        find_active_primary_session_id(&sessions_dir, Some("agent-a")).unwrap(),
        Some("s-20260101-000001-aaaaaa".to_string()),
        "own still wins over ownerless"
    );
}

// ---- unreadable closed files are skipped with a warning ------------------

#[test]
fn recent_closed_reader_skips_an_unparseable_file_and_reports_it() {
    let (_dir, sessions_dir) = setup();
    let mut ok = owned("s-20260101-000001-000001", Some("agent-a"), "fine");
    ok.ended_at = Some("2026-01-01T00:00:01+00:00".to_string());
    write_session_with_status(&sessions_dir, &ok, "closed").unwrap();
    std::fs::write(
        sessions_dir.join("20260102-000000-broken.closed.json"),
        "<<<<<<< HEAD\n{\"version\": 2}\n=======\n{}\n>>>>>>> branch\n",
    )
    .unwrap();

    let read = read_recent_closed_sessions(&sessions_dir, 10).unwrap();

    assert_eq!(read.sessions.len(), 1);
    assert_eq!(read.sessions[0].summary, "fine");
    assert_eq!(read.skipped.len(), 1);
    assert_eq!(read.skipped[0].file, "20260102-000000-broken.closed.json");
    assert!(!read.skipped[0].reason.is_empty());
}

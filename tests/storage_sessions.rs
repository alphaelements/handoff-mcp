use handoff_mcp::storage::sessions::*;
use std::fs;
use tempfile::TempDir;

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn make_session(summary: &str, ended_at: &str) -> SessionData {
    SessionData {
        version: 2,
        id: None,
        ended_at: Some(ended_at.to_string()),
        summary: summary.to_string(),
        branch: Some("main".to_string()),
        commit: Some("abc1234".to_string()),
        dirty_files: Vec::new(),
        decisions: Vec::new(),
        blockers: Vec::new(),
        checklist: Vec::new(),
        handoff_notes: Vec::new(),
        references: Vec::new(),
        context_pointers: Vec::new(),
        environment: None,
        timeline: None,
        label: None,
        parent_session_id: None,
        related_task_ids: Vec::new(),
    }
}

#[test]
fn write_open_session_creates_file() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let data = make_session("test session", "2026-06-13T14:30:00Z");
    let path = write_open_session(&sessions_dir, &data).unwrap();

    assert!(path.exists());
    assert!(
        path.to_string_lossy().ends_with(".open.json"),
        "filename: {}",
        path.display()
    );

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["summary"], "test session");
    assert_eq!(parsed["version"], 2);
}

#[test]
fn read_open_sessions_empty() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let sessions = read_open_sessions(&sessions_dir).unwrap();
    assert!(sessions.is_empty());
}

#[test]
fn read_open_sessions_returns_open_only() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("open one", "2026-06-13T10:00:00Z");
    write_open_session(&sessions_dir, &s1).unwrap();

    let s2 = make_session("open two", "2026-06-13T11:00:00Z");
    write_open_session(&sessions_dir, &s2).unwrap();

    fs::write(
        sessions_dir.join("20260612-090000-old.closed.json"),
        r#"{"version":2,"summary":"old closed","branch":"main","commit":"111"}"#,
    )
    .unwrap();

    let sessions = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(sessions.len(), 2);
}

#[test]
fn activate_open_sessions_renames_to_active() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("session one", "2026-06-13T10:00:00Z");
    let open_path = write_open_session(&sessions_dir, &s1).unwrap();

    let activated = activate_open_sessions(&sessions_dir).unwrap();
    assert_eq!(activated.len(), 1);
    assert!(!open_path.exists());
    assert!(activated[0].exists());
    assert!(activated[0].to_string_lossy().contains(".active.json"));
}

#[test]
fn close_active_sessions_renames_to_closed() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("session one", "2026-06-13T10:00:00Z");
    write_open_session(&sessions_dir, &s1).unwrap();
    let activated = activate_open_sessions(&sessions_dir).unwrap();
    let active_path = &activated[0];

    let closed = close_active_sessions(&sessions_dir).unwrap();
    assert_eq!(closed.len(), 1);
    assert!(!active_path.exists());
    assert!(closed[0].exists());
    assert!(closed[0].to_string_lossy().contains(".closed.json"));
}

#[test]
fn close_open_sessions_renames_to_closed() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("session one", "2026-06-13T10:00:00Z");
    let open_path = write_open_session(&sessions_dir, &s1).unwrap();

    let closed = close_open_sessions(&sessions_dir).unwrap();
    assert_eq!(closed.len(), 1);
    assert!(!open_path.exists());
    assert!(closed[0].exists());
    assert!(closed[0].to_string_lossy().contains(".closed.json"));
}

#[test]
fn full_session_lifecycle_open_active_closed() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("first session", "2026-06-13T10:00:00Z");
    write_open_session(&sessions_dir, &s1).unwrap();

    let open = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].summary, "first session");

    activate_open_sessions(&sessions_dir).unwrap();

    assert!(read_open_sessions(&sessions_dir).unwrap().is_empty());
    let active = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].summary, "first session");

    close_active_sessions(&sessions_dir).unwrap();

    assert!(read_active_sessions(&sessions_dir).unwrap().is_empty());

    let s2 = make_session("second session", "2026-06-13T14:00:00Z");
    write_open_session(&sessions_dir, &s2).unwrap();

    let open2 = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(open2.len(), 1);
    assert_eq!(open2[0].summary, "second session");
}

#[test]
fn enforce_history_limit_removes_oldest() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    for i in 1..=5 {
        let name = format!("20260610-{i:06}-s{i}.closed.json");
        fs::write(
            sessions_dir.join(&name),
            format!(r#"{{"version":2,"summary":"session {i}"}}"#),
        )
        .unwrap();
    }

    let removed = enforce_history_limit(&sessions_dir, 3).unwrap();
    assert_eq!(removed, 2);

    let remaining: Vec<_> = fs::read_dir(&sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().ends_with(".closed.json"))
        .collect();
    assert_eq!(remaining.len(), 3);
}

#[test]
fn enforce_history_limit_ignores_open_and_active() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    for i in 1..=3 {
        let name = format!("20260610-{i:06}-s{i}.closed.json");
        fs::write(
            sessions_dir.join(&name),
            format!(r#"{{"version":2,"summary":"closed {i}"}}"#),
        )
        .unwrap();
    }

    let s = make_session("open session", "2026-06-13T10:00:00Z");
    write_open_session(&sessions_dir, &s).unwrap();

    enforce_history_limit(&sessions_dir, 2).unwrap();

    let open = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(open.len(), 1);
}

#[test]
fn enforce_history_limit_under_limit_removes_nothing() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    fs::write(
        sessions_dir.join("20260610-000001-s1.closed.json"),
        r#"{"version":2,"summary":"s1"}"#,
    )
    .unwrap();

    let removed = enforce_history_limit(&sessions_dir, 5).unwrap();
    assert_eq!(removed, 0);
}

#[test]
fn generate_session_filename_format() {
    let name = generate_session_filename("Pattern run fix", "20260613-143000");
    assert!(name.starts_with("20260613-143000-"));
    assert!(name.contains("pattern-run-fix"));
}

#[test]
fn read_open_sessions_nonexistent_dir() {
    let dir = setup();
    let sessions_dir = dir.path().join("nonexistent");
    let sessions = read_open_sessions(&sessions_dir).unwrap();
    assert!(sessions.is_empty());
}

#[test]
fn generate_session_id_format() {
    let id = generate_session_id();
    assert!(id.starts_with("s-"), "should start with s-: {id}");
    assert!(id.len() > 20, "should be long enough: {id}");
    let parts: Vec<&str> = id.splitn(2, '-').collect();
    assert_eq!(parts[0], "s");
}

#[test]
fn write_open_session_assigns_id() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s = make_session("test", "2026-06-15T10:00:00Z");
    assert!(s.id.is_none());

    write_open_session(&sessions_dir, &s).unwrap();

    let sessions = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(
        sessions[0].id.is_some(),
        "written session should have an id"
    );
    assert!(sessions[0].id.as_ref().unwrap().starts_with("s-"));
}

#[test]
fn read_old_session_without_id_gets_synthesized_id() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    // Write a session file without id field (simulating pre-upgrade file)
    fs::write(
        sessions_dir.join("20260613-143000-old-session.open.json"),
        r#"{"version":2,"summary":"old session"}"#,
    )
    .unwrap();

    let sessions = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(sessions.len(), 1);
    assert!(
        sessions[0].id.is_some(),
        "old session should get a synthesized id"
    );
    let id = sessions[0].id.as_ref().unwrap();
    assert!(id.starts_with("s-"), "synthesized id format: {id}");
    assert!(
        id.contains("20260613-143000"),
        "synthesized id should contain original timestamp: {id}"
    );
}

#[test]
fn close_session_by_id_closes_only_targeted() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("session one", "2026-06-15T10:00:00Z");
    let s2 = make_session("session two", "2026-06-15T11:00:00Z");
    write_open_session(&sessions_dir, &s1).unwrap();
    write_open_session(&sessions_dir, &s2).unwrap();

    let sessions = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(sessions.len(), 2);

    let target_id = sessions[0].id.as_ref().unwrap().clone();
    let other_id = sessions[1].id.as_ref().unwrap().clone();

    let result = close_session_by_id(&sessions_dir, &target_id).unwrap();
    assert!(result.is_some(), "should close the targeted session");

    let remaining_open = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(remaining_open.len(), 1);
    assert_eq!(remaining_open[0].id.as_deref().unwrap(), other_id);
}

#[test]
fn activate_session_by_id_activates_only_targeted() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("session one", "2026-06-15T10:00:00Z");
    let s2 = make_session("session two", "2026-06-15T11:00:00Z");
    write_open_session(&sessions_dir, &s1).unwrap();
    write_open_session(&sessions_dir, &s2).unwrap();

    let sessions = read_open_sessions(&sessions_dir).unwrap();
    let target_id = sessions[0].id.as_ref().unwrap().clone();

    let result = activate_session_by_id(&sessions_dir, &target_id).unwrap();
    assert!(result.is_some(), "should activate the targeted session");

    let remaining_open = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(remaining_open.len(), 1, "one session should remain open");

    let active = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(active.len(), 1, "one session should be active");
    assert_eq!(active[0].id.as_deref().unwrap(), target_id);
}

#[test]
fn close_session_by_id_nonexistent_returns_none() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let result = close_session_by_id(&sessions_dir, "s-nonexistent").unwrap();
    assert!(result.is_none());
}

#[test]
fn pause_active_sessions_renames_to_paused() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s = make_session("working on feature", "2026-06-15T10:00:00Z");
    write_open_session(&sessions_dir, &s).unwrap();
    activate_open_sessions(&sessions_dir).unwrap();

    let paused = pause_active_sessions(&sessions_dir).unwrap();
    assert_eq!(paused.len(), 1);
    assert!(paused[0].to_string_lossy().contains(".paused.json"));
    assert!(paused[0].exists());

    assert!(read_active_sessions(&sessions_dir).unwrap().is_empty());
    let paused_sessions = read_paused_sessions(&sessions_dir).unwrap();
    assert_eq!(paused_sessions.len(), 1);
    assert_eq!(paused_sessions[0].summary, "working on feature");
}

#[test]
fn pause_session_by_id_pauses_only_targeted() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("session one", "2026-06-15T10:00:00Z");
    let s2 = make_session("session two", "2026-06-15T11:00:00Z");
    write_open_session(&sessions_dir, &s1).unwrap();
    write_open_session(&sessions_dir, &s2).unwrap();
    activate_open_sessions(&sessions_dir).unwrap();

    let active = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(active.len(), 2);
    let target_id = active[0].id.as_ref().unwrap().clone();

    let result = pause_session_by_id(&sessions_dir, &target_id).unwrap();
    assert!(result.is_some());

    let remaining_active = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(remaining_active.len(), 1);

    let paused = read_paused_sessions(&sessions_dir).unwrap();
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].id.as_deref().unwrap(), target_id);
}

#[test]
fn resume_paused_session_by_id_reactivates() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s = make_session("paused work", "2026-06-15T10:00:00Z");
    write_open_session(&sessions_dir, &s).unwrap();
    activate_open_sessions(&sessions_dir).unwrap();
    pause_active_sessions(&sessions_dir).unwrap();

    let paused = read_paused_sessions(&sessions_dir).unwrap();
    assert_eq!(paused.len(), 1);
    let sid = paused[0].id.as_ref().unwrap().clone();

    let result = resume_paused_session_by_id(&sessions_dir, &sid).unwrap();
    assert!(result.is_some());
    assert!(result.unwrap().to_string_lossy().contains(".active.json"));

    assert!(read_paused_sessions(&sessions_dir).unwrap().is_empty());
    let active = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].summary, "paused work");
}

#[test]
fn close_session_by_id_closes_paused() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s = make_session("will close from paused", "2026-06-15T10:00:00Z");
    write_open_session(&sessions_dir, &s).unwrap();
    activate_open_sessions(&sessions_dir).unwrap();
    pause_active_sessions(&sessions_dir).unwrap();

    let paused = read_paused_sessions(&sessions_dir).unwrap();
    let sid = paused[0].id.as_ref().unwrap().clone();

    let result = close_session_by_id(&sessions_dir, &sid).unwrap();
    assert!(result.is_some());
    assert!(result.unwrap().to_string_lossy().contains(".closed.json"));

    assert!(read_paused_sessions(&sessions_dir).unwrap().is_empty());
}

#[test]
fn close_paused_sessions_closes_all() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("paused one", "2026-06-15T10:00:00Z");
    let s2 = make_session("paused two", "2026-06-15T11:00:00Z");
    write_open_session(&sessions_dir, &s1).unwrap();
    write_open_session(&sessions_dir, &s2).unwrap();
    activate_open_sessions(&sessions_dir).unwrap();
    pause_active_sessions(&sessions_dir).unwrap();

    assert_eq!(read_paused_sessions(&sessions_dir).unwrap().len(), 2);

    let closed = close_paused_sessions(&sessions_dir).unwrap();
    assert_eq!(closed.len(), 2);
    assert!(read_paused_sessions(&sessions_dir).unwrap().is_empty());
}

#[test]
fn full_lifecycle_with_pause_and_resume() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s1 = make_session("feature work", "2026-06-15T10:00:00Z");
    write_open_session(&sessions_dir, &s1).unwrap();
    activate_open_sessions(&sessions_dir).unwrap();

    let active = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(active.len(), 1);
    let s1_id = active[0].id.as_ref().unwrap().clone();

    pause_session_by_id(&sessions_dir, &s1_id).unwrap();
    assert!(read_active_sessions(&sessions_dir).unwrap().is_empty());
    assert_eq!(read_paused_sessions(&sessions_dir).unwrap().len(), 1);

    let s2 = make_session("urgent fix", "2026-06-15T12:00:00Z");
    write_open_session(&sessions_dir, &s2).unwrap();
    activate_open_sessions(&sessions_dir).unwrap();

    let active2 = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(active2.len(), 1);
    assert_eq!(active2[0].summary, "urgent fix");
    assert_eq!(read_paused_sessions(&sessions_dir).unwrap().len(), 1);

    close_active_sessions(&sessions_dir).unwrap();

    resume_paused_session_by_id(&sessions_dir, &s1_id).unwrap();
    let active3 = read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(active3.len(), 1);
    assert_eq!(active3[0].summary, "feature work");
    assert!(read_paused_sessions(&sessions_dir).unwrap().is_empty());
}

#[test]
fn enforce_history_limit_ignores_paused() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    for i in 1..=3 {
        let name = format!("20260610-{i:06}-s{i}.closed.json");
        fs::write(
            sessions_dir.join(&name),
            format!(r#"{{"version":2,"summary":"closed {i}"}}"#),
        )
        .unwrap();
    }

    let s = make_session("paused session", "2026-06-15T10:00:00Z");
    write_open_session(&sessions_dir, &s).unwrap();
    activate_open_sessions(&sessions_dir).unwrap();
    pause_active_sessions(&sessions_dir).unwrap();

    enforce_history_limit(&sessions_dir, 2).unwrap();

    let paused = read_paused_sessions(&sessions_dir).unwrap();
    assert_eq!(paused.len(), 1, "paused sessions should not be removed");
}

#[test]
fn pause_session_by_id_pauses_open_session() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s = make_session("open session", "2026-06-20T10:00:00Z");
    write_open_session(&sessions_dir, &s).unwrap();

    let open = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(open.len(), 1);
    let sid = open[0].id.as_ref().unwrap().clone();

    let result = pause_session_by_id(&sessions_dir, &sid).unwrap();
    assert!(result.is_some(), "should pause the open session");
    assert!(result.unwrap().to_string_lossy().contains(".paused.json"));

    assert!(read_open_sessions(&sessions_dir).unwrap().is_empty());
    let paused = read_paused_sessions(&sessions_dir).unwrap();
    assert_eq!(paused.len(), 1);
    assert_eq!(paused[0].id.as_deref().unwrap(), sid);
}

#[test]
fn pause_session_by_id_prefers_active_over_open() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s = make_session("test session", "2026-06-20T10:00:00Z");
    write_open_session(&sessions_dir, &s).unwrap();
    let open = read_open_sessions(&sessions_dir).unwrap();
    let sid = open[0].id.as_ref().unwrap().clone();

    activate_open_sessions(&sessions_dir).unwrap();
    assert_eq!(read_active_sessions(&sessions_dir).unwrap().len(), 1);

    let result = pause_session_by_id(&sessions_dir, &sid).unwrap();
    assert!(result.is_some());

    assert!(read_active_sessions(&sessions_dir).unwrap().is_empty());
    let paused = read_paused_sessions(&sessions_dir).unwrap();
    assert_eq!(paused.len(), 1);
}

#[test]
fn close_session_by_id_prefix_match() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let mut data = make_session("prefix test", "2026-06-20T12:00:00Z");
    data.id = Some("s-20260620-120000-123456".to_string());
    write_open_session(&sessions_dir, &data).unwrap();

    let short_id = "s-20260620-120000";
    let result = close_session_by_id(&sessions_dir, short_id).unwrap();
    assert!(result.is_some(), "prefix match should find the session");
    assert!(read_open_sessions(&sessions_dir).unwrap().is_empty());
}

#[test]
fn close_session_by_id_synthesized_prefix_match() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let content = serde_json::json!({
        "version": 2,
        "summary": "old session without id",
        "ended_at": "2026-06-15T08:30:00Z"
    });
    fs::write(
        sessions_dir.join("20260615-083000-old-session.open.json"),
        serde_json::to_string_pretty(&content).unwrap(),
    )
    .unwrap();

    let result = close_session_by_id(&sessions_dir, "s-20260615-083000").unwrap();
    assert!(result.is_some(), "synthesized ID prefix match should work");
}

#[test]
fn close_session_by_id_ambiguous_prefix_errors() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let mut data1 = make_session("session A", "2026-06-20T12:00:00Z");
    data1.id = Some("s-20260620-120000-111111".to_string());
    write_open_session(&sessions_dir, &data1).unwrap();

    let mut data2 = make_session("session B", "2026-06-20T12:00:00Z");
    data2.id = Some("s-20260620-120000-222222".to_string());
    write_open_session(&sessions_dir, &data2).unwrap();

    let result = close_session_by_id(&sessions_dir, "s-20260620-120000");
    assert!(result.is_err(), "ambiguous prefix should error");
    let err_msg = result.unwrap_err().to_string();
    assert!(
        err_msg.contains("Ambiguous"),
        "error should mention ambiguity: {err_msg}"
    );
    assert!(
        err_msg.contains("111111"),
        "error should list candidate: {err_msg}"
    );
    assert!(
        err_msg.contains("222222"),
        "error should list candidate: {err_msg}"
    );
}

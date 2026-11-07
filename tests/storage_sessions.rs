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
        agent_id: None,
        worktree: None,
        scope: None,
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
fn session_filename_strips_trailing_non_ascii_symbols() {
    let name = generate_session_filename("設計を実装して—", "20260613-143000");
    assert_eq!(name, "20260613-143000-設計を実装して");
    let name = generate_session_filename("完了…", "20260613-143000");
    assert_eq!(name, "20260613-143000-完了");
}

#[test]
fn session_filename_normalizes_inner_punctuation() {
    let name = generate_session_filename("「設計」、レビュー。", "20260613-143000");
    assert_eq!(name, "20260613-143000-設計-レビュー");
}

#[test]
fn session_filename_symbols_only_falls_back_to_session() {
    let name = generate_session_filename("—…、。！　", "20260613-143000");
    assert_eq!(name, "20260613-143000-session");
}

#[test]
fn session_filename_keeps_nfc_japanese_with_voiced_marks() {
    let name = generate_session_filename("バグ修正ガイド", "20260613-143000");
    assert_eq!(name, "20260613-143000-バグ修正ガイド");
}

/// FR-V0473-013 AC2: only the filename slug is normalized/truncated; the saved
/// `summary` keeps the full original text, symbols included.
#[test]
fn session_filename_normalization_keeps_full_summary_in_metadata() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let summary =
        "「設計」、レビュー。長い要約の末尾まで保持されることを確認するための文章—その後も続く…";
    let data = make_session(summary, "2026-06-13T14:30:00Z");
    let path = write_open_session(&sessions_dir, &data).unwrap();

    let file_name = path.file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        !file_name.contains('「') && !file_name.contains('—'),
        "{file_name}"
    );
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed["summary"], summary);
}

/// DS-V0473-015 AC2: NFD input (base + combining voiced mark) keeps the
/// combining mark attached instead of turning it into a separator.
#[test]
fn session_filename_keeps_nfd_combining_marks() {
    // か + U+3099 (が), は + U+309A (ぱ), e + U+0301 (é)
    let name = generate_session_filename("か\u{3099}は\u{309A}e\u{0301}", "20260613-143000");
    assert_eq!(name, "20260613-143000-か\u{3099}は\u{309A}e\u{0301}");
}

#[test]
fn session_filename_drops_leading_combining_mark() {
    let name = generate_session_filename("\u{3099}\u{0301}バグ", "20260613-143000");
    assert_eq!(name, "20260613-143000-バグ");
}

/// SPEC-V0473-013 AC2: a summary far over the 40-char slug limit is saved in
/// full in the session JSON.
#[test]
fn long_summary_is_stored_in_full_in_metadata() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    let summary = "long summary ".repeat(20);
    assert!(summary.chars().count() > 40);
    let data = make_session(&summary, "2026-06-13T14:30:00Z");
    let path = write_open_session(&sessions_dir, &data).unwrap();
    let parsed: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(parsed["summary"], summary.as_str());
}

#[test]
fn session_filename_full_width_alphanumerics_kept() {
    let name = generate_session_filename("ＡＢＣ１２３", "20260613-143000");
    assert_eq!(name, "20260613-143000-ＡＢＣ１２３");
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
fn session_data_roundtrips_scope_agent_worktree_fields() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let mut s = make_session("wt session", "2026-08-21T10:00:00Z");
    s.agent_id = Some("agent-1".to_string());
    s.worktree = Some("/home/user/pro/repo-wt2".to_string());
    s.scope = Some("worktree".to_string());
    let path = write_open_session(&sessions_dir, &s).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["agent_id"], "agent-1");
    assert_eq!(parsed["worktree"], "/home/user/pro/repo-wt2");
    assert_eq!(parsed["scope"], "worktree");

    let sessions = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].agent_id.as_deref(), Some("agent-1"));
    assert_eq!(
        sessions[0].worktree.as_deref(),
        Some("/home/user/pro/repo-wt2")
    );
    assert_eq!(sessions[0].scope.as_deref(), Some("worktree"));
}

#[test]
fn session_data_without_scope_fields_serializes_without_them() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    let s = make_session("no scope fields", "2026-08-21T10:00:00Z");
    let path = write_open_session(&sessions_dir, &s).unwrap();

    let content = fs::read_to_string(&path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&content).unwrap();
    assert!(
        parsed.get("agent_id").is_none(),
        "agent_id should be omitted when None: {parsed}"
    );
    assert!(
        parsed.get("worktree").is_none(),
        "worktree should be omitted when None: {parsed}"
    );
    assert!(
        parsed.get("scope").is_none(),
        "scope should be omitted when None: {parsed}"
    );
}

/// Backward compatibility: a session file written before this field set
/// existed (no `agent_id`/`worktree`/`scope`/`parent_session_id` keys at
/// all) must still parse cleanly, with the new fields defaulting to `None`.
#[test]
fn read_legacy_session_without_scope_fields_still_parses() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();

    fs::write(
        sessions_dir.join("20260610-090000-legacy.open.json"),
        r#"{"version":2,"summary":"legacy session","branch":"main","commit":"abc1234"}"#,
    )
    .unwrap();

    let sessions = read_open_sessions(&sessions_dir).unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].agent_id, None);
    assert_eq!(sessions[0].worktree, None);
    assert_eq!(sessions[0].scope, None);
    assert_eq!(sessions[0].parent_session_id, None);
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

// --- t439.7 (WS4-2): session auto-prune is off by default ---

fn write_closed(sessions_dir: &std::path::Path, n: u32, id: &str, branch: &str) -> String {
    let name = format!("20260610-{n:06}-s{n}.closed.json");
    fs::write(
        sessions_dir.join(&name),
        format!(r#"{{"version":2,"id":"{id}","summary":"session {n}","branch":"{branch}"}}"#),
    )
    .unwrap();
    name
}

fn closed_names(sessions_dir: &std::path::Path) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(sessions_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.ends_with(".closed.json"))
        .collect();
    v.sort();
    v
}

/// AC3: a limit of 0 used to delete every closed session.
#[test]
fn enforce_history_limit_zero_removes_nothing() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    for i in 1..=4 {
        write_closed(&sessions_dir, i, &format!("s-{i}"), "main");
    }

    let removed = enforce_history_limit(&sessions_dir, 0).unwrap();

    assert_eq!(removed, 0);
    assert_eq!(closed_names(&sessions_dir).len(), 4);
}

fn ids(set: &[&str]) -> std::collections::HashSet<String> {
    set.iter().map(|s| s.to_string()).collect()
}

/// Closed file whose name and body both carry `ended_at`, with a free-form
/// slug so name order can be made to disagree with age order.
fn write_closed_at(sessions_dir: &std::path::Path, ended_at: &str, id: &str, slug: &str) {
    let compact = ended_at[..19].replace(['-', ':'], "").replace('T', "-");
    fs::write(
        sessions_dir.join(format!("{compact}-{slug}.closed.json")),
        format!(r#"{{"version":2,"id":"{id}","ended_at":"{ended_at}","summary":"{slug}"}}"#),
    )
    .unwrap();
}

#[test]
fn prune_closed_sessions_zero_limit_is_a_noop() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    write_closed(&sessions_dir, 1, "s-1", "main");
    write_closed(&sessions_dir, 2, "s-2", "main");

    let outcome = prune_closed_sessions(&sessions_dir, 0, &ids(&[])).unwrap();

    assert!(outcome.removed.is_empty());
    assert_eq!(outcome.retained_for_head, 0);
    assert_eq!(closed_names(&sessions_dir).len(), 2);
}

/// DS-V0473-310 AC2: a head passed in by the caller is never removed, even
/// when it is the oldest, and the overshoot is counted.
#[test]
fn prune_closed_sessions_never_removes_a_passed_head() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    write_closed(&sessions_dir, 1, "s-1", "main");
    write_closed(&sessions_dir, 2, "s-2", "main");
    write_closed(&sessions_dir, 3, "s-3", "main");

    let outcome = prune_closed_sessions(&sessions_dir, 1, &ids(&["s-1"])).unwrap();

    let removed_ids: Vec<&str> = outcome.removed.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(removed_ids, vec!["s-2", "s-3"]);
    assert_eq!(outcome.retained_for_head, 0);
    assert_eq!(closed_names(&sessions_dir).len(), 1);

    // With two heads and limit 1 the overshoot is reported.
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    for i in 1..=3 {
        write_closed(&sessions_dir, i, &format!("s-{i}"), "main");
    }
    let outcome = prune_closed_sessions(&sessions_dir, 1, &ids(&["s-1", "s-3"])).unwrap();
    let removed_ids: Vec<&str> = outcome.removed.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(removed_ids, vec!["s-2"]);
    assert_eq!(outcome.retained_for_head, 1);
    assert_eq!(closed_names(&sessions_dir).len(), 2);
}

#[test]
fn prune_closed_sessions_removes_oldest_and_reports_ids() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    for i in 1..=5 {
        write_closed(&sessions_dir, i, &format!("s-{i}"), "main");
    }

    let outcome = prune_closed_sessions(&sessions_dir, 3, &ids(&[])).unwrap();

    let removed_ids: Vec<&str> = outcome.removed.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(removed_ids, vec!["s-1", "s-2"]);
    assert_eq!(outcome.retained_for_head, 0);
    assert_eq!(closed_names(&sessions_dir).len(), 3);
}

/// Review finding: two closed saves in the same second share the file-name
/// timestamp, so name order is slug order. The age order must come from
/// `ended_at` (sub-second), or the just-saved session is deleted.
#[test]
fn prune_order_follows_ended_at_not_file_name_slug() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    // Same second; the NEWER save has the alphabetically SMALLER slug.
    write_closed_at(
        &sessions_dir,
        "2026-06-10T10:00:00.100000000+00:00",
        "s-old",
        "zzz-old",
    );
    write_closed_at(
        &sessions_dir,
        "2026-06-10T10:00:00.900000000+00:00",
        "s-new",
        "aaa-new",
    );

    let heads = provisional_heads(&sessions_dir).unwrap();
    assert_eq!(heads, ids(&["s-new"]));

    let outcome = prune_closed_sessions(&sessions_dir, 1, &heads).unwrap();

    let removed_ids: Vec<&str> = outcome.removed.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(removed_ids, vec!["s-old"]);
    let survivors = closed_names(&sessions_dir);
    assert_eq!(survivors.len(), 1);
    assert!(survivors[0].contains("aaa-new"), "{survivors:?}");

    // The head the prune protects is the one `previous_session` reads.
    let latest = read_latest_closed_session(&sessions_dir).unwrap().unwrap();
    assert_eq!(latest.id.as_deref(), Some("s-new"));
}

/// Without a readable `ended_at` the timestamp in the id orders the file
/// (`close_session_id` leaves `ended_at` unset on a session it closes), and a
/// file with no timestamp at all sorts as the oldest.
#[test]
fn prune_order_falls_back_to_id_timestamp_then_oldest() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    // Closed by id: no ended_at, but a fresh id.
    fs::write(
        sessions_dir.join("20260901-000000-closed-by-id.closed.json"),
        r#"{"version":2,"id":"s-20260901-000000-123456","summary":"closed by id"}"#,
    )
    .unwrap();
    write_closed_at(
        &sessions_dir,
        "2026-06-10T10:00:00+00:00",
        "s-dated",
        "dated",
    );
    // No ended_at and an id without a timestamp.
    fs::write(
        sessions_dir.join("20260101-000000-undated.closed.json"),
        r#"{"version":2,"id":"custom-id","summary":"undated"}"#,
    )
    .unwrap();

    assert_eq!(
        provisional_heads(&sessions_dir).unwrap(),
        ids(&["s-20260901-000000-123456"])
    );
    let outcome =
        prune_closed_sessions(&sessions_dir, 1, &ids(&["s-20260901-000000-123456"])).unwrap();
    let removed_ids: Vec<&str> = outcome.removed.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(removed_ids, vec!["custom-id", "s-dated"]);
}

/// SPEC-V0473-110: one chronological lineage, so exactly one head no matter
/// how many branches or worktrees the history spans.
#[test]
fn provisional_heads_is_the_single_newest_closed_session() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    assert!(provisional_heads(&sessions_dir).unwrap().is_empty());

    write_closed(&sessions_dir, 1, "s-1", "branch-a");
    write_closed(&sessions_dir, 2, "s-2", "branch-b");
    write_closed(&sessions_dir, 3, "s-3", "branch-c");

    assert_eq!(provisional_heads(&sessions_dir).unwrap(), ids(&["s-3"]));
    assert!(provisional_heads(&dir.path().join("missing"))
        .unwrap()
        .is_empty());
}

/// DS-V0473-310 AC1: 30 closed sessions over many branches, limit 10 -> the
/// newest 10 remain and no head warning is needed.
#[test]
fn provisional_heads_with_many_branches_leave_exactly_limit() {
    let dir = setup();
    let sessions_dir = dir.path().join("sessions");
    fs::create_dir_all(&sessions_dir).unwrap();
    for i in 1..=30 {
        write_closed(&sessions_dir, i, &format!("s-{i}"), &format!("branch-{i}"));
    }

    let heads = provisional_heads(&sessions_dir).unwrap();
    let outcome = prune_closed_sessions(&sessions_dir, 10, &heads).unwrap();

    assert_eq!(outcome.removed.len(), 20);
    assert_eq!(outcome.retained_for_head, 0);
    assert_eq!(closed_names(&sessions_dir).len(), 10);
}

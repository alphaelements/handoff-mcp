use handoff_mcp::storage::sessions::*;
use std::fs;
use tempfile::TempDir;

fn setup() -> TempDir {
    tempfile::tempdir().expect("failed to create temp dir")
}

fn make_session(summary: &str, ended_at: &str) -> SessionData {
    SessionData {
        version: 2,
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

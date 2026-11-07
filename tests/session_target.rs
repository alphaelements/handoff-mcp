//! Unit tests for `storage::session_target` — the single place that decides
//! which session an operation touches (t439.5, FR-V0473-008).

use std::path::PathBuf;

use handoff_mcp::storage::session_target::{
    activatable_open, adoptable, effective_owner, own_active_group, resolve_session_target, Caller,
    Resolution, TargetOp, EVENT_FOREIGN_CLOSE, EVENT_FOREIGN_WRITE, OWNER_V,
};
use handoff_mcp::storage::sessions::{adopt_session, OwnerStamp, SessionData, SessionEntry};

const WT: &str = "/work/main";

fn session(id: &str, owner: Option<&str>, owner_v: bool, worktree: Option<&str>) -> SessionData {
    SessionData {
        version: 2,
        id: Some(id.to_string()),
        ended_at: Some(format!("2026-01-01T00:00:{}+00:00", &id[id.len() - 2..])),
        summary: format!("summary {id}"),
        branch: None,
        commit: None,
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
        agent_id: owner.map(String::from),
        owner_v: if owner_v { Some(OWNER_V) } else { None },
        worktree: worktree.map(String::from),
        scope: None,
        ..Default::default()
    }
}

fn entry(status: &str, data: SessionData) -> SessionEntry {
    SessionEntry {
        data,
        status: status.to_string(),
        path: PathBuf::from(format!("/tmp/x.{status}.json")),
    }
}

/// A session owned (with a trusted `owner_v`) by `owner` in `WT`.
fn owned(id: &str, owner: &str, status: &str) -> SessionEntry {
    entry(status, session(id, Some(owner), true, Some(WT)))
}

fn me(agent: &str) -> Caller<'_> {
    Caller {
        agent_id: Some(agent),
        worktree: WT,
    }
}

fn target_id(r: &Resolution) -> &str {
    match r {
        Resolution::Target(t) => &t.id,
        other => panic!("expected a target, got {other:?}"),
    }
}

#[test]
fn owner_without_owner_v_is_not_trusted() {
    let legacy = session("s-0001", Some("a-20260101-000000-000000"), false, None);
    assert_eq!(effective_owner(&legacy), None);
    let current = session("s-0002", Some("agent-a"), true, None);
    assert_eq!(effective_owner(&current), Some("agent-a"));
    let no_agent = session("s-0003", None, true, None);
    assert_eq!(effective_owner(&no_agent), None);
}

#[test]
fn implicit_close_prefers_the_callers_own_active_over_another_agents() {
    let entries = vec![
        owned("s-0001", "agent-b", "active"),
        owned("s-0002", "agent-a", "active"),
        owned("s-0003", "agent-b", "active"),
    ];
    let r = resolve_session_target(&entries, &me("agent-a"), None, TargetOp::Close, false);
    assert_eq!(target_id(&r), "s-0002");
    if let Resolution::Target(t) = r {
        assert!(t.event.is_none(), "own session needs no event");
        assert!(t.warnings.is_empty());
    }
}

#[test]
fn implicit_choice_ignores_actives_in_other_worktrees() {
    let mut other_wt = session("s-0001", Some("agent-a"), true, Some("/work/other"));
    other_wt.worktree = Some("/work/other".to_string());
    let entries = vec![entry("active", other_wt)];
    let r = resolve_session_target(&entries, &me("agent-a"), None, TargetOp::Close, false);
    assert_eq!(r, Resolution::NoTarget);
}

#[test]
fn implicit_choice_with_only_foreign_actives_finds_nothing() {
    let entries = vec![owned("s-0001", "agent-b", "active")];
    for op in [
        TargetOp::Close,
        TargetOp::Overwrite,
        TargetOp::Append,
        TargetOp::Select,
    ] {
        let r = resolve_session_target(&entries, &me("agent-a"), None, op, false);
        assert_eq!(r, Resolution::NoTarget, "{op:?}");
    }
}

#[test]
fn implicit_choice_falls_back_to_ownerless_sessions_with_an_event() {
    let entries = vec![
        entry("active", session("s-0001", None, false, None)),
        owned("s-0002", "agent-b", "active"),
    ];
    let r = resolve_session_target(&entries, &me("agent-a"), None, TargetOp::Close, false);
    let Resolution::Target(t) = r else {
        panic!("expected target: {r:?}")
    };
    assert_eq!(t.id, "s-0001");
    assert_eq!(t.event, Some(EVENT_FOREIGN_CLOSE));
    assert!(!t.warnings.is_empty());
}

#[test]
fn selecting_an_ownerless_session_is_silent() {
    let entries = vec![entry(
        "active",
        session("s-0001", Some("a-old-generated"), false, None),
    )];
    let r = resolve_session_target(&entries, &me("agent-a"), None, TargetOp::Select, false);
    let Resolution::Target(t) = r else {
        panic!("expected target")
    };
    assert_eq!(t.id, "s-0001");
    assert!(t.event.is_none());
    assert!(t.warnings.is_empty());
}

#[test]
fn several_own_actives_make_a_close_ambiguous_but_an_append_uses_the_latest() {
    let entries = vec![
        owned("s-0001", "agent-a", "active"),
        owned("s-0003", "agent-a", "active"),
        owned("s-0002", "agent-a", "active"),
    ];
    let close = resolve_session_target(&entries, &me("agent-a"), None, TargetOp::Close, false);
    assert_eq!(
        close,
        Resolution::Ambiguous {
            ids: vec!["s-0001".into(), "s-0003".into(), "s-0002".into()]
        }
    );
    let append = resolve_session_target(&entries, &me("agent-a"), None, TargetOp::Append, false);
    let Resolution::Target(t) = append else {
        panic!("expected target")
    };
    assert_eq!(t.id, "s-0003", "latest ended_at wins");
    assert!(t.warnings.iter().any(|w| w.contains("s-0003")));
}

#[test]
fn caller_without_identity_gets_ownerless_sessions_only() {
    let cli = Caller {
        agent_id: None,
        worktree: WT,
    };
    let only_ownerless = vec![entry("active", session("s-0001", None, false, None))];
    let r = resolve_session_target(&only_ownerless, &cli, None, TargetOp::Close, false);
    let Resolution::Target(t) = r else {
        panic!("expected target")
    };
    assert!(t.event.is_none(), "CLI closing CLI data is silent");

    let with_owned = vec![
        entry("active", session("s-0001", None, false, None)),
        owned("s-0002", "agent-a", "active"),
    ];
    let r = resolve_session_target(&with_owned, &cli, None, TargetOp::Close, false);
    assert_eq!(
        r,
        Resolution::NeedsExplicitId {
            ids: vec!["s-0002".into(), "s-0001".into()]
        }
    );

    let none = resolve_session_target(&[], &cli, None, TargetOp::Close, false);
    assert_eq!(none, Resolution::NoTarget);
}

#[test]
fn explicit_close_of_a_foreign_active_is_refused_unless_forced() {
    let entries = vec![owned("s-0001", "agent-b", "active")];
    for op in [TargetOp::Close, TargetOp::Pause, TargetOp::Merge] {
        let r = resolve_session_target(&entries, &me("agent-a"), Some("s-0001"), op, false);
        assert_eq!(
            r,
            Resolution::Refused {
                id: "s-0001".into(),
                owner: "agent-b".into()
            },
            "{op:?}"
        );
        let forced = resolve_session_target(&entries, &me("agent-a"), Some("s-0001"), op, true);
        let Resolution::Target(t) = forced else {
            panic!("forced {op:?} should proceed")
        };
        assert!(t.forced);
        assert!(t.event.is_some());
    }
}

#[test]
fn explicit_append_or_overwrite_of_a_foreign_active_is_allowed_with_a_write_event() {
    let entries = vec![owned("s-0001", "agent-b", "active")];
    for op in [TargetOp::Append, TargetOp::Overwrite] {
        let r = resolve_session_target(&entries, &me("agent-a"), Some("s-0001"), op, false);
        let Resolution::Target(t) = r else {
            panic!("expected target for {op:?}: {r:?}")
        };
        assert_eq!(t.event, Some(EVENT_FOREIGN_WRITE), "{op:?}");
        assert_eq!(t.foreign_owner.as_deref(), Some("agent-b"), "{op:?}");
        assert!(!t.forced, "{op:?}");
    }
}

#[test]
fn explicit_close_or_pause_of_a_foreign_paused_or_open_session_is_allowed_with_an_event() {
    for status in ["paused", "open"] {
        let entries = vec![owned("s-0001", "agent-b", status)];
        let r = resolve_session_target(
            &entries,
            &me("agent-a"),
            Some("s-0001"),
            TargetOp::Close,
            false,
        );
        let Resolution::Target(t) = r else {
            panic!("{status}: expected target")
        };
        assert_eq!(t.event, Some(EVENT_FOREIGN_CLOSE), "{status}");
        assert!(!t.forced);
    }
}

#[test]
fn explicit_ops_only_see_the_statuses_they_can_act_on() {
    let entries = vec![owned("s-0001", "agent-a", "paused")];
    let c = me("agent-a");
    assert!(matches!(
        resolve_session_target(&entries, &c, Some("s-0001"), TargetOp::Close, false),
        Resolution::Target(_)
    ));
    assert_eq!(
        resolve_session_target(&entries, &c, Some("s-0001"), TargetOp::Append, false),
        Resolution::NotFound
    );
    assert_eq!(
        resolve_session_target(&entries, &c, Some("s-0001"), TargetOp::Pause, false),
        Resolution::NotFound
    );
    assert!(matches!(
        resolve_session_target(&entries, &c, Some("s-0001"), TargetOp::Select, false),
        Resolution::Target(_)
    ));
}

#[test]
fn explicit_id_accepts_a_prefix_and_reports_ambiguity() {
    let entries = vec![
        owned("s-20260101-000001", "agent-a", "active"),
        owned("s-20260101-000002", "agent-a", "active"),
    ];
    let c = me("agent-a");
    let r = resolve_session_target(
        &entries,
        &c,
        Some("s-20260101-000002"),
        TargetOp::Close,
        false,
    );
    assert_eq!(target_id(&r), "s-20260101-000002");
    let r = resolve_session_target(&entries, &c, Some("s-2026"), TargetOp::Close, false);
    assert!(matches!(r, Resolution::Ambiguous { .. }), "{r:?}");
}

#[test]
fn caller_without_identity_may_touch_an_owned_session_by_id_with_a_warning() {
    let cli = Caller {
        agent_id: None,
        worktree: WT,
    };
    let entries = vec![owned("s-0001", "agent-a", "active")];
    let r = resolve_session_target(&entries, &cli, Some("s-0001"), TargetOp::Close, false);
    let Resolution::Target(t) = r else {
        panic!("expected target")
    };
    assert_eq!(t.event, Some(EVENT_FOREIGN_CLOSE));
    assert!(!t.warnings.is_empty());
}

#[test]
fn select_reports_the_foreign_owner_so_the_handler_can_require_adopt() {
    let entries = vec![
        owned("s-0001", "agent-b", "active"),
        owned("s-0002", "agent-b", "paused"),
    ];
    for id in ["s-0001", "s-0002"] {
        let r = resolve_session_target(&entries, &me("agent-a"), Some(id), TargetOp::Select, false);
        let Resolution::Target(t) = r else {
            panic!("expected target")
        };
        assert_eq!(t.foreign_owner.as_deref(), Some("agent-b"));
        assert!(t.event.is_none(), "adopting logs its own event");
    }
}

#[test]
fn own_active_group_is_mine_plus_ownerless_in_this_worktree() {
    let entries = vec![
        owned("s-0001", "agent-a", "active"),
        owned("s-0002", "agent-b", "active"),
        entry("active", session("s-0003", None, false, None)),
        owned("s-0004", "agent-a", "paused"),
        entry(
            "active",
            session("s-0005", Some("agent-a"), true, Some("/work/other")),
        ),
    ];
    let ids: Vec<_> = own_active_group(&entries, &me("agent-a"))
        .into_iter()
        .map(|e| e.data.id.clone().unwrap())
        .collect();
    assert_eq!(ids, vec!["s-0001", "s-0003"]);

    let cli = Caller {
        agent_id: None,
        worktree: WT,
    };
    assert!(
        own_active_group(&entries, &cli).is_empty(),
        "an owned active here means the CLI must name a session"
    );
}

#[test]
fn only_own_and_ownerless_open_sessions_are_activatable() {
    let entries = vec![
        owned("s-0001", "agent-a", "open"),
        owned("s-0002", "agent-b", "open"),
        entry("open", session("s-0003", None, false, None)),
        owned("s-0004", "agent-a", "paused"),
    ];
    let ids: Vec<_> = activatable_open(&entries, &me("agent-a"))
        .into_iter()
        .map(|e| e.data.id.clone().unwrap())
        .collect();
    assert_eq!(ids, vec!["s-0001", "s-0003"]);
}

#[test]
fn adoptable_lists_foreign_active_and_paused_newest_first() {
    let entries = vec![
        owned("s-0001", "agent-b", "paused"),
        owned("s-0003", "agent-b", "active"),
        owned("s-0002", "agent-a", "active"),
        owned("s-0004", "agent-b", "open"),
        entry("active", session("s-0005", None, false, None)),
    ];
    let ids: Vec<_> = adoptable(&entries, &me("agent-a"))
        .into_iter()
        .map(|e| e.data.id.clone().unwrap())
        .collect();
    assert_eq!(ids, vec!["s-0003", "s-0001"]);
}

// ---- adopt_session: the owner comparison happens under a lock ------------

#[test]
fn two_simultaneous_adopts_of_one_session_have_exactly_one_winner() {
    let dir = tempfile::tempdir().unwrap();
    let handoff = dir.path().join(".handoff");
    let sessions_dir = handoff.join("sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let data = session(
        "s-20260101-000001-000001",
        Some("agent-old"),
        true,
        Some(WT),
    );
    handoff_mcp::storage::sessions::write_session_with_status(&sessions_dir, &data, "active")
        .unwrap();

    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles: Vec<_> = ["agent-x", "agent-y"]
        .into_iter()
        .map(|new_owner| {
            let barrier = barrier.clone();
            let sessions_dir = sessions_dir.clone();
            std::thread::spawn(move || {
                barrier.wait();
                adopt_session(
                    &sessions_dir,
                    "s-20260101-000001-000001",
                    Some("agent-old"),
                    &OwnerStamp {
                        agent_id: new_owner,
                        worktree: "/new/wt",
                        scope: "primary",
                    },
                    "active",
                )
                .map(|o| (new_owner, o))
            })
        })
        .collect();
    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    let winners: Vec<_> = results.iter().filter_map(|r| r.as_ref().ok()).collect();
    assert_eq!(winners.len(), 1, "exactly one adopt succeeds: {results:?}");
    let loser = results.iter().find_map(|r| r.as_ref().err()).unwrap();
    assert!(
        loser.to_string().contains("changed owner"),
        "loser is told the owner changed: {loser}"
    );
    let (winner, outcome) = winners[0];
    assert_eq!(outcome.previous_owner.as_deref(), Some("agent-old"));
    let on_disk = handoff_mcp::storage::sessions::read_active_sessions(&sessions_dir).unwrap();
    assert_eq!(on_disk.len(), 1);
    assert_eq!(on_disk[0].agent_id.as_deref(), Some(*winner));
    assert_eq!(on_disk[0].worktree.as_deref(), Some("/new/wt"));
    assert_eq!(on_disk[0].scope.as_deref(), Some("primary"));
}

#[test]
fn adopt_rewrites_body_owner_fields_and_moves_a_paused_session_to_active() {
    let dir = tempfile::tempdir().unwrap();
    let sessions_dir = dir.path().join(".handoff/sessions");
    std::fs::create_dir_all(&sessions_dir).unwrap();
    let mut data = session(
        "s-20260101-000001-000001",
        Some("agent-old"),
        true,
        Some("/old/wt"),
    );
    data.scope = Some("worktree".to_string());
    handoff_mcp::storage::sessions::write_session_with_status(&sessions_dir, &data, "paused")
        .unwrap();

    let outcome = adopt_session(
        &sessions_dir,
        "s-20260101-000001-000001",
        Some("agent-old"),
        &OwnerStamp {
            agent_id: "agent-new",
            worktree: "/new/wt",
            scope: "primary",
        },
        "active",
    )
    .unwrap();

    assert_eq!(outcome.previous_owner.as_deref(), Some("agent-old"));
    assert_eq!(outcome.previous_worktree.as_deref(), Some("/old/wt"));
    assert_eq!(outcome.previous_scope.as_deref(), Some("worktree"));
    assert!(outcome.path.to_string_lossy().ends_with(".active.json"));
    assert!(
        handoff_mcp::storage::sessions::read_paused_sessions(&sessions_dir)
            .unwrap()
            .is_empty()
    );
    assert_eq!(outcome.data.agent_id.as_deref(), Some("agent-new"));
    assert_eq!(outcome.data.owner_v, Some(OWNER_V));
}

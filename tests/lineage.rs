//! Unit tests for `storage::lineage` — head detection, candidate narrowing,
//! scoring and the confirm/ambiguous decision (t439.6, FR-V0473-009).

use std::cell::Cell;
use std::collections::BTreeSet;

use chrono::{DateTime, TimeZone, Utc};
use handoff_mcp::storage::lineage::{
    decide, dirty_paths_from_porcelain, extract_task_ids, find_heads, score, Decision, GitFacts,
    Head, Identity,
};
use handoff_mcp::storage::sessions::SessionData;

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 11, 8, 12, 0, 0).unwrap()
}

/// A closed session ended `days_ago` days (plus `minute` minutes) before `now()`.
fn closed(id: &str, days_ago: i64, minute: i64) -> SessionData {
    let ended = now() - chrono::Duration::days(days_ago) + chrono::Duration::minutes(minute);
    SessionData {
        version: 2,
        id: Some(id.to_string()),
        ended_at: Some(ended.to_rfc3339()),
        summary: format!("summary {id}"),
        ..Default::default()
    }
}

/// New-format closed session continuing `prev`.
fn lineage(id: &str, days_ago: i64, minute: i64, prev: &[&str]) -> SessionData {
    let mut s = closed(id, days_ago, minute);
    s.lineage_v = Some(1);
    s.continues = Some(prev.iter().map(|p| p.to_string()).collect());
    s
}

fn ids(heads: &[Head<'_>]) -> Vec<String> {
    let mut v: Vec<String> = heads
        .iter()
        .map(|h| h.session.id.clone().unwrap())
        .collect();
    v.sort();
    v
}

fn identity() -> Identity {
    Identity::default()
}

fn no_git() -> GitFacts {
    GitFacts::default()
}

// ---------------------------------------------------------------- heads --

/// SPEC-V0473-009 AC3: old data is one chronological lineage.
#[test]
fn legacy_sessions_have_only_the_newest_as_head() {
    let all: Vec<SessionData> = (0..10)
        .map(|i| closed(&format!("s-{i}"), 20 - i, 0))
        .collect();
    let heads = find_heads(&all, &[]);
    assert_eq!(ids(&heads), vec!["s-9"]);
}

/// AC4: a chain has one head.
#[test]
fn a_chain_has_one_head() {
    let all = vec![
        lineage("a", 3, 0, &[]),
        lineage("b", 2, 0, &["a"]),
        lineage("c", 1, 0, &["b"]),
    ];
    assert_eq!(ids(&find_heads(&all, &[])), vec!["c"]);
}

/// AC5: a new closed continuing the legacy tip removes the legacy tip.
#[test]
fn new_closed_continuing_the_legacy_tip_replaces_it_as_head() {
    let all = vec![
        closed("old-1", 9, 0),
        closed("old-2", 8, 0),
        lineage("new-1", 1, 0, &["old-2"]),
    ];
    assert_eq!(ids(&find_heads(&all, &[])), vec!["new-1"]);
}

/// A new lineage that does not continue the legacy tip leaves both heads.
#[test]
fn new_origin_keeps_the_legacy_tip_as_a_second_head() {
    let all = vec![closed("old-1", 9, 0), lineage("new-1", 1, 0, &[])];
    assert_eq!(ids(&find_heads(&all, &[])), vec!["new-1", "old-1"]);
}

/// AC7: `lineage_end` and worktree children are never heads.
#[test]
fn lineage_end_and_worktree_children_are_not_heads() {
    let mut ended = lineage("ended", 3, 0, &[]);
    ended.lineage_end = Some(true);
    let mut child = lineage("child", 2, 0, &["root"]);
    child.parent_session_id = Some("root".into());
    child.scope = Some("worktree".into());
    let root = lineage("root", 4, 0, &[]);
    let all = [ended, child, root];
    let heads = find_heads(&all, &[]);
    // `child` and `ended` are excluded. `child` hangs off `root` without
    // superseding it, so `root` stays a head.
    assert_eq!(ids(&heads), vec!["root"]);
}

/// A fork (has a parent but is not a worktree session) stays a head.
#[test]
fn a_forked_closed_session_is_still_a_head() {
    let mut fork = lineage("fork", 1, 0, &["src"]);
    fork.parent_session_id = Some("src".into());
    fork.scope = Some("primary".into());
    let src = lineage("src", 2, 0, &[]);
    assert_eq!(ids(&find_heads(&[fork, src], &[])), vec!["fork"]);
}

/// AC6: an active continuing a head marks it in flight.
#[test]
fn active_continuing_a_head_marks_it_in_flight() {
    let all = vec![lineage("x", 2, 0, &[]), lineage("y", 1, 0, &[])];
    let mut active = SessionData {
        id: Some("act".into()),
        ..Default::default()
    };
    active.lineage_v = Some(1);
    active.continues = Some(vec!["x".into()]);
    let heads = find_heads(&all, &[&active]);
    assert_eq!(ids(&heads), vec!["x", "y"]);
    let x = heads
        .iter()
        .find(|h| h.session.id.as_deref() == Some("x"))
        .unwrap();
    assert_eq!(x.in_flight_by, vec!["act".to_string()]);
    let y = heads
        .iter()
        .find(|h| h.session.id.as_deref() == Some("y"))
        .unwrap();
    assert!(y.in_flight_by.is_empty());
}

/// A pruned (dangling) `continues` target is simply ignored.
#[test]
fn dangling_continues_are_ignored() {
    let all = vec![lineage("b", 1, 0, &["pruned"])];
    assert_eq!(ids(&find_heads(&all, &[])), vec!["b"]);
}

// ------------------------------------------------------------- decision --

fn heads_of(all: &[SessionData]) -> Vec<Head<'_>> {
    find_heads(all, &[])
}

#[test]
fn no_heads_means_nothing_to_decide() {
    let heads: Vec<Head<'_>> = Vec::new();
    let d = decide(&heads, now(), &identity(), no_git);
    assert!(matches!(d, Decision::NoHeads));
}

/// SPEC-V0473-209 AC1: one candidate is confirmed without any git work and
/// without a score threshold.
#[test]
fn single_candidate_is_confirmed_without_git() {
    let mut s = lineage("only", 1, 0, &[]);
    s.branch = Some("some-other-branch".into());
    s.commit = Some("deadbee".into());
    let all = vec![s];
    let heads = heads_of(&all);
    let calls = Cell::new(0);
    let d = decide(&heads, now(), &identity(), || {
        calls.set(calls.get() + 1);
        no_git()
    });
    match d {
        Decision::Confirmed { head, evidence } => {
            assert_eq!(head.session.id.as_deref(), Some("only"));
            assert_eq!(evidence["candidates"], 1);
            let not_evaluated: Vec<&str> = evidence["not_evaluated"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect();
            assert!(not_evaluated.contains(&"branch"), "{evidence}");
            assert!(not_evaluated.contains(&"commit"), "{evidence}");
        }
        other => panic!("expected Confirmed, got {other:?}"),
    }
    assert_eq!(
        calls.get(),
        0,
        "git must not be consulted for one candidate"
    );
}

/// SPEC-V0473-109 AC2: the git-free signals appear in the evidence.
#[test]
fn single_candidate_evidence_lists_git_free_signals() {
    let mut s = lineage("only", 1, 0, &[]);
    s.agent_id = Some("agent-1".into());
    s.owner_v = Some(1);
    s.worktree_key = Some("main".into());
    s.related_task_ids = vec!["t5".into()];
    let all = vec![s];
    let heads = heads_of(&all);
    let id = Identity {
        agent_id: Some("agent-1".into()),
        worktree_key: Some("main".into()),
        claimed: ["t5".to_string()].into(),
    };
    match decide(&heads, now(), &id, no_git) {
        Decision::Confirmed { evidence, .. } => {
            assert_eq!(evidence["signals"]["agent_id"], 1);
            assert_eq!(evidence["signals"]["worktree"], 1);
            assert_eq!(evidence["signals"]["task_overlap"], 2);
        }
        other => panic!("expected Confirmed, got {other:?}"),
    }
}

fn two_heads(a_branch: &str, b_branch: &str) -> Vec<SessionData> {
    let mut a = lineage("a", 1, 10, &[]);
    a.branch = Some(a_branch.into());
    let mut b = lineage("b", 1, 0, &[]);
    b.branch = Some(b_branch.into());
    vec![a, b]
}

fn git_on(branch: &str) -> GitFacts {
    GitFacts {
        branch: Some(branch.into()),
        ..Default::default()
    }
}

/// AC2: a gap of 3 (branch) confirms the matching lineage.
#[test]
fn two_candidates_with_a_clear_winner_are_confirmed() {
    let all = two_heads("feature-a", "feature-b");
    let heads = heads_of(&all);
    match decide(&heads, now(), &identity(), || git_on("feature-b")) {
        Decision::Confirmed { head, evidence } => {
            assert_eq!(head.session.id.as_deref(), Some("b"));
            assert_eq!(evidence["candidates"], 2);
            assert_eq!(evidence["score"], 3);
            assert_eq!(evidence["runner_up_score"], 0);
            assert_eq!(evidence["signals"]["branch"], 3);
        }
        other => panic!("expected Confirmed, got {other:?}"),
    }
}

/// AC2/AC6: a gap below 2 is ambiguous, ranked by score then recency.
#[test]
fn two_candidates_with_a_small_gap_are_ambiguous() {
    // Same branch for both: 0 gap; `a` ended later so it ranks first.
    let all = two_heads("main", "main");
    let heads = heads_of(&all);
    match decide(&heads, now(), &identity(), || git_on("main")) {
        Decision::Ambiguous { ranked, stale } => {
            assert!(!stale);
            let order: Vec<&str> = ranked
                .iter()
                .map(|r| r.head.session.id.as_deref().unwrap())
                .collect();
            assert_eq!(order, vec!["a", "b"]);
            assert_eq!(ranked[0].score, Some(3));
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

/// agent_id is worth +1 and cannot decide alone (AC5 of SPEC-V0473-309).
#[test]
fn agent_id_alone_cannot_confirm() {
    let mut all = two_heads("main", "main");
    all[1].agent_id = Some("me".into());
    all[1].owner_v = Some(1);
    let heads = heads_of(&all);
    let id = Identity {
        agent_id: Some("me".into()),
        ..Default::default()
    };
    assert!(matches!(
        decide(&heads, now(), &id, || git_on("main")),
        Decision::Ambiguous { .. }
    ));
}

/// A head older than 30 days is not a candidate; of 9 recent ones only the
/// newest 8 are scored.
#[test]
fn candidates_are_the_newest_eight_within_thirty_days() {
    let mut all: Vec<SessionData> = (0..9)
        .map(|i| lineage(&format!("h{i}"), 1, i, &[]))
        .collect();
    all.push(lineage("ancient", 40, 0, &[]));
    let heads = heads_of(&all);
    match decide(&heads, now(), &identity(), no_git) {
        Decision::Ambiguous { ranked, .. } => {
            let shown: BTreeSet<&str> = ranked
                .iter()
                .map(|r| r.head.session.id.as_deref().unwrap())
                .collect();
            assert!(!shown.contains("ancient"));
            assert!(!shown.contains("h0"), "the oldest of nine is dropped");
            assert_eq!(shown.len(), 5, "at most five are listed");
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

/// The 9th-newest head is outside the scored set: even a perfect match on it
/// cannot win.
#[test]
fn only_eight_candidates_are_scored() {
    let mut all: Vec<SessionData> = (0..9)
        .map(|i| lineage(&format!("h{i}"), 1, i, &[]))
        .collect();
    all[0].branch = Some("target".into()); // h0 is the oldest of nine
    let heads = heads_of(&all);
    match decide(&heads, now(), &identity(), || git_on("target")) {
        Decision::Ambiguous { ranked, .. } => {
            assert!(ranked
                .iter()
                .all(|r| r.head.session.id.as_deref() != Some("h0")));
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }
}

/// SPEC-V0473-209 AC4: a head another agent's active session continues is never
/// confirmed itself: not as the only candidate, and not as the top scorer.
#[test]
fn in_flight_head_is_never_auto_confirmed() {
    let all = vec![lineage("x", 1, 0, &[])];
    let mut active = SessionData {
        id: Some("act".into()),
        ..Default::default()
    };
    active.continues = Some(vec!["x".into()]);
    let heads = find_heads(&all, &[&active]);
    match decide(&heads, now(), &identity(), no_git) {
        Decision::Ambiguous { ranked, .. } => {
            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].head.in_flight_by, vec!["act".to_string()]);
        }
        other => panic!("expected Ambiguous, got {other:?}"),
    }

    // `b` is in flight and wins on the branch: it must not be confirmed.
    let two = two_heads("feature-a", "feature-b");
    let heads = find_heads(&two, &[&active_for("b")]);
    assert!(matches!(
        decide(&heads, now(), &identity(), || git_on("feature-b")),
        Decision::Ambiguous { .. }
    ));
}

/// SPEC-V0473-209 AC4 (AC2 of the requirement): another lineage's head being in
/// flight does not stop a clear winner elsewhere from being confirmed.
#[test]
fn clear_winner_is_confirmed_although_a_losing_head_is_in_flight() {
    let two = two_heads("feature-a", "feature-b");
    let heads = find_heads(&two, &[&active_for("b")]);
    match decide(&heads, now(), &identity(), || git_on("feature-a")) {
        Decision::Confirmed { head, .. } => assert_eq!(head.session.id.as_deref(), Some("a")),
        other => panic!("expected Confirmed, got {other:?}"),
    }
}

/// An in-flight runner-up within the margin of the leader keeps the choice
/// ambiguous (the lead is too small to confirm anyway).
#[test]
fn in_flight_runner_up_within_the_margin_is_ambiguous() {
    // Equal scores: no lead at all.
    let two = two_heads("same", "same");
    let heads = find_heads(&two, &[&active_for("b")]);
    assert!(matches!(
        decide(&heads, now(), &identity(), || git_on("same")),
        Decision::Ambiguous { .. }
    ));
}

fn active_for(head: &str) -> SessionData {
    SessionData {
        id: Some("act".into()),
        continues: Some(vec![head.into()]),
        ..Default::default()
    }
}

/// SPEC-V0473-109 AC1: heads exist but none is recent.
#[test]
fn only_stale_heads_yield_a_stale_ambiguity() {
    let all = vec![lineage("old", 45, 0, &[]), lineage("older", 90, 0, &[])];
    let heads = heads_of(&all);
    match decide(&heads, now(), &identity(), no_git) {
        Decision::Ambiguous { ranked, stale } => {
            assert!(stale);
            assert_eq!(ranked.len(), 1);
            assert_eq!(ranked[0].head.session.id.as_deref(), Some("old"));
        }
        other => panic!("expected stale Ambiguous, got {other:?}"),
    }
}

/// SPEC-V0473-309 AC7: git is consulted exactly once, for 2+ candidates.
#[test]
fn git_is_consulted_once_for_several_candidates() {
    let all = two_heads("a", "b");
    let heads = heads_of(&all);
    let calls = Cell::new(0);
    let _ = decide(&heads, now(), &identity(), || {
        calls.set(calls.get() + 1);
        git_on("a")
    });
    assert_eq!(calls.get(), 1);
}

// -------------------------------------------------------------- scoring --

fn total(s: &SessionData, id: &Identity, git: &GitFacts) -> i32 {
    score(s, id, git).total
}

fn hist(n: usize) -> Vec<String> {
    // 40 hex digits whose first seven are unique per index, so an abbreviated
    // hash identifies exactly one commit.
    (0..n)
        .map(|i| format!("{:a<40}", format!("{i:07x}")))
        .collect()
}

/// SPEC-V0473-309 AC1.
#[test]
fn branch_match_scores_three_and_detached_head_scores_zero() {
    let mut s = closed("s", 1, 0);
    s.branch = Some("main".into());
    assert_eq!(total(&s, &identity(), &git_on("main")), 3);
    assert_eq!(total(&s, &identity(), &git_on("other")), 0);
    // Detached HEAD: no branch fact, so a same-named branch gets nothing.
    assert_eq!(total(&s, &identity(), &no_git()), 0);
}

/// AC2: distance bands, including a 7-char abbreviated hash.
#[test]
fn commit_distance_bands() {
    let history = hist(200);
    let git = GitFacts {
        history: history.clone(),
        ..Default::default()
    };
    let at = |idx: usize, abbreviated: bool| {
        let mut s = closed("s", 1, 0);
        let full = &history[idx];
        s.commit = Some(if abbreviated {
            full[..7].to_string()
        } else {
            full.clone()
        });
        total(&s, &identity(), &git)
    };
    assert_eq!(at(0, false), 3);
    assert_eq!(at(0, true), 3);
    assert_eq!(at(1, false), 2);
    assert_eq!(at(20, false), 2);
    assert_eq!(at(21, false), 1);
    assert_eq!(at(100, true), 1);
    assert_eq!(at(101, false), 0);
    let mut missing = closed("s", 1, 0);
    missing.commit = Some("f".repeat(40));
    assert_eq!(total(&missing, &identity(), &git), 0);
    let mut unknown = closed("s", 1, 0);
    unknown.commit = Some("unknown".into());
    assert_eq!(total(&unknown, &identity(), &git), 0);
}

/// AC3.
#[test]
fn task_overlap_scores_two_and_ignores_oversized_related_lists() {
    let mut s = closed("s", 1, 0);
    s.related_task_ids = vec!["t5".into(), "t9".into()];
    let id = Identity {
        claimed: ["t9".to_string()].into(),
        ..Default::default()
    };
    assert_eq!(total(&s, &id, &no_git()), 2);

    // From the branch name / commit subjects (via GitFacts.task_ids).
    let git = GitFacts {
        task_ids: ["t5".to_string()].into(),
        ..Default::default()
    };
    assert_eq!(total(&s, &identity(), &git), 2);

    // Only 'in progress' tasks are not an input at all: no overlap -> 0.
    assert_eq!(total(&s, &identity(), &no_git()), 0);

    // More than 20 related ids -> 0 even with a common id.
    s.related_task_ids = (0..21).map(|i| format!("t{i}")).collect();
    assert_eq!(total(&s, &id_with("t3"), &no_git()), 0);
    s.related_task_ids.pop();
    assert_eq!(total(&s, &id_with("t3"), &no_git()), 2);
}

fn id_with(task: &str) -> Identity {
    Identity {
        claimed: [task.to_string()].into(),
        ..Default::default()
    }
}

/// AC4.
#[test]
fn context_pointer_paths_overlapping_dirty_files_score_one() {
    let mut s = closed("s", 1, 0);
    s.context_pointers = vec![serde_json::json!({ "path": "src/a.rs" })];
    let git = GitFacts {
        dirty_paths: ["src/a.rs".to_string()].into(),
        ..Default::default()
    };
    assert_eq!(total(&s, &identity(), &git), 1);
    let other = GitFacts {
        dirty_paths: ["src/b.rs".to_string()].into(),
        ..Default::default()
    };
    assert_eq!(total(&s, &identity(), &other), 0);
}

/// AC5.
#[test]
fn agent_and_worktree_each_score_one() {
    let mut s = closed("s", 1, 0);
    s.agent_id = Some("me".into());
    s.owner_v = Some(1);
    s.worktree_key = Some("wt-a".into());
    let id = Identity {
        agent_id: Some("me".into()),
        worktree_key: Some("wt-a".into()),
        ..Default::default()
    };
    let sc = score(&s, &id, &no_git());
    assert_eq!(sc.total, 2);
    // An agent_id without owner_v is not trusted.
    s.owner_v = None;
    assert_eq!(total(&s, &id, &no_git()), 1);
    // Old data has no worktree_key: that signal is 0.
    s.worktree_key = None;
    assert_eq!(total(&s, &id, &no_git()), 0);
}

// ---------------------------------------------------------- extraction --

#[test]
fn task_ids_come_from_branches_and_subjects_including_ranges() {
    let set = extract_task_ids("feat/t439.6-lineage and t12 fix");
    assert!(set.contains("t439.6"), "{set:?}");
    assert!(set.contains("t12"), "{set:?}");

    let range = extract_task_ids("work/t419-t424");
    let expect: BTreeSet<String> = (419..=424).map(|n| format!("t{n}")).collect();
    assert_eq!(range, expect);

    // A range is expanded to at most ten ids.
    let wide = extract_task_ids("t1-t500");
    assert_eq!(wide.len(), 10, "{wide:?}");

    // Words that merely contain "t" and digits are not task ids.
    let none = extract_task_ids("fix/v0.47.3 ws4-3 test2 float64");
    assert!(none.is_empty(), "{none:?}");
}

#[test]
fn porcelain_lines_are_parsed_to_repo_relative_paths() {
    let lines = vec![
        " M src/a.rs".to_string(),
        "?? new file.txt".to_string(),
        "R  old.rs -> renamed.rs".to_string(),
        "A  \"quoted name.rs\"".to_string(),
    ];
    let set = dirty_paths_from_porcelain(&lines);
    assert!(set.contains("src/a.rs"), "{set:?}");
    assert!(set.contains("new file.txt"), "{set:?}");
    assert!(set.contains("renamed.rs"), "{set:?}");
    assert!(set.contains("quoted name.rs"), "{set:?}");
    assert!(!set.contains("old.rs"), "{set:?}");
}

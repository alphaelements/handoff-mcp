//! The release guard (SPEC-ONB-041): what the pre-release check means for a
//! milestone that is about to become `released`.
//!
//! `[onboarding] release_guard` decides: `off` asks nothing, `warn` releases
//! and says what did not pass, `block` refuses unless the caller forces it.
//! This module only turns a finished check into that verdict. The check
//! itself is read by the caller (`crate::mcp::handlers::release_check`) with
//! the command items answered from the record: releasing a milestone never
//! runs a command.

use super::check::{ReleaseItem, ReleaseReport, COMMAND_ITEMS};
use crate::setup::doctor::CheckStatus;

/// `[onboarding] release_guard`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuardMode {
    /// Releasing a milestone asks nothing.
    Off,
    /// Release, and report every item that did not pass.
    Warn,
    /// Refuse to release while an item did not pass, unless forced.
    Block,
}

impl GuardMode {
    /// The mode a `release_guard` value stands for. Anything but `warn` and
    /// `block` (unset, or a value this version does not know) is `off`: a
    /// hand-written typo must not start blocking releases.
    pub fn from_setting(value: Option<&str>) -> Self {
        match value {
            Some("warn") => Self::Warn,
            Some("block") => Self::Block,
            _ => Self::Off,
        }
    }

    /// The value as it is written in config.toml.
    fn setting(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Warn => "warn",
            Self::Block => "block",
        }
    }
}

/// What the guard decides about one release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    /// The milestone may be released. `warnings` (one line each) say what
    /// did not pass; empty when everything passed or the guard is off.
    Release { warnings: Vec<String> },
    /// The milestone must stay as it is; `message` says why and how to go on.
    Refuse { message: String },
}

/// Decides whether `milestone` may be released. `check` is its pre-release
/// check with the command items answered from the record, or why that check
/// could not be carried out.
pub fn judge(
    mode: GuardMode,
    force: bool,
    milestone: &str,
    check: Result<&ReleaseReport, &str>,
) -> Verdict {
    if mode == GuardMode::Off {
        return Verdict::Release {
            warnings: Vec::new(),
        };
    }
    let refuses = mode == GuardMode::Block && !force;
    let setting = mode.setting();
    // Why the milestone is released although something did not pass.
    let released_anyway = if mode == GuardMode::Block {
        format!(
            "the milestone was released anyway (force: true overrides [onboarding] \
             release_guard = \"{setting}\")"
        )
    } else {
        format!("the milestone was released anyway ([onboarding] release_guard = \"{setting}\")")
    };

    let report = match check {
        Ok(report) => report,
        // A check that could not be carried out verified nothing: under
        // `block` that is not a reason to let the release through.
        Err(reason) => {
            let reason = first_line(reason);
            return if refuses {
                Verdict::Refuse {
                    message: format!(
                        "Milestone '{milestone}' was not released: the pre-release check could \
                         not be carried out ({reason}) and [onboarding] release_guard = \
                         \"{setting}\" requires it. Fix the cause, or pass force: true to \
                         release anyway."
                    ),
                }
            } else {
                Verdict::Release {
                    warnings: vec![format!(
                        "release check could not be carried out, so nothing was verified: \
                         {reason}; {released_anyway}."
                    )],
                }
            };
        }
    };

    let findings: Vec<&ReleaseItem> = report
        .items
        .iter()
        .filter(|item| item.status != CheckStatus::Pass)
        .collect();
    if findings.is_empty() {
        return Verdict::Release {
            warnings: Vec::new(),
        };
    }
    let lines: Vec<String> = findings.iter().map(|item| finding_line(item)).collect();
    let record_note = record_note(report, milestone);

    if refuses {
        let mut message = format!(
            "Milestone '{milestone}' was not released: {} pre-release check item(s) did not \
             pass ([onboarding] release_guard = \"{setting}\").",
            findings.len()
        );
        for line in &lines {
            message.push_str(&format!("\n- {line}"));
        }
        if let Some(note) = &record_note {
            message.push_str(&format!("\n{note}"));
        }
        message.push_str(&format!(
            "\nResolve the items and check again with handoff_release_check(milestone: \
             \"{milestone}\"), or pass force: true to release anyway."
        ));
        return Verdict::Refuse { message };
    }

    let mut warnings = vec![format!(
        "release check: {} item(s) of milestone '{milestone}' did not pass; {released_anyway}.",
        findings.len()
    )];
    warnings.extend(
        lines
            .into_iter()
            .map(|line| format!("release check: {line}")),
    );
    warnings.extend(record_note.map(|note| format!("release check: {note}")));
    Verdict::Release { warnings }
}

fn status_word(status: CheckStatus) -> &'static str {
    match status {
        CheckStatus::Pass => "pass",
        CheckStatus::Fail => "fail",
        CheckStatus::Skip => "skip",
        CheckStatus::Manual => "manual",
    }
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or_default().trim()
}

/// One item that did not pass, on one line: `<id> [<status>] <what was found>`.
/// A detail's further lines (a command's output, what to do next) stay in
/// `handoff_release_check`, which the reader is pointed to.
fn finding_line(item: &ReleaseItem) -> String {
    format!(
        "{} [{}] {}",
        item.id,
        status_word(item.status),
        first_line(&item.detail)
    )
}

/// Says which command results were read from the record and when each was
/// judged: the guard runs nothing, so what it saw may be out of date. `None`
/// when no command item had a result on record.
fn record_note(report: &ReleaseReport, milestone: &str) -> Option<String> {
    let recorded: Vec<String> = report
        .items
        .iter()
        .filter(|item| COMMAND_ITEMS.iter().any(|(id, _)| *id == item.id))
        .filter_map(|item| {
            item.checked_at.as_deref().map(|checked_at| {
                format!(
                    "{} ({}, checked {checked_at})",
                    item.id,
                    status_word(item.status)
                )
            })
        })
        .collect();
    if recorded.is_empty() {
        return None;
    }
    Some(format!(
        "command results were read from the record, not from a new run, and may be out of \
         date: {}. Run handoff_release_check(milestone: \"{milestone}\") and report fresh \
         results to update them.",
        recorded.join(", ")
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::setup::doctor::ResultSource;

    fn item(id: &str, status: CheckStatus, detail: &str) -> ReleaseItem {
        ReleaseItem {
            id: id.to_string(),
            status,
            detail: detail.to_string(),
            refs: None,
            source: None,
            checked_at: None,
        }
    }

    fn recorded(id: &str, status: CheckStatus, detail: &str, checked_at: &str) -> ReleaseItem {
        ReleaseItem {
            source: Some(ResultSource::Reported),
            checked_at: Some(checked_at.to_string()),
            ..item(id, status, detail)
        }
    }

    fn report(items: Vec<ReleaseItem>) -> ReleaseReport {
        ReleaseReport {
            milestone: "v1".to_string(),
            ok: items.iter().all(|item| item.status == CheckStatus::Pass),
            items,
            commands_to_run: Vec::new(),
        }
    }

    /// Every item passes; the command items come from the record.
    fn all_passing() -> ReleaseReport {
        report(vec![
            item("tasks_done", CheckStatus::Pass, "all 2 leaf task(s) done"),
            item("no_open_bugs", CheckStatus::Pass, "no open bug task"),
            recorded(
                "tests_pass",
                CheckStatus::Pass,
                "result on record from 2026-11-10T08:00:00+00:00: exit code 0",
                "2026-11-10T08:00:00+00:00",
            ),
            item("secrets_untracked", CheckStatus::Pass, "none"),
            ReleaseItem {
                checked_at: Some("2026-11-09T08:00:00+00:00".to_string()),
                ..item("manual:docs_updated", CheckStatus::Pass, "confirmed")
            },
        ])
    }

    /// An unfinished task, a failed smoke run on record, tests never run and
    /// a manual item nobody confirmed.
    fn with_findings() -> ReleaseReport {
        report(vec![
            ReleaseItem {
                refs: Some(vec!["t2".to_string()]),
                ..item(
                    "tasks_done",
                    CheckStatus::Fail,
                    "1 of 2 leaf task(s) of the milestone are not done or skipped: t2 (todo)",
                )
            },
            item("no_open_bugs", CheckStatus::Pass, "no open bug task"),
            item(
                "tests_pass",
                CheckStatus::Skip,
                "not verified: no result is on record for `npm test`. Run it",
            ),
            recorded(
                "smoke_pass",
                CheckStatus::Fail,
                "result on record from 2026-11-10T08:00:00+00:00: exit code 2\noutput (last 1 line(s)):\nboom",
                "2026-11-10T08:00:00+00:00",
            ),
            item(
                "secrets_untracked",
                CheckStatus::Fail,
                "Git tracks 1 file(s): `.env`.\nNext: Stop tracking each file",
            ),
            item(
                "manual:docs_updated",
                CheckStatus::Manual,
                "waiting for a person to confirm `docs_updated`.",
            ),
        ])
    }

    fn warnings(verdict: Verdict) -> Vec<String> {
        match verdict {
            Verdict::Release { warnings } => warnings,
            Verdict::Refuse { message } => panic!("refused: {message}"),
        }
    }

    fn refusal(verdict: Verdict) -> String {
        match verdict {
            Verdict::Refuse { message } => message,
            Verdict::Release { warnings } => panic!("released with {warnings:?}"),
        }
    }

    #[test]
    fn only_warn_and_block_switch_the_guard_on() {
        assert_eq!(GuardMode::from_setting(Some("warn")), GuardMode::Warn);
        assert_eq!(GuardMode::from_setting(Some("block")), GuardMode::Block);
        for off in [None, Some("off"), Some(""), Some("strict"), Some("BLOCK")] {
            assert_eq!(GuardMode::from_setting(off), GuardMode::Off, "{off:?}");
        }
    }

    /// Every value config.toml accepts is a mode of its own, spelled the way
    /// the messages quote it.
    #[test]
    fn every_accepted_setting_has_its_own_mode() {
        use crate::storage::config::RELEASE_GUARD_MODES;
        let modes: Vec<GuardMode> = RELEASE_GUARD_MODES
            .iter()
            .map(|value| GuardMode::from_setting(Some(value)))
            .collect();
        assert_eq!(
            modes,
            vec![GuardMode::Off, GuardMode::Warn, GuardMode::Block]
        );
        for (mode, value) in modes.iter().zip(RELEASE_GUARD_MODES) {
            assert_eq!(mode.setting(), *value);
        }
    }

    #[test]
    fn off_releases_without_a_word_whatever_the_check_says() {
        let findings = with_findings();
        for check in [Ok(&findings), Err("the record is broken")] {
            for force in [false, true] {
                assert_eq!(
                    judge(GuardMode::Off, force, "v1", check),
                    Verdict::Release {
                        warnings: Vec::new()
                    }
                );
            }
        }
    }

    // SPEC-ONB-041 AC4
    #[test]
    fn a_check_in_which_everything_passes_releases_without_warnings() {
        let passing = all_passing();
        for mode in [GuardMode::Warn, GuardMode::Block] {
            for force in [false, true] {
                assert_eq!(
                    judge(mode, force, "v1", Ok(&passing)),
                    Verdict::Release {
                        warnings: Vec::new()
                    },
                    "{mode:?} force={force}"
                );
            }
        }
    }

    // SPEC-ONB-041 AC1, AC6
    #[test]
    fn warn_releases_and_lists_every_item_that_did_not_pass() {
        let findings = with_findings();
        let warnings = warnings(judge(GuardMode::Warn, false, "v1", Ok(&findings)));
        assert_eq!(
            warnings,
            vec![
                "release check: 5 item(s) of milestone 'v1' did not pass; the milestone was \
                 released anyway ([onboarding] release_guard = \"warn\")."
                    .to_string(),
                "release check: tasks_done [fail] 1 of 2 leaf task(s) of the milestone are not \
                 done or skipped: t2 (todo)"
                    .to_string(),
                "release check: tests_pass [skip] not verified: no result is on record for \
                 `npm test`. Run it"
                    .to_string(),
                "release check: smoke_pass [fail] result on record from \
                 2026-11-10T08:00:00+00:00: exit code 2"
                    .to_string(),
                "release check: secrets_untracked [fail] Git tracks 1 file(s): `.env`.".to_string(),
                "release check: manual:docs_updated [manual] waiting for a person to confirm \
                 `docs_updated`."
                    .to_string(),
                "release check: command results were read from the record, not from a new \
                 run, and may be out of date: smoke_pass (fail, checked \
                 2026-11-10T08:00:00+00:00). Run handoff_release_check(milestone: \"v1\") and \
                 report fresh results to update them."
                    .to_string(),
            ]
        );
        for line in &warnings {
            assert!(!line.contains('\n'), "one line per warning: {line:?}");
        }
    }

    /// The date of every record that was read is given, also for the command
    /// items that passed: they are the ones that may have gone stale.
    #[test]
    fn the_time_of_every_record_that_was_read_is_given() {
        let mut check = all_passing();
        check.items[0] = item(
            "tasks_done",
            CheckStatus::Fail,
            "1 of 2 unfinished: t2 (todo)",
        );
        check.items.push(recorded(
            "dependencies_audited",
            CheckStatus::Pass,
            "result on record from 2026-11-11T09:30:00+00:00: exit code 0",
            "2026-11-11T09:30:00+00:00",
        ));
        let warnings = warnings(judge(GuardMode::Warn, false, "v1", Ok(&check)));
        assert_eq!(warnings.len(), 3, "{warnings:?}");
        let note = &warnings[2];
        assert!(
            note.contains("tests_pass (pass, checked 2026-11-10T08:00:00+00:00)")
                && note.contains("dependencies_audited (pass, checked 2026-11-11T09:30:00+00:00)"),
            "{note}"
        );
        assert!(
            !note.contains("manual:docs_updated"),
            "a confirmation is not a command result: {note}"
        );

        // Nothing on record: nothing to date.
        let unrecorded = report(vec![item(
            "tests_pass",
            CheckStatus::Skip,
            "not verified: no result is on record",
        )]);
        let warnings = warnings_of(GuardMode::Warn, &unrecorded);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(warnings
            .iter()
            .all(|line| !line.contains("from the record")));
    }

    fn warnings_of(mode: GuardMode, check: &ReleaseReport) -> Vec<String> {
        warnings(judge(mode, false, "v1", Ok(check)))
    }

    // SPEC-ONB-041 AC2
    #[test]
    fn block_refuses_and_says_what_did_not_pass_and_how_to_go_on() {
        let findings = with_findings();
        let message = refusal(judge(GuardMode::Block, false, "v1", Ok(&findings)));
        assert_eq!(
            message,
            "Milestone 'v1' was not released: 5 pre-release check item(s) did not pass \
             ([onboarding] release_guard = \"block\").\n\
             - tasks_done [fail] 1 of 2 leaf task(s) of the milestone are not done or skipped: \
             t2 (todo)\n\
             - tests_pass [skip] not verified: no result is on record for `npm test`. Run it\n\
             - smoke_pass [fail] result on record from 2026-11-10T08:00:00+00:00: exit code 2\n\
             - secrets_untracked [fail] Git tracks 1 file(s): `.env`.\n\
             - manual:docs_updated [manual] waiting for a person to confirm `docs_updated`.\n\
             command results were read from the record, not from a new run, and may be out of \
             date: smoke_pass (fail, checked 2026-11-10T08:00:00+00:00). Run \
             handoff_release_check(milestone: \"v1\") and report fresh results to update them.\n\
             Resolve the items and check again with handoff_release_check(milestone: \"v1\"), \
             or pass force: true to release anyway."
        );
    }

    // SPEC-ONB-041 AC3
    #[test]
    fn block_with_force_releases_and_still_says_what_did_not_pass() {
        let findings = with_findings();
        let forced = warnings(judge(GuardMode::Block, true, "v1", Ok(&findings)));
        assert_eq!(
            forced[0],
            "release check: 5 item(s) of milestone 'v1' did not pass; the milestone was \
             released anyway (force: true overrides [onboarding] release_guard = \"block\")."
        );
        // The findings themselves read as they do under `warn`.
        let warned = warnings(judge(GuardMode::Warn, true, "v1", Ok(&findings)));
        assert_eq!(forced[1..], warned[1..]);
        assert!(
            warned[0].contains("release_guard = \"warn\""),
            "force does not silence a warning: {}",
            warned[0]
        );
    }

    /// A single skipped item is a finding: an unverified release is not a
    /// verified one.
    #[test]
    fn a_single_item_that_is_not_pass_is_enough() {
        for status in [CheckStatus::Fail, CheckStatus::Skip, CheckStatus::Manual] {
            let mut check = all_passing();
            check.items[1] = item("no_open_bugs", status, "detail");
            let message = refusal(judge(GuardMode::Block, false, "v1", Ok(&check)));
            assert!(message.contains("1 pre-release check item(s)"), "{message}");
            assert!(
                message.contains(&format!("- no_open_bugs [{}] detail", status_word(status))),
                "{message}"
            );
        }
    }

    #[test]
    fn a_check_that_could_not_be_carried_out_is_never_a_silent_release() {
        let reason = "the release record /p/.handoff/release/v1.json is not valid\ncaused by: x";
        let warned = warnings(judge(GuardMode::Warn, false, "v1", Err(reason)));
        assert_eq!(
            warned,
            vec![
                "release check could not be carried out, so nothing was verified: the release \
                 record /p/.handoff/release/v1.json is not valid; the milestone was released \
                 anyway ([onboarding] release_guard = \"warn\")."
                    .to_string()
            ]
        );

        let message = refusal(judge(GuardMode::Block, false, "v1", Err(reason)));
        assert!(
            message.starts_with("Milestone 'v1' was not released: the pre-release check could not be carried out (the release record /p/.handoff/release/v1.json is not valid)"),
            "{message}"
        );
        assert!(message.contains("force: true"), "{message}");

        let forced = warnings(judge(GuardMode::Block, true, "v1", Err(reason)));
        assert_eq!(forced.len(), 1);
        assert!(forced[0].contains("force: true overrides"), "{}", forced[0]);
    }
}

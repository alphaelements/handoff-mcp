//! The "Orphan Tasks" section of the periodic reports (REQ-TRC-010).
//!
//! The daily, weekly, monthly, quarterly and yearly reports end with the
//! tasks that sit outside requirement traceability: how many there are and,
//! per task, its id, title and status. Which tasks those are is the shared
//! predicate of `crate::trace::orphan` (the one the `task_orphan` lint rule
//! and the orphan guard use), so the section lists nothing in a project with
//! no layer document, and never a `no-trace` or skipped task or a parent
//! traced through its children.
//!
//! The section is a state of the project at generation time, not of the
//! report's period: an orphan is listed whenever it exists, whatever happened
//! to it during the period.
//!
//! With no orphan task [`section_data`] returns `None`, the `data` key stays
//! absent, and the `orphan_tasks` partial renders nothing: such a report is
//! byte for byte what it was before the section existed.

use anyhow::Result;
use serde_json::{json, Value};

use super::ReportType;
use crate::trace::orphan::OrphanTask;

/// Key of the report `data` the section is rendered from:
/// `{count, tasks: [{id, title, status}]}`.
pub const DATA_KEY: &str = "orphan_tasks";

/// Name of the built-in partial that renders the section. A project template
/// includes it with `{{> orphan_tasks}}`.
pub const PARTIAL: &str = "orphan_tasks";

/// The report types whose built-in template ends with the section: the
/// periodic progress reports. The other types are about one subject (a
/// verification campaign, a milestone, the time log, the bug list, the
/// completion sign-off) and stay as they are.
pub const REPORT_TYPES: [ReportType; 5] = [
    ReportType::Daily,
    ReportType::Weekly,
    ReportType::Monthly,
    ReportType::Quarterly,
    ReportType::Yearly,
];

/// Whether a report of `report_type` carries the section.
pub fn applies_to(report_type: ReportType) -> bool {
    REPORT_TYPES.contains(&report_type)
}

/// The value of the [`DATA_KEY`] key for `orphans` (kept in the order given,
/// which the shared predicate makes the natural task-id order). `None` when
/// there is no orphan: the key must then stay absent.
pub fn section_data(orphans: &[OrphanTask]) -> Option<Value> {
    if orphans.is_empty() {
        return None;
    }
    let tasks: Vec<Value> = orphans
        .iter()
        .map(|o| json!({ "id": o.id, "title": o.title, "status": o.status }))
        .collect();
    Some(json!({ "count": orphans.len(), "tasks": tasks }))
}

/// Whether generating a report of `report_type` from `data` has to work the
/// section out: the type carries it, and the caller did not pass the
/// [`DATA_KEY`] key (a key the caller passes is left alone, like `charts`,
/// so a caller can supply the list or suppress it with `null`).
pub fn needs_collection(report_type: ReportType, data: &Value) -> bool {
    applies_to(report_type) && data.get(DATA_KEY).is_none()
}

/// Puts the outcome of the orphan check into the report `data`: the section
/// data when there are orphans, nothing when there are none, and a note in
/// `data.warnings` when the check itself failed, so a report never passes off
/// "could not check" as "no orphan task".
pub fn apply(data: &mut Value, orphans: Result<Vec<OrphanTask>>) -> Result<()> {
    match orphans {
        Ok(orphans) => {
            if let Some(section) = section_data(&orphans) {
                data[DATA_KEY] = section;
            }
            Ok(())
        }
        Err(e) => {
            super::push_data_warning(data, format!("Orphan tasks could not be listed: {e:#}"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn orphan(id: &str, title: &str, status: &str) -> OrphanTask {
        OrphanTask {
            id: id.to_string(),
            title: title.to_string(),
            status: status.to_string(),
        }
    }

    #[test]
    fn no_orphan_means_no_section_data() {
        assert_eq!(section_data(&[]), None);
    }

    #[test]
    fn section_data_carries_the_count_and_each_task_in_the_order_given() {
        let orphans = [
            orphan("t2", "Second", "todo"),
            orphan("t2.10", "Nested", "in_progress"),
            orphan("t10", "Tenth", "done"),
        ];
        assert_eq!(
            section_data(&orphans),
            Some(json!({
                "count": 3,
                "tasks": [
                    { "id": "t2", "title": "Second", "status": "todo" },
                    { "id": "t2.10", "title": "Nested", "status": "in_progress" },
                    { "id": "t10", "title": "Tenth", "status": "done" },
                ],
            }))
        );
    }

    #[test]
    fn collection_is_needed_for_a_periodic_report_unless_the_caller_passed_the_key() {
        for t in ReportType::ALL {
            assert_eq!(
                needs_collection(t, &json!({})),
                applies_to(t),
                "{}",
                t.as_str()
            );
            assert_eq!(
                needs_collection(t, &json!({ "summary": "s" })),
                applies_to(t),
                "{}",
                t.as_str()
            );
            for passed in [
                json!(null),
                json!({ "count": 0, "tasks": [] }),
                json!(false),
            ] {
                assert!(
                    !needs_collection(t, &json!({ "orphan_tasks": passed })),
                    "{}",
                    t.as_str()
                );
            }
        }
    }

    #[test]
    fn apply_sets_the_key_only_when_there_are_orphans() {
        let mut data = json!({ "summary": "s" });
        apply(&mut data, Ok(Vec::new())).unwrap();
        assert_eq!(data, json!({ "summary": "s" }));

        apply(&mut data, Ok(vec![orphan("t1", "One", "review")])).unwrap();
        assert_eq!(
            data,
            json!({
                "summary": "s",
                "orphan_tasks": {
                    "count": 1,
                    "tasks": [{ "id": "t1", "title": "One", "status": "review" }],
                },
            })
        );
    }

    #[test]
    fn a_failed_check_becomes_a_warning_of_the_report_not_an_empty_section() {
        let failed = || Err(anyhow::anyhow!("docs unreadable").context("reading documents"));

        let mut data = json!({});
        apply(&mut data, failed()).unwrap();
        assert_eq!(
            data,
            json!({ "warnings": [
                "Orphan tasks could not be listed: reading documents: docs unreadable"
            ] })
        );

        // Existing warnings are kept, in order.
        let mut data = json!({ "warnings": ["first"] });
        apply(&mut data, failed()).unwrap();
        assert_eq!(data["warnings"].as_array().unwrap().len(), 2);
        assert_eq!(data["warnings"][0], "first");
        assert!(data.get(DATA_KEY).is_none());

        let mut data = json!({ "warnings": null });
        apply(&mut data, failed()).unwrap();
        assert_eq!(data["warnings"].as_array().unwrap().len(), 1);

        // A `warnings` that is not a list is the caller's mistake, reported
        // rather than overwritten.
        let mut data = json!({ "warnings": "text" });
        let err = apply(&mut data, failed()).unwrap_err();
        assert!(
            err.to_string().contains("'data.warnings' must be an array"),
            "{err}"
        );
    }

    #[test]
    fn only_the_periodic_report_types_carry_the_section() {
        let with: Vec<&str> = ReportType::ALL
            .into_iter()
            .filter(|t| applies_to(*t))
            .map(ReportType::as_str)
            .collect();
        assert_eq!(with, ["weekly", "monthly", "daily", "quarterly", "yearly"]);
    }
}

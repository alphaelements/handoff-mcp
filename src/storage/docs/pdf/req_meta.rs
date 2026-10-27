//! Requirement meta blocks for the PDF export (REQ-PDF-030).
//!
//! A requirement document keeps each item's attributes (`- priority:`,
//! `- refines:`, ...) as bullet lines under its heading. A plain export drops
//! them (REQ-PDF-005 AC2); an *enriched* export turns them into a visual block
//! under the heading instead: priority and dev-stage badges, horizontal and
//! vertical coverage bars, the titled `refines` / `verifies` targets, the
//! implementing tasks, and the acceptance criteria marked checked or not.
//!
//! This module holds the pure data ([`ReqMeta`], filled in by the tool layer
//! from the trace graph) and its rendering as one call of the template's
//! `req-meta()` function. Finding the items in a document and removing the
//! attribute lines is the Markdown converter's job
//! (`markdown_to_typst::ConvertOptions::req_meta`).

use std::collections::{BTreeSet, HashMap};
use std::fmt::Write;

use super::template::typst_string;

/// Name of the Typst function (`templates/parts/req-meta.typ`) that draws a
/// meta block.
pub const REQ_META_FUNCTION: &str = "req-meta";

/// Item stable id -> its meta, for every item of the exported document(s).
pub type ReqMetaIndex = HashMap<String, ReqMeta>;

/// Everything the meta block of one requirement shows, except its acceptance
/// criteria text (that is written in the document body).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReqMeta {
    /// `P0`..`P3`, as authored.
    pub priority: Option<String>,
    /// `not_started` | `in_progress` | `implemented` | `tested` | `verified`.
    pub dev_stage: Option<String>,
    /// Horizontal coverage: is the item verified by a right-side item.
    pub horizontal: Option<Coverage>,
    /// Vertical coverage: is the item refined down to the layers below.
    pub vertical: Option<Coverage>,
    /// Upper items this one refines.
    pub refines: Vec<LinkRef>,
    /// Items this one verifies.
    pub verifies: Vec<LinkRef>,
    /// Tasks implementing the item.
    pub tasks: Vec<TaskRef>,
    /// Labels (`AC1`, ...) of the acceptance criteria a passing verification
    /// item covers.
    pub verified_acs: BTreeSet<String>,
    /// A passing verification covers the item as a whole, so every one of its
    /// acceptance criteria counts as checked.
    pub all_acs_verified: bool,
}

/// Outcome of one coverage axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CoverageState {
    Covered,
    Partial,
    Uncovered,
    Waived,
}

impl CoverageState {
    /// The word the template switches on.
    fn as_str(self) -> &'static str {
        match self {
            Self::Covered => "covered",
            Self::Partial => "partial",
            Self::Uncovered => "uncovered",
            Self::Waived => "waived",
        }
    }
}

/// One coverage axis of an item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Coverage {
    pub state: CoverageState,
    /// `(done, total)` acceptance criteria when the state is `Partial` and the
    /// item declares some; the bar then shows the real ratio instead of a
    /// half-filled one.
    pub done: Option<(usize, usize)>,
}

/// A `refines` / `verifies` target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinkRef {
    /// As authored (`REQ-003`, or `REQ-003#AC2` for one criterion).
    pub id: String,
    /// Title of the target item when it resolves.
    pub title: Option<String>,
}

/// A task implementing the item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRef {
    pub id: String,
    pub title: Option<String>,
    pub status: Option<String>,
}

/// One acceptance criterion found in the document body.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptanceItem {
    /// `AC1`, ...; `None` for a bullet without a label.
    pub label: Option<String>,
    pub text: String,
    /// The bullet carried a `[x]` task-list mark.
    pub marked_done: bool,
}

/// Parses one acceptance bullet (marker already stripped): an optional
/// `[x]` / `[ ]` task-list mark, then an optional `AC<n>:` or `AC<n>(kind):`
/// label, then the text.
pub fn parse_acceptance_bullet(raw: &str) -> AcceptanceItem {
    let mut rest = raw.trim();
    let mut marked_done = false;
    for (mark, done) in [("[x]", true), ("[X]", true), ("[ ]", false)] {
        if let Some(after) = rest.strip_prefix(mark) {
            marked_done = done;
            rest = after.trim_start();
            break;
        }
    }
    let (label, text) = split_label(rest);
    AcceptanceItem {
        label,
        text: text.to_string(),
        marked_done,
    }
}

/// `("AC12", "text")` for `AC12: text` / `AC12(gwt): text`; `(None, whole)`
/// otherwise.
fn split_label(raw: &str) -> (Option<String>, &str) {
    let bytes = raw.as_bytes();
    if bytes.len() < 3 || !raw.is_char_boundary(2) || !raw[..2].eq_ignore_ascii_case("ac") {
        return (None, raw);
    }
    let digits = raw[2..].chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return (None, raw);
    }
    let mut after = &raw[2 + digits..];
    if let Some(inner) = after.strip_prefix('(') {
        match inner.split_once(')') {
            Some((_, tail)) => after = tail,
            None => return (None, raw),
        }
    }
    match after.strip_prefix(':').or_else(|| after.strip_prefix('：')) {
        Some(text) => (Some(format!("AC{}", &raw[2..2 + digits])), text.trim()),
        None => (None, raw),
    }
}

impl ReqMeta {
    /// Whether `item` is shown as checked: it carries a `[x]` mark, or a
    /// passing verification covers it.
    fn is_checked(&self, item: &AcceptanceItem) -> bool {
        item.marked_done
            || self.all_acs_verified
            || item
                .label
                .as_ref()
                .is_some_and(|label| self.verified_acs.contains(label))
    }

    /// The `#req-meta(...)` call for this item with the criteria `acceptance`
    /// found in its body, or `None` when there is nothing to show.
    pub fn render_call(&self, acceptance: &[AcceptanceItem]) -> Option<String> {
        let mut args: Vec<String> = Vec::new();
        if let Some(p) = &self.priority {
            args.push(format!("priority: {}", typst_string(p)));
        }
        if let Some(s) = &self.dev_stage {
            args.push(format!("dev-stage: {}", typst_string(s)));
        }
        for (key, coverage) in [
            ("horizontal", &self.horizontal),
            ("vertical", &self.vertical),
        ] {
            if let Some(c) = coverage {
                args.push(format!("{key}: {}", coverage_dict(c)));
            }
        }
        if !self.refines.is_empty() {
            args.push(format!("refines: {}", links_array(&self.refines)));
        }
        if !self.verifies.is_empty() {
            args.push(format!("verifies: {}", links_array(&self.verifies)));
        }
        if !self.tasks.is_empty() {
            args.push(format!("tasks: {}", tasks_array(&self.tasks)));
        }
        if !acceptance.is_empty() {
            args.push(format!("acceptance: {}", self.acceptance_array(acceptance)));
        }
        if args.is_empty() {
            return None;
        }
        Some(format!("#{REQ_META_FUNCTION}({})", args.join(", ")))
    }

    fn acceptance_array(&self, acceptance: &[AcceptanceItem]) -> String {
        let items: Vec<String> = acceptance
            .iter()
            .map(|ac| {
                let mut dict = String::from("(");
                if let Some(label) = &ac.label {
                    let _ = write!(dict, "label: {}, ", typst_string(label));
                }
                let _ = write!(
                    dict,
                    "text: {}, checked: {})",
                    typst_string(&ac.text),
                    self.is_checked(ac)
                );
                dict
            })
            .collect();
        array_literal(&items)
    }
}

/// A Typst array literal; the trailing comma keeps a one-element array an
/// array.
fn array_literal(items: &[String]) -> String {
    let mut out = String::from("(");
    for item in items {
        out.push_str(item);
        out.push_str(", ");
    }
    out.push(')');
    out
}

fn coverage_dict(c: &Coverage) -> String {
    let mut out = format!("(state: {}", typst_string(c.state.as_str()));
    if let Some((done, total)) = c.done {
        let _ = write!(out, ", done: ({done}, {total})");
    }
    out.push(')');
    out
}

fn optional_field(out: &mut String, key: &str, value: &Option<String>) {
    if let Some(v) = value {
        let _ = write!(out, ", {key}: {}", typst_string(v));
    }
}

fn links_array(links: &[LinkRef]) -> String {
    let items: Vec<String> = links
        .iter()
        .map(|l| {
            let mut dict = format!("(id: {}", typst_string(&l.id));
            optional_field(&mut dict, "title", &l.title);
            dict.push(')');
            dict
        })
        .collect();
    array_literal(&items)
}

fn tasks_array(tasks: &[TaskRef]) -> String {
    let items: Vec<String> = tasks
        .iter()
        .map(|t| {
            let mut dict = format!("(id: {}", typst_string(&t.id));
            optional_field(&mut dict, "title", &t.title);
            optional_field(&mut dict, "status", &t.status);
            dict.push(')');
            dict
        })
        .collect();
    array_literal(&items)
}

/// `call` as a ` ```typst ` passthrough block (REQ-PDF-010), fenced with more
/// backticks than any run inside it so no title can close the block early.
pub fn passthrough_block(call: &str) -> String {
    let mut longest = 0;
    let mut run = 0;
    for c in call.chars() {
        if c == '`' {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    let fence = "`".repeat((longest + 1).max(3));
    format!("{fence}typst\n{call}\n{fence}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ac(label: Option<&str>, text: &str, marked_done: bool) -> AcceptanceItem {
        AcceptanceItem {
            label: label.map(str::to_string),
            text: text.to_string(),
            marked_done,
        }
    }

    // -- bullet parsing ----------------------------------------------------

    #[test]
    fn bullet_with_label_and_kind_is_split() {
        let item = parse_acceptance_bullet("AC1(text): 表示される");
        assert_eq!(item, ac(Some("AC1"), "表示される", false));
    }

    #[test]
    fn bullet_with_plain_label_is_split() {
        let item = parse_acceptance_bullet("ac12: works");
        assert_eq!(item, ac(Some("AC12"), "works", false));
    }

    #[test]
    fn bullet_without_label_keeps_its_text() {
        let item = parse_acceptance_bullet("just a sentence");
        assert_eq!(item, ac(None, "just a sentence", false));
    }

    #[test]
    fn text_that_merely_starts_with_ac_is_not_a_label() {
        for raw in [
            "AC: no digits",
            "ACK received",
            "AC1 without colon",
            "AC1(open: x",
        ] {
            assert_eq!(parse_acceptance_bullet(raw).label, None, "{raw}");
        }
    }

    #[test]
    fn task_list_marks_are_recognised_and_removed() {
        assert_eq!(
            parse_acceptance_bullet("[x] AC2: done"),
            ac(Some("AC2"), "done", true)
        );
        assert_eq!(
            parse_acceptance_bullet("[X] shipped"),
            ac(None, "shipped", true)
        );
        assert_eq!(
            parse_acceptance_bullet("[ ] AC3: todo"),
            ac(Some("AC3"), "todo", false)
        );
    }

    // -- rendering ---------------------------------------------------------

    #[test]
    fn empty_meta_without_criteria_renders_nothing() {
        assert_eq!(ReqMeta::default().render_call(&[]), None);
    }

    #[test]
    fn badges_and_coverage_are_rendered_as_named_arguments() {
        let meta = ReqMeta {
            priority: Some("P0".into()),
            dev_stage: Some("in_progress".into()),
            horizontal: Some(Coverage {
                state: CoverageState::Partial,
                done: Some((1, 3)),
            }),
            vertical: Some(Coverage {
                state: CoverageState::Uncovered,
                done: None,
            }),
            ..ReqMeta::default()
        };
        assert_eq!(
            meta.render_call(&[]).unwrap(),
            "#req-meta(priority: \"P0\", dev-stage: \"in_progress\", \
             horizontal: (state: \"partial\", done: (1, 3)), \
             vertical: (state: \"uncovered\"))"
        );
    }

    #[test]
    fn links_and_tasks_are_one_element_arrays_with_a_trailing_comma() {
        let meta = ReqMeta {
            refines: vec![LinkRef {
                id: "REQ-1".into(),
                title: Some("Parent".into()),
            }],
            verifies: vec![LinkRef {
                id: "REQ-2#AC1".into(),
                title: None,
            }],
            tasks: vec![TaskRef {
                id: "t1".into(),
                title: Some("Do it".into()),
                status: Some("done".into()),
            }],
            ..ReqMeta::default()
        };
        assert_eq!(
            meta.render_call(&[]).unwrap(),
            "#req-meta(refines: ((id: \"REQ-1\", title: \"Parent\"), ), \
             verifies: ((id: \"REQ-2#AC1\"), ), \
             tasks: ((id: \"t1\", title: \"Do it\", status: \"done\"), ))"
        );
    }

    #[test]
    fn a_criterion_is_checked_by_mark_by_label_or_by_whole_item_verification() {
        let mut meta = ReqMeta::default();
        meta.verified_acs.insert("AC2".into());
        let items = [
            ac(Some("AC1"), "open", false),
            ac(Some("AC2"), "verified", false),
            ac(None, "marked", true),
        ];
        let call = meta.render_call(&items).unwrap();
        assert!(
            call.contains("(label: \"AC1\", text: \"open\", checked: false)"),
            "{call}"
        );
        assert!(
            call.contains("(label: \"AC2\", text: \"verified\", checked: true)"),
            "{call}"
        );
        assert!(call.contains("(text: \"marked\", checked: true)"), "{call}");

        meta.all_acs_verified = true;
        let call = meta.render_call(&items).unwrap();
        assert!(!call.contains("checked: false"), "{call}");
    }

    #[test]
    fn criteria_alone_are_enough_for_a_block() {
        let call = ReqMeta::default()
            .render_call(&[ac(None, "x", false)])
            .unwrap();
        assert_eq!(
            call,
            "#req-meta(acceptance: ((text: \"x\", checked: false), ))"
        );
    }

    #[test]
    fn user_text_cannot_break_out_of_its_string_literal() {
        let meta = ReqMeta {
            tasks: vec![TaskRef {
                id: "t\"1".into(),
                title: Some("a\\\") #panic() (\"".into()),
                status: None,
            }],
            ..ReqMeta::default()
        };
        let call = meta.render_call(&[]).unwrap();
        assert!(call.contains("id: \"t\\\"1\""), "{call}");
        assert!(
            call.contains("title: \"a\\\\\\\") #panic() (\\\"\""),
            "{call}"
        );
    }

    // -- passthrough fence -------------------------------------------------

    #[test]
    fn passthrough_block_uses_a_three_backtick_fence_normally() {
        assert_eq!(passthrough_block("#x()"), "```typst\n#x()\n```\n");
    }

    #[test]
    fn passthrough_fence_is_longer_than_any_backtick_run_inside() {
        let block = passthrough_block("#x(\"a ```` b\")");
        assert!(block.starts_with("`````typst\n"), "{block}");
        assert!(block.ends_with("\n`````\n"), "{block}");
    }
}

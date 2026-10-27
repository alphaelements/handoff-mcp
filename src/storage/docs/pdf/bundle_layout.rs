//! Layout options of a bundle PDF (REQ-PDF-024): how the documents are
//! separated and what the running header / footer of the body pages show.
//!
//! Both are rendered as Typst markup that goes into the generated `.typ`, not
//! into the template, so they work with every page template (including a
//! customised one). They apply to bundles only: a single document keeps the
//! template's own page set-up.

use super::template::typst_string;

/// Label put on the running header block of the body pages, so the generated
/// document can be inspected with `typst query` (the header is otherwise only
/// visible in the rendered pages).
pub const HEADER_LABEL: &str = "handoff-running-header";

/// Label put on the footer block (the page number), see [`HEADER_LABEL`].
pub const FOOTER_LABEL: &str = "handoff-running-footer";

/// Date format of the header when the caller supplies no date: today.
const TODAY_FORMAT: &str = "[year]-[month]-[day]";

/// How consecutive documents of a bundle are separated
/// (`page_break_style`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum PageBreakStyle {
    /// Every document starts on a new page (the default).
    #[default]
    Break,
    /// Every document is introduced by a divider page that carries only its
    /// title; the content starts on the page after it.
    Section,
    /// No page break: a document follows the previous one on the same page.
    /// The first document still starts on its own page after the front
    /// matter (cover, table of contents).
    Continuous,
}

impl PageBreakStyle {
    /// Accepted `page_break_style` values, in documentation order.
    pub const NAMES: [&'static str; 3] = ["break", "section", "none"];

    /// Parses a `page_break_style` value (exact, lower-case).
    pub fn parse(name: &str) -> Option<Self> {
        match name {
            "break" => Some(Self::Break),
            "section" => Some(Self::Section),
            "none" => Some(Self::Continuous),
            _ => None,
        }
    }
}

/// What the running header / footer of a bundle's body pages show
/// (`header`). A field the caller leaves out is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HeaderOptions {
    /// The title of the document the page belongs to, at the header's left.
    pub show_title: bool,
    /// The page number, centred in the footer.
    pub show_page_number: bool,
    /// The date, at the header's right.
    pub show_date: bool,
}

impl Default for HeaderOptions {
    fn default() -> Self {
        Self {
            show_title: true,
            show_page_number: true,
            show_date: true,
        }
    }
}

/// Typst markup that starts a bundled document under `title` as a level-1
/// heading, separated from the previous content as `style` says. The title
/// goes through a Typst string literal, so no character in it can be read as
/// markup.
pub fn section_heading(title: &str, style: PageBreakStyle) -> String {
    let title = typst_string(title);
    match style {
        // A weak break: no second page when one is already pending, e.g.
        // after the table of contents.
        PageBreakStyle::Break => {
            format!("#pagebreak(weak: true)\n#heading(level: 1)[#{title}]\n\n")
        }
        PageBreakStyle::Section => format!(
            "#pagebreak(weak: true)\n\
             #page(header: none, footer: none)[#align(center + horizon)[#heading(level: 1)[#{title}]]]\n\n"
        ),
        PageBreakStyle::Continuous => format!("#heading(level: 1)[#{title}]\n\n"),
    }
}

/// `#set page(header: ..., footer: ...)` for the body pages: the title of the
/// document the page belongs to (the first level-1 heading on the page, else
/// the last one before it, else `fallback_title`), the date, and the page
/// number, per `options`. `date` is shown as is; `None` shows today's date.
pub fn running_page_rule(
    options: HeaderOptions,
    fallback_title: &str,
    date: Option<&str>,
) -> String {
    let header = if options.show_title || options.show_date {
        let title = if options.show_title {
            format!(
                "let heads = query(heading.where(level: 1, outlined: true))\n    \
                 let on-page = heads.filter(h => h.location().page() == here().page())\n    \
                 let before = heads.filter(h => h.location().page() < here().page())\n    \
                 let current = if on-page.len() > 0 {{ on-page.first() }} else if before.len() > 0 \
                 {{ before.last() }} else {{ none }}\n    \
                 let title = if current == none {{ {} }} else {{ current.body }}\n",
                typst_string(fallback_title)
            )
        } else {
            "let title = none\n".to_string()
        };
        let date = if options.show_date {
            match date {
                Some(d) => typst_string(d),
                None => format!("datetime.today().display({})", typst_string(TODAY_FORMAT)),
            }
        } else {
            "none".to_string()
        };
        format!(
            "context {{\n    set text(size: 9pt, fill: luma(90))\n    {title}    \
             [#block(width: 100%)[#grid(columns: (1fr, auto), align: (left, right), [#title], [#{date}])\n    \
             #line(length: 100%, stroke: 0.5pt + luma(160))] <{HEADER_LABEL}>]\n  }}"
        )
    } else {
        "none".to_string()
    };
    let footer = if options.show_page_number {
        format!(
            "[#block(width: 100%, align(center, context counter(page).display(\"1\"))) \
             <{FOOTER_LABEL}>]"
        )
    } else {
        "none".to_string()
    };
    if header == "none" && footer == "none" {
        return "#set page(header: none, footer: none)\n\n".to_string();
    }
    format!("#set page(\n  header: {header},\n  footer: {footer},\n)\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn page_break_style_parses_exactly_the_documented_names() {
        for (name, want) in [
            ("break", PageBreakStyle::Break),
            ("section", PageBreakStyle::Section),
            ("none", PageBreakStyle::Continuous),
        ] {
            assert_eq!(PageBreakStyle::parse(name), Some(want), "{name}");
            assert!(PageBreakStyle::NAMES.contains(&name));
        }
        for bad in ["", "Break", "page", "NONE", " none"] {
            assert_eq!(PageBreakStyle::parse(bad), None, "{bad:?}");
        }
        assert_eq!(PageBreakStyle::default(), PageBreakStyle::Break);
    }

    #[test]
    fn break_style_is_a_weak_pagebreak_then_the_heading() {
        let h = section_heading("Alpha", PageBreakStyle::Break);
        assert_eq!(
            h,
            "#pagebreak(weak: true)\n#heading(level: 1)[#\"Alpha\"]\n\n"
        );
    }

    #[test]
    fn section_style_puts_the_heading_alone_on_a_divider_page() {
        let h = section_heading("Alpha", PageBreakStyle::Section);
        assert!(h.starts_with("#pagebreak(weak: true)\n#page("), "{h}");
        assert!(
            h.contains("header: none") && h.contains("footer: none"),
            "{h}"
        );
        assert!(h.contains("#heading(level: 1)[#\"Alpha\"]"), "{h}");
    }

    #[test]
    fn none_style_has_no_page_break() {
        let h = section_heading("Alpha", PageBreakStyle::Continuous);
        assert!(!h.contains("pagebreak") && !h.contains("#page"), "{h}");
        assert_eq!(h, "#heading(level: 1)[#\"Alpha\"]\n\n");
    }

    #[test]
    fn section_heading_cannot_be_broken_out_of_by_the_title() {
        for style in [
            PageBreakStyle::Break,
            PageBreakStyle::Section,
            PageBreakStyle::Continuous,
        ] {
            let h = section_heading("a]\n#panic()\n[b", style);
            assert!(h.contains("\\n"), "{style:?}: {h}");
            assert!(!h.contains("a]\n"), "{style:?}: {h}");
        }
    }

    #[test]
    fn default_header_shows_everything() {
        let o = HeaderOptions::default();
        assert!(o.show_title && o.show_page_number && o.show_date);
    }

    #[test]
    fn full_rule_sets_header_and_footer_with_labels() {
        let rule = running_page_rule(HeaderOptions::default(), "Pack", Some("2026-10-27"));
        assert!(rule.starts_with("#set page("), "{rule}");
        assert!(rule.contains("header: context"), "{rule}");
        assert!(rule.contains(&format!("<{HEADER_LABEL}>")), "{rule}");
        assert!(rule.contains("footer:"), "{rule}");
        assert!(rule.contains(&format!("<{FOOTER_LABEL}>")), "{rule}");
        assert!(rule.contains("counter(page)"), "{rule}");
        assert!(rule.contains("\"2026-10-27\""), "{rule}");
        assert!(rule.contains("\"Pack\""), "{rule}");
        assert!(!rule.contains("datetime.today"), "{rule}");
    }

    #[test]
    fn without_a_date_the_header_shows_today() {
        let rule = running_page_rule(HeaderOptions::default(), "Pack", None);
        assert!(rule.contains("datetime.today().display("), "{rule}");
    }

    #[test]
    fn each_switch_removes_exactly_its_part() {
        let rule = |show_title, show_page_number, show_date| {
            running_page_rule(
                HeaderOptions {
                    show_title,
                    show_page_number,
                    show_date,
                },
                "Pack",
                Some("2026-10-27"),
            )
        };
        let no_title = rule(false, true, true);
        assert!(!no_title.contains("query(") && no_title.contains("2026-10-27"));
        let no_date = rule(true, true, false);
        assert!(no_date.contains("query(") && !no_date.contains("2026-10-27"));
        let no_number = rule(true, false, true);
        assert!(no_number.contains("footer: none") && !no_number.contains("counter(page)"));
        assert!(no_number.contains("header: context"));
    }

    #[test]
    fn nothing_to_show_clears_header_and_footer() {
        let rule = running_page_rule(
            HeaderOptions {
                show_title: false,
                show_page_number: false,
                show_date: false,
            },
            "Pack",
            None,
        );
        assert_eq!(rule, "#set page(header: none, footer: none)\n\n");
    }

    #[test]
    fn titles_and_dates_cannot_break_out_of_their_string_literals() {
        let rule = running_page_rule(
            HeaderOptions::default(),
            "a\"]\n#panic()\n[b",
            Some("d\"]\n#panic()"),
        );
        assert!(!rule.contains("\n#panic()"), "{rule}");
    }
}

//! Keyword-based `doc_type` inference for `doc_save` (REQ-PLA-001).
//!
//! When a new document is saved without an explicit `doc_type`, the type is
//! guessed from its slug, title and tags. An explicit `doc_type` always wins
//! (the caller of [`infer_doc_type`] only consults it when none was passed).

/// Keyword table in priority order: the first doc type with any matching
/// keyword wins (`spec` > `design` > `plan` > `adr` > `guide`).
///
/// Each keyword is a token sequence. ASCII keywords must match whole tokens
/// (so `plan` does not fire on `planet`); a sequence matches when its tokens
/// appear contiguously, which lets `how-to`, `how to` and `implementation
/// plan` match regardless of the separator used by slug, title or tag.
/// Non-ASCII keywords (Japanese has no word separators) match as a substring
/// of a token.
const KEYWORDS: &[(&str, &[&[&str]])] = &[
    (
        "spec",
        &[
            &["spec"],
            &["specs"],
            &["specification"],
            &["specifications"],
            &["requirement"],
            &["requirements"],
            &["仕様"],
            &["要件"],
        ],
    ),
    ("design", &[&["design"], &["architecture"], &["設計"]]),
    (
        "plan",
        &[
            &["plan"],
            &["impl", "plan"],
            &["implementation", "plan"],
            &["実装計画"],
        ],
    ),
    ("adr", &[&["adr"], &["decision"], &["decisions"]]),
    (
        "guide",
        &[&["guide"], &["howto"], &["how", "to"], &["ガイド"]],
    ),
];

/// The `doc_type` used when no keyword matches.
pub const DEFAULT_DOC_TYPE: &str = "note";

/// Splits `text` into lowercase tokens on every non-alphanumeric character.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(str::to_string)
        .collect()
}

fn keyword_matches(tokens: &[String], keyword: &[&str]) -> bool {
    if keyword.iter().all(|k| k.is_ascii()) {
        tokens
            .windows(keyword.len())
            .any(|w| w.iter().zip(keyword).all(|(t, k)| t == k))
    } else {
        // Non-ASCII keywords are single-element sequences matched as substrings.
        tokens.iter().any(|t| t.contains(keyword[0]))
    }
}

/// Infers a `doc_type` from a document's slug (hyphen-separated tokens),
/// title (whitespace-separated tokens) and tags (each tag tokenized the same
/// way). Falls back to [`DEFAULT_DOC_TYPE`] when nothing matches.
pub fn infer_doc_type(slug: &str, title: &str, tags: &[String]) -> &'static str {
    // Each source is tokenized on its own so a keyword sequence never spans
    // two unrelated sources (e.g. the end of the slug and the start of a tag).
    let sources: Vec<Vec<String>> = std::iter::once(slug)
        .chain(std::iter::once(title))
        .chain(tags.iter().map(String::as_str))
        .map(tokenize)
        .collect();
    KEYWORDS
        .iter()
        .find(|(_, keywords)| {
            keywords
                .iter()
                .any(|kw| sources.iter().any(|tokens| keyword_matches(tokens, kw)))
        })
        .map_or(DEFAULT_DOC_TYPE, |(doc_type, _)| doc_type)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn infer(slug: &str, title: &str, tags: &[&str]) -> &'static str {
        let tags: Vec<String> = tags.iter().map(|t| t.to_string()).collect();
        infer_doc_type(slug, title, &tags)
    }

    #[test]
    fn plan_keywords_from_slug_title_and_tags() {
        assert_eq!(infer("impl-plan", "Anything", &[]), "plan");
        assert_eq!(infer("x", "Implementation plan for X", &[]), "plan");
        assert_eq!(infer("x", "Y", &["plan"]), "plan");
        assert_eq!(infer("x", "認証の実装計画", &[]), "plan");
        assert_eq!(infer("2026-roadmap-plan", "Roadmap", &[]), "plan");
    }

    #[test]
    fn spec_keywords() {
        assert_eq!(infer("auth-spec", "Auth", &[]), "spec");
        assert_eq!(infer("x", "Login Specification", &[]), "spec");
        assert_eq!(infer("x", "Requirements", &[]), "spec");
        assert_eq!(infer("x", "認証仕様", &[]), "spec");
        assert_eq!(infer("x", "要件定義", &[]), "spec");
        assert_eq!(infer("x", "Y", &["requirement"]), "spec");
    }

    #[test]
    fn design_adr_and_guide_keywords() {
        assert_eq!(infer("api-design", "API", &[]), "design");
        assert_eq!(infer("x", "System Architecture", &[]), "design");
        assert_eq!(infer("x", "詳細設計", &[]), "design");
        assert_eq!(infer("adr-001", "Use Postgres", &[]), "adr");
        assert_eq!(infer("x", "Decision log", &[]), "adr");
        assert_eq!(infer("setup-guide", "Setup", &[]), "guide");
        assert_eq!(infer("x", "セットアップガイド", &[]), "guide");
        assert_eq!(infer("x", "Deploy howto", &[]), "guide");
        assert_eq!(infer("how-to-deploy", "Deploy", &[]), "guide");
        assert_eq!(infer("x", "How to deploy", &[]), "guide");
    }

    #[test]
    fn priority_is_spec_design_plan_adr_guide() {
        assert_eq!(infer("plan-spec", "x", &[]), "spec");
        assert_eq!(infer("plan-design", "x", &[]), "design");
        assert_eq!(infer("adr-plan", "x", &[]), "plan");
        assert_eq!(infer("guide-adr", "x", &[]), "adr");
        // Priority applies across sources too: title says plan, tag says spec.
        assert_eq!(infer("x", "Release plan", &["spec"]), "spec");
    }

    #[test]
    fn no_match_falls_back_to_note() {
        assert_eq!(infer("meeting-notes", "Weekly sync", &["team"]), "note");
        assert_eq!(infer("", "", &[]), "note");
    }

    #[test]
    fn ascii_keywords_match_whole_tokens_only() {
        assert_eq!(infer("planet-notes", "Planetary", &[]), "note");
        assert_eq!(infer("designer-tips", "Designers", &[]), "note");
        assert_eq!(infer("inspect", "Specious", &[]), "note");
    }

    #[test]
    fn matching_is_case_insensitive() {
        assert_eq!(infer("X", "IMPLEMENTATION PLAN", &[]), "plan");
        assert_eq!(infer("x", "y", &["ADR"]), "adr");
    }

    #[test]
    fn keyword_sequences_do_not_span_sources() {
        // "how" at the end of the slug and "to" in the title must not form "how to".
        assert_eq!(infer("know-how", "to do", &[]), "note");
    }
}

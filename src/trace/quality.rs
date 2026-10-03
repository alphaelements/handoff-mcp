//! Requirement quality aspects (wiki/270-vmodel-m3-design.md §4.6, M3-10,
//! FR-504/E22): ISO/IEC/IEEE 29148-aligned quality aspects and the LLM prompt
//! templates `handoff_trace_lint(action="quality_prompt")` returns for them.
//! Pure, no file I/O (D1, same convention as every other `src/trace/`
//! module) — the caller fills a template with the item's own text and sends
//! it to an LLM itself (E22: "MCP サーバー内で LLM を呼ばない").
//!
//! Deterministically-checkable aspects (`ambiguous_word`/`missing_acceptance`/
//! `passive_voice_hint`) are implemented as ordinary `trace_lint` rules in
//! `src/trace/lint.rs` instead of living here — this module is only for the
//! aspects that genuinely require judgment an LLM, not a string match, must
//! make.

/// Every word flagged by the `ambiguous_word` rule (wiki/270 §4.6's table) —
/// Japanese and English. Matched via `str::contains` against an item's title
/// (`ItemLintMeta::title`, `SubItem.description`): no tokenization, no
/// normalization beyond what the author wrote — a deliberately cheap
/// substring check (§6: "品質ルールは文字列照合のみで計算コストは無視できる").
pub const AMBIGUOUS_WORDS: &[&str] = &[
    "適切に",
    "など",
    "必要に応じて",
    "ユーザーフレンドリー",
    "できるだけ",
    "通常",
    "appropriate",
    "etc.",
    "as needed",
    "user-friendly",
    "as much as possible",
    "usually",
    "normally",
];

/// Returns the first [`AMBIGUOUS_WORDS`] entry found in `text`, or `None`.
/// Case-insensitive for the ASCII entries (a title authored as "As Needed"
/// must still match "as needed") — Japanese entries have no case to fold.
pub fn find_ambiguous_word(text: &str) -> Option<&'static str> {
    let lower = text.to_lowercase();
    AMBIGUOUS_WORDS
        .iter()
        .find(|w| {
            if w.is_ascii() {
                lower.contains(&w.to_lowercase())
            } else {
                text.contains(*w)
            }
        })
        .copied()
}

/// English past-participle endings/irregulars the `passive_voice_hint` rule's
/// "is/are/was/were + past participle" heuristic accepts as the word right
/// after a be-verb. Not an exhaustive list of irregular participles — just
/// enough common ones (wiki/270 §4.6 gives no exhaustive list either, only
/// the pattern shape) to catch the common case without a full POS tagger,
/// which would be well outside this rule's "string matching only" budget
/// (§6).
const IRREGULAR_PAST_PARTICIPLES: &[&str] = &[
    "done", "written", "given", "taken", "made", "shown", "known", "seen", "sent", "built", "held",
    "kept", "chosen", "broken", "found", "left", "set", "read", "put", "run",
];

/// True if `text` contains an English "is/are/was/were <word>" bigram where
/// `<word>` looks like a past participle (ends in "-ed", or is one of
/// [`IRREGULAR_PAST_PARTICIPLES`]) — the ASCII half of `passive_voice_hint`
/// (wiki/270 §4.6: "英語は \"is/are/was/were + past participle\""). Word
/// boundaries are ASCII-whitespace/punctuation splits; a be-verb as a
/// substring of a longer word (e.g. "this") never matches because the split
/// token must equal "is"/"are"/"was"/"were" exactly.
fn has_english_passive_voice(text: &str) -> bool {
    const BE_VERBS: &[&str] = &["is", "are", "was", "were"];
    let tokens: Vec<String> = text
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| t.to_lowercase())
        .collect();
    tokens.windows(2).any(|w| {
        BE_VERBS.contains(&w[0].as_str())
            && (w[1].ends_with("ed") || IRREGULAR_PAST_PARTICIPLES.contains(&w[1].as_str()))
    })
}

/// Japanese passive-voice markers the `passive_voice_hint` rule's Japanese
/// half matches (wiki/270 §4.6: "「される」「られる」") — a plain substring
/// check, same budget as [`find_ambiguous_word`].
const JAPANESE_PASSIVE_MARKERS: &[&str] = &["される", "られる"];

/// True if `text` contains a Japanese or English passive-voice hint.
pub fn has_passive_voice_hint(text: &str) -> bool {
    JAPANESE_PASSIVE_MARKERS.iter().any(|m| text.contains(m)) || has_english_passive_voice(text)
}

/// One ISO/IEC/IEEE 29148 quality characteristic `quality_prompt` offers a
/// template for (wiki/270 §4.6's list — the LLM-judgment half of FR-504,
/// E22). Declaration order is the order `quality_prompt` lists aspects in
/// when `aspects` is omitted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QualityAspect {
    Singular,
    Verifiable,
    Unambiguous,
    Complete,
    Feasible,
    Traceable,
}

/// Every aspect `quality_prompt` knows, in the order §4.6 lists them.
pub const ALL_ASPECTS: &[QualityAspect] = &[
    QualityAspect::Singular,
    QualityAspect::Verifiable,
    QualityAspect::Unambiguous,
    QualityAspect::Complete,
    QualityAspect::Feasible,
    QualityAspect::Traceable,
];

impl QualityAspect {
    pub fn name(self) -> &'static str {
        match self {
            QualityAspect::Singular => "singular",
            QualityAspect::Verifiable => "verifiable",
            QualityAspect::Unambiguous => "unambiguous",
            QualityAspect::Complete => "complete",
            QualityAspect::Feasible => "feasible",
            QualityAspect::Traceable => "traceable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        ALL_ASPECTS.iter().find(|a| a.name() == s).copied()
    }

    /// A short explanation of what this 29148 characteristic means —
    /// embedded in the prompt template as context for the LLM evaluating it.
    pub fn context(self) -> &'static str {
        match self {
            QualityAspect::Singular => {
                "The requirement states a single capability, constraint, or \
                 condition — it does not bundle multiple requirements with \
                 \"and\"/\"or\" into one statement."
            }
            QualityAspect::Verifiable => {
                "The requirement can be verified by inspection, analysis, \
                 demonstration, or test — it does not rely on a subjective or \
                 unmeasurable judgment."
            }
            QualityAspect::Unambiguous => {
                "The requirement has only one possible interpretation — its \
                 terms are defined or used consistently, and it is free of \
                 vague qualifiers."
            }
            QualityAspect::Complete => {
                "The requirement needs no further amplification — it states \
                 what is needed without missing conditions, exceptions, or \
                 responses."
            }
            QualityAspect::Feasible => {
                "The requirement can be realized within the constraints of \
                 the project (technical, schedule, budget, and legal)."
            }
            QualityAspect::Traceable => {
                "The requirement can be traced backward to its origin (e.g. a \
                 stakeholder need) and forward to the design/test artifacts \
                 that satisfy and verify it."
            }
        }
    }

    /// Renders the prompt template an LLM evaluates `item_text` against for
    /// this aspect. `{text}` is the literal placeholder the caller (wiki/270
    /// §4.6: "呼び出し側がプロンプトテンプレートに項目テキストを埋めて LLM に
    /// 評価させる") substitutes with the requirement's own text before
    /// sending it to an LLM — this crate never performs that substitution or
    /// calls an LLM itself (E22).
    pub fn prompt_template(self) -> String {
        format!(
            "Evaluate whether the following requirement satisfies the ISO/IEC/IEEE 29148 \
             \"{name}\" quality characteristic.\n\n\
             Characteristic: {name}\n\
             Definition: {context}\n\n\
             Requirement text:\n\"\"\"\n{{text}}\n\"\"\"\n\n\
             Answer with: pass or fail, followed by a one-sentence reason. If fail, suggest a \
             concrete rewrite.",
            name = self.name(),
            context = self.context(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_japanese_ambiguous_words() {
        assert_eq!(find_ambiguous_word("適切に処理すること"), Some("適切に"));
        assert_eq!(
            find_ambiguous_word("必要に応じて再試行する"),
            Some("必要に応じて")
        );
        assert_eq!(find_ambiguous_word("明確な仕様である"), None);
    }

    #[test]
    fn finds_english_ambiguous_words_case_insensitively() {
        assert_eq!(
            find_ambiguous_word("The system should behave in an Appropriate way"),
            Some("appropriate")
        );
        assert_eq!(
            find_ambiguous_word("Retry AS NEEDED on failure"),
            Some("as needed")
        );
        assert_eq!(
            find_ambiguous_word("The system shall log every request"),
            None
        );
    }

    #[test]
    fn detects_japanese_passive_voice() {
        assert!(has_passive_voice_hint("データは自動的に削除される"));
        assert!(has_passive_voice_hint("結果はユーザーに表示される"));
        assert!(!has_passive_voice_hint("システムがデータを削除する"));
    }

    #[test]
    fn detects_english_passive_voice() {
        assert!(has_passive_voice_hint(
            "The request is processed by the server"
        ));
        assert!(has_passive_voice_hint("The file was written to disk"));
        assert!(has_passive_voice_hint("Errors are logged for review"));
        assert!(!has_passive_voice_hint("The server processes the request"));
    }

    #[test]
    fn all_aspects_render_a_template_containing_the_text_placeholder() {
        for aspect in ALL_ASPECTS {
            let template = aspect.prompt_template();
            assert!(
                template.contains("{text}"),
                "{:?} template must contain the {{text}} placeholder",
                aspect
            );
            assert!(template.contains(aspect.name()));
        }
    }

    #[test]
    fn parse_roundtrips_every_aspect_name() {
        for aspect in ALL_ASPECTS {
            assert_eq!(QualityAspect::parse(aspect.name()), Some(*aspect));
        }
        assert_eq!(QualityAspect::parse("not-a-real-aspect"), None);
    }
}

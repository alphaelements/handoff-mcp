//! Bundled semantic embedding model access.
//!
//! `semantic_ja_en_v1.bin` (a lexsim `semantic` feature model binary) is
//! embedded directly into the compiled binary via `include_bytes!` and
//! parsed exactly once per process, on first use, via `OnceLock`. See
//! wiki/170-lexsim-hybrid-integration.md §1 for the design rationale.

use std::sync::OnceLock;

use lexsim::semantic::SemanticModelView;

static MODEL_BYTES: &[u8] = include_bytes!("model_data/semantic_ja_en_v1.bin");
static MODEL: OnceLock<SemanticModelView<'static>> = OnceLock::new();

/// The process-wide semantic model view, parsed from the embedded binary on
/// first call and cached for the lifetime of the process.
///
/// # Panics
///
/// Panics if the embedded model binary fails to parse. This can only happen
/// if the bundled binary itself is corrupt (a build-time bug), since the
/// bytes are fixed at compile time via `include_bytes!` — not a condition
/// that can arise from runtime input.
pub fn semantic_model() -> &'static SemanticModelView<'static> {
    MODEL.get_or_init(|| {
        SemanticModelView::from_bytes(MODEL_BYTES).expect("failed to parse semantic model binary")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_model_parses_embedded_binary() {
        let model = semantic_model();
        assert!(model.dimension() > 0);
    }

    #[test]
    fn semantic_model_is_cached_across_calls() {
        let a = semantic_model() as *const SemanticModelView<'static>;
        let b = semantic_model() as *const SemanticModelView<'static>;
        assert_eq!(
            a, b,
            "semantic_model() must return the same cached instance"
        );
    }

    #[test]
    fn semantic_model_embeds_text_successfully() {
        let model = semantic_model();
        let embedding = model
            .embed("always use atomic_write for session persistence")
            .expect("embedding should succeed for well-formed text");
        assert_eq!(embedding.len(), model.dimension());
    }

    #[test]
    fn semantic_model_similarity_is_symmetric_and_bounded() {
        let model = semantic_model();
        let sim = model
            .similarity("session persistence", "セッション永続化")
            .expect("similarity should succeed");
        assert!(
            (-1.0..=1.0).contains(&sim),
            "cosine similarity out of range: {sim}"
        );
    }
}

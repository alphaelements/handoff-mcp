//! Document metadata filter shared by `handoff_doc_list` and the scoped
//! export of `handoff_doc_export` (REQ-PDF-021), so both select documents by
//! one definition. Every given condition must hold (AND).

use super::model::DocMetadata;

/// Conditions a document must all satisfy. A `None` / empty field does not
/// constrain.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocFilter {
    /// Exact `doc_type`.
    pub doc_type: Option<String>,
    /// Every listed tag must be present on the document.
    pub tags: Vec<String>,
    /// The document must be linked to this task.
    pub task_id: Option<String>,
    /// Exact V-model `layer` id.
    pub layer: Option<String>,
    /// Exact document-level `milestone`.
    pub milestone: Option<String>,
}

impl DocFilter {
    /// `true` when no condition is set, i.e. every document would match.
    pub fn is_empty(&self) -> bool {
        self.doc_type.is_none()
            && self.tags.is_empty()
            && self.task_id.is_none()
            && self.layer.is_none()
            && self.milestone.is_none()
    }

    /// Whether `doc` satisfies every condition.
    pub fn matches(&self, doc: &DocMetadata) -> bool {
        self.doc_type.as_deref().is_none_or(|t| doc.doc_type == t)
            && self.tags.iter().all(|t| doc.tags.contains(t))
            && self
                .task_id
                .as_deref()
                .is_none_or(|id| doc.task_ids.iter().any(|t| t == id))
            && self
                .layer
                .as_deref()
                .is_none_or(|l| doc.layer.as_deref() == Some(l))
            && self
                .milestone
                .as_deref()
                .is_none_or(|m| doc.milestone.as_deref() == Some(m))
    }

    /// Keeps only the documents that match, preserving order.
    pub fn retain(&self, docs: &mut Vec<DocMetadata>) {
        docs.retain(|d| self.matches(d));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn doc() -> DocMetadata {
        let mut d = DocMetadata::new(
            "doc-1".into(),
            "slug-1".into(),
            "T".into(),
            "spec".into(),
            "2026-01-01T00:00:00Z".into(),
        );
        d.tags = vec!["a".into(), "b".into()];
        d.task_ids = vec!["t1".into()];
        d.layer = Some("requirement".into());
        d.milestone = Some("v1".into());
        d
    }

    #[test]
    fn empty_filter_matches_everything() {
        let f = DocFilter::default();
        assert!(f.is_empty());
        assert!(f.matches(&doc()));
    }

    #[test]
    fn each_condition_constrains() {
        let d = doc();
        let f = |f: DocFilter| f.matches(&d);
        assert!(f(DocFilter {
            doc_type: Some("spec".into()),
            ..Default::default()
        }));
        assert!(!f(DocFilter {
            doc_type: Some("adr".into()),
            ..Default::default()
        }));
        assert!(f(DocFilter {
            tags: vec!["a".into(), "b".into()],
            ..Default::default()
        }));
        assert!(!f(DocFilter {
            tags: vec!["a".into(), "z".into()],
            ..Default::default()
        }));
        assert!(f(DocFilter {
            task_id: Some("t1".into()),
            ..Default::default()
        }));
        assert!(!f(DocFilter {
            task_id: Some("t2".into()),
            ..Default::default()
        }));
        assert!(f(DocFilter {
            layer: Some("requirement".into()),
            ..Default::default()
        }));
        assert!(!f(DocFilter {
            layer: Some("acceptance".into()),
            ..Default::default()
        }));
        assert!(f(DocFilter {
            milestone: Some("v1".into()),
            ..Default::default()
        }));
        assert!(!f(DocFilter {
            milestone: Some("v2".into()),
            ..Default::default()
        }));
    }

    #[test]
    fn conditions_are_anded() {
        let d = doc();
        let both_ok = DocFilter {
            layer: Some("requirement".into()),
            milestone: Some("v1".into()),
            ..Default::default()
        };
        assert!(!both_ok.is_empty() && both_ok.matches(&d));
        let one_bad = DocFilter {
            milestone: Some("v2".into()),
            ..both_ok
        };
        assert!(!one_bad.matches(&d));
    }

    #[test]
    fn layer_and_milestone_filters_reject_documents_without_them() {
        let mut d = doc();
        d.layer = None;
        d.milestone = None;
        assert!(!DocFilter {
            layer: Some("requirement".into()),
            ..Default::default()
        }
        .matches(&d));
        assert!(!DocFilter {
            milestone: Some("v1".into()),
            ..Default::default()
        }
        .matches(&d));
    }
}

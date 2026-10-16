//! Deterministic 3,000-item synthetic requirement-tree corpus
//! (wiki/300-requirement-tree-design.md, SPEC-TREE-015, t-rtree.7).
//!
//! A **pure** generator: no I/O, no randomness, no `HashMap` iteration. The
//! same call always returns byte-identical documents, so a bench or a
//! determinism test that feeds them to the real binary exercises the same
//! workspace every time.
//!
//! Shape (3,000 items over 30 documents):
//!
//! | layer           | items | documents |
//! |-----------------|-------|-----------|
//! | `requirement`   | 2,000 | 20        |
//! | `basic_spec`    |   700 | 7         |
//! | `detailed_spec` |   300 | 3         |
//!
//! Inside each layer the `- parent:` edges form a DAG:
//!
//! * the first [`CHAIN_LEN`] items of the `requirement` layer are a single
//!   chain (depth 25), the "deep tree" case;
//! * [`ROOTS_PER_LAYER`] items per layer are roots;
//! * every other item hangs under an earlier item; every
//!   [`SECOND_PARENT_EVERY`]-th one also under a second earlier item
//!   (multiple parents, DAG-shared nodes);
//! * every [`DECISION_EVERY`]-th item is `kind: decision`, every
//!   [`GROUP_EVERY`]-th a declared `kind: group` (composites);
//! * every [`ORDER_EVERY`]-th item carries an explicit `- order:` (and every
//!   second of those a per-parent `order@<parent>`), the rest fall back to
//!   document/sequence order;
//! * every `basic_spec` / `detailed_spec` item `refines` one item of the layer
//!   above, so the vertical V-model edges are present too.
//!
//! Parents always point to an *earlier* item, so saving the documents in
//! the returned order never trips the dangling-parent check.

#![allow(dead_code)] // each including crate uses a different subset

use std::fmt::Write as _;

/// Items per generated document.
const ITEMS_PER_DOC: usize = 100;
/// Length of the deliberate deep chain at the start of the `requirement` layer.
pub const CHAIN_LEN: usize = 25;
/// Roots (parentless items after the chain) per layer.
pub const ROOTS_PER_LAYER: usize = 8;
/// How far back (in layer-local index) an ordinary parent may sit.
const PARENT_WINDOW: usize = 50;
pub const SECOND_PARENT_EVERY: usize = 5;
pub const DECISION_EVERY: usize = 23;
pub const GROUP_EVERY: usize = 29;
pub const ORDER_EVERY: usize = 11;

/// `(layer id, id prefix, item count)`, upper layers first.
pub const LAYERS: [(&str, &str, usize); 3] = [
    ("requirement", "REQ", 2000),
    ("basic_spec", "SPEC", 700),
    ("detailed_spec", "DS", 300),
];

pub const TOTAL_ITEMS: usize = 3000;
pub const TOTAL_DOCS: usize = 30;

/// One layer document to save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SynthDoc {
    pub slug: String,
    pub layer: &'static str,
    pub body: String,
}

fn item_id(prefix: &str, index: usize) -> String {
    format!("{prefix}-{index:05}")
}

/// Layer-local indices of `index`'s parents (empty for a root), all `< index`.
/// Only the `requirement` layer (`layer_pos == 0`) starts with the deep chain.
fn parent_indices(layer_pos: usize, index: usize) -> Vec<usize> {
    let chain = if layer_pos == 0 { CHAIN_LEN } else { 0 };
    if index < chain {
        // Item 0 is the chain head (a root); every later chain link hangs
        // under its predecessor.
        return if index == 0 { vec![] } else { vec![index - 1] };
    }
    if index - chain < ROOTS_PER_LAYER {
        return vec![];
    }
    let window = index.min(PARENT_WINDOW);
    let first = index - 1 - (index * 31) % window;
    let mut parents = vec![first];
    if index % SECOND_PARENT_EVERY == 0 {
        let second = (index * 17 + 5) % index;
        if second != first {
            parents.push(second);
        }
    }
    parents
}

/// The whole corpus, in the order it must be saved (upper layers first).
pub fn synth_docs() -> Vec<SynthDoc> {
    let mut docs = Vec::with_capacity(TOTAL_DOCS);
    for (layer_pos, (layer, prefix, count)) in LAYERS.iter().enumerate() {
        let upper = layer_pos.checked_sub(1).map(|p| LAYERS[p]);
        let mut index = 0usize;
        let mut doc_no = 0usize;
        while index < *count {
            let end = (index + ITEMS_PER_DOC).min(*count);
            let mut body = format!("# {layer} synthetic {doc_no}\n\n");
            for i in index..end {
                write_item(&mut body, layer_pos, prefix, i, upper);
            }
            docs.push(SynthDoc {
                slug: format!("synth-{}-{doc_no:02}", layer.replace('_', "-")),
                layer,
                body,
            });
            index = end;
            doc_no += 1;
        }
    }
    docs
}

fn write_item(
    body: &mut String,
    layer_pos: usize,
    prefix: &str,
    i: usize,
    upper: Option<(&'static str, &'static str, usize)>,
) {
    let id = item_id(prefix, i);
    let parents = parent_indices(layer_pos, i);
    // Writes to a `String` cannot fail.
    let _ = write!(
        body,
        "### {id} Synthetic item {i}\n\n- priority: P{}\n",
        i % 4
    );
    if let Some((_, upper_prefix, upper_count)) = upper {
        let target = item_id(upper_prefix, (i * 7 + 3) % upper_count);
        let _ = writeln!(body, "- refines: {target}");
    }
    if !parents.is_empty() {
        let list: Vec<String> = parents.iter().map(|p| item_id(prefix, *p)).collect();
        let _ = writeln!(body, "- parent: {}", list.join(", "));
    }
    if i % DECISION_EVERY == DECISION_EVERY - 1 {
        body.push_str("- kind: decision\n");
    } else if i % GROUP_EVERY == GROUP_EVERY - 1 {
        body.push_str("- kind: group\n");
    }
    if i % ORDER_EVERY == 0 {
        let _ = writeln!(body, "- order: {}", (i * 13) % 97);
        if (i / ORDER_EVERY) % 2 == 0 && parents.len() > 1 {
            let _ = writeln!(
                body,
                "- order@{}: {}",
                item_id(prefix, parents[1]),
                (i * 7) % 89
            );
        }
    }
    let _ = write!(body, "\nSynthetic statement for {id}.\n\n");
}

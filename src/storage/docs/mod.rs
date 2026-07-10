//! Document management: splitting a single authored Markdown body into
//! fragments, and reassembling fragments back into a byte-identical body.
//!
//! Layout (established here, populated by storage I/O in a follow-up task):
//!
//! ```text
//! .handoff/docs/
//!   _doc.<doc-id>.json          # document metadata
//!   _frag.<doc-id>.<seq>.json   # fragment metadata
//!   _frag.<doc-id>.<seq>.md     # fragment body (pure Markdown)
//!   injected/
//!     <session-id>.json         # per-session "already injected" sidecar
//! ```
//!
//! See `wiki/130-document-management.md` §3-4 for the full storage
//! architecture and data model, and §8 for the reversibility guarantee that
//! [`split`](split::split) + [`reassemble`](reassemble::reassemble) implement.
//!
//! This module currently only hosts the pure split/reassemble algorithm
//! (P1-1). File I/O (`write_doc`, `read_doc`, `write_fragment`, ...) is a
//! separate task (P1-2) and intentionally not implemented here yet.

pub mod reassemble;
pub mod split;

//! PDF export pipeline building blocks (REQ-PDF-*).

pub mod bundle_layout;
#[cfg(feature = "bundled-typst")]
pub mod bundled;
pub mod deps;
pub mod export;
pub mod gitignore;
pub mod markdown_to_typst;
pub mod mermaid;
pub mod req_meta;
pub mod template;
pub mod types;

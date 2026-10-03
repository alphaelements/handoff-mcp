//! M1 trace derivation (wiki/220-vmodel-integration-design.md §2.7, t360.9):
//! a pure-function engine over layer items, task requirement links, and run
//! results — no file I/O. [`engine::TraceGraph::build`] is the entry point;
//! [`adapter`] shows how to gather its [`types::TraceInput`] from the live
//! storage layer, but nothing here is wired into an MCP handler yet
//! (`handoff_trace_report`/`handoff_trace_slice` are t360.10/11's scope).

pub mod adapter;
pub mod baseline;
pub mod engine;
pub mod lint;
pub mod matrix;
pub mod next;
pub mod profile;
pub mod quality;
pub mod suspect;
pub mod task_view;
pub mod types;

pub use engine::TraceGraph;
pub use next::{derive_next_actions, ItemNextMeta, NextAction, NextActionKind, Suggest};
pub use task_view::{
    compute_task_blockers_for_task, compute_task_views, TaskBlockerCounts, TaskLayerCount,
    TaskTraceView,
};
pub use types::*;

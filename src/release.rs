//! Pre-release check of a milestone (SPEC-ONB-040, SPEC-ONB-041,
//! SPEC-ONB-050): is everything the milestone promised done, verified and
//! safe to ship?
//!
//! - [`check`] judges the items from facts it is handed. It reads nothing
//!   and starts nothing.
//! - [`record`] keeps what was checked in `.handoff/release/<milestone>.json`,
//!   so a later step (releasing the milestone) can refer to it.
//! - [`cli`] is `handoff-mcp release-check`, the only entry point that runs
//!   the project's commands itself (`--run`).
//!
//! The `handoff_release_check` MCP tool (`crate::mcp::handlers::release_check`)
//! gathers the facts and never runs a command: it hands the commands back in
//! `commands_to_run` and judges the results the agent reports.

pub mod check;
pub mod cli;
pub mod guard;
pub mod record;

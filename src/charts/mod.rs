//! Chart generation file channel: MCP -> VSCode extension communication.
//!
//! The MCP server writes a request JSON to `.handoff/charts/requests/<uuid>.json`
//! and polls for a response at `.handoff/charts/responses/<uuid>.json`. The
//! VSCode extension (when running) picks up the request, renders the chart, writes
//! the response, and the MCP server reads it back.
//!
//! When the extension is not running (timeout), the result is "no image" — not an
//! error — so reports degrade gracefully with a note instead of failing.

pub mod file_channel;

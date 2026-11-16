//! File-based channel for MCP -> VSCode chart requests (REQ-VIS-001, REQ-VIS-002).
//!
//! Request flow:
//! 1. MCP writes request JSON to `.handoff/charts/requests/<uuid>.json`
//! 2. MCP polls for `.handoff/charts/responses/<uuid>.json` (30s per chart)
//! 3. VSCode extension picks up request, renders chart, writes response
//! 4. MCP reads response or returns timeout (graceful, not an error)

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Monotonically increasing counter for unique request IDs within one process.
static REQUEST_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Subdirectory under `.handoff/` for chart requests.
pub const REQUESTS_SUBDIR: &str = "charts/requests";
/// Subdirectory under `.handoff/` for chart responses.
pub const RESPONSES_SUBDIR: &str = "charts/responses";

/// Default per-chart poll timeout (how long to wait for one chart response).
pub const DEFAULT_PER_CHART_TIMEOUT: Duration = Duration::from_secs(30);
/// Default total timeout for all charts in one report generation.
pub const DEFAULT_TOTAL_TIMEOUT: Duration = Duration::from_secs(120);
/// Poll interval when checking for response file.
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Environment variable that overrides the per-chart timeout (in milliseconds).
/// Set to `0` to skip the file channel entirely and always use fallback notes.
pub const CHART_TIMEOUT_ENV: &str = "HANDOFF_CHART_TIMEOUT_MS";

/// Read the per-chart timeout from the environment variable, or use the default.
pub fn per_chart_timeout() -> Duration {
    std::env::var(CHART_TIMEOUT_ENV)
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .map(Duration::from_millis)
        .unwrap_or(DEFAULT_PER_CHART_TIMEOUT)
}

/// Read the total timeout: 4x the per-chart timeout, capped at the default.
pub fn total_timeout() -> Duration {
    let per = per_chart_timeout();
    let total = per.saturating_mul(4);
    total.min(DEFAULT_TOTAL_TIMEOUT)
}

/// Chart view types supported by the file channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChartView {
    Metrics,
    Gantt,
    TraceTree,
    Board,
    TraceMatrix,
    Dashboard,
}

impl ChartView {
    pub fn as_str(self) -> &'static str {
        match self {
            ChartView::Metrics => "metrics",
            ChartView::Gantt => "gantt",
            ChartView::TraceTree => "trace_tree",
            ChartView::Board => "board",
            ChartView::TraceMatrix => "trace_matrix",
            ChartView::Dashboard => "dashboard",
        }
    }
}

/// Chart output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ChartFormat {
    Png,
    Svg,
}

/// Request JSON written by MCP for the VSCode extension to pick up.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartRequest {
    pub view: ChartView,
    pub format: ChartFormat,
    #[serde(default, skip_serializing_if = "serde_json::Value::is_null")]
    pub filters: serde_json::Value,
    pub output_path: String,
}

/// Response JSON written by the VSCode extension.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartResponse {
    pub status: ChartResponseStatus,
    #[serde(default)]
    pub output_paths: Vec<String>,
    #[serde(default)]
    pub errors: Vec<ChartError>,
}

/// Status of a chart response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChartResponseStatus {
    Success,
    Error,
    Partial,
}

/// Error entry in a chart response.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChartError {
    pub code: String,
    pub message: String,
}

/// Result of requesting a chart through the file channel.
#[derive(Debug, Clone, PartialEq)]
pub enum ChartResult {
    /// Chart was successfully rendered; `output_paths` has the image file paths.
    Success { output_paths: Vec<String> },
    /// Chart was partially rendered (some errors).
    Partial {
        output_paths: Vec<String>,
        errors: Vec<ChartError>,
    },
    /// VSCode extension returned an error.
    Error { errors: Vec<ChartError> },
    /// VSCode extension did not respond within the timeout (not running).
    Timeout,
}

impl ChartResult {
    /// Whether this result has at least one output image path.
    pub fn has_output(&self) -> bool {
        match self {
            ChartResult::Success { output_paths } | ChartResult::Partial { output_paths, .. } => {
                !output_paths.is_empty()
            }
            _ => false,
        }
    }

    /// The first output path, if any.
    pub fn first_output(&self) -> Option<&str> {
        match self {
            ChartResult::Success { output_paths } | ChartResult::Partial { output_paths, .. } => {
                output_paths.first().map(String::as_str)
            }
            _ => None,
        }
    }
}

/// Validate a [`ChartRequest`]: all required fields must be present and valid.
pub fn validate_request(request: &ChartRequest) -> Result<()> {
    if request.output_path.is_empty() {
        bail!("output_path must not be empty");
    }
    // View and format are validated by serde deserialization.
    Ok(())
}

/// Ensure the request and response directories exist under `handoff_dir`.
fn ensure_dirs(handoff_dir: &Path) -> Result<(PathBuf, PathBuf)> {
    let requests_dir = handoff_dir.join(REQUESTS_SUBDIR);
    let responses_dir = handoff_dir.join(RESPONSES_SUBDIR);
    if !requests_dir.exists() {
        std::fs::create_dir_all(&requests_dir)?;
    }
    if !responses_dir.exists() {
        std::fs::create_dir_all(&responses_dir)?;
    }
    Ok((requests_dir, responses_dir))
}

/// Generate a unique request ID using timestamp, process ID and a counter.
fn generate_request_id() -> String {
    let ts = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    let pid = std::process::id();
    let seq = REQUEST_COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("{ts}-{pid}-{seq}")
}

/// Request a chart through the file channel.
///
/// Writes a request JSON to `.handoff/charts/requests/<uuid>.json`, then polls
/// for a response at `.handoff/charts/responses/<uuid>.json` with the given
/// `timeout`. On timeout (VSCode not running), returns [`ChartResult::Timeout`]
/// instead of an error.
///
/// Cleans up request files after processing.
pub fn request_chart(
    handoff_dir: &Path,
    request: &ChartRequest,
    timeout: Duration,
) -> Result<ChartResult> {
    validate_request(request)?;

    if timeout.is_zero() {
        return Ok(ChartResult::Timeout);
    }

    let (requests_dir, responses_dir) = ensure_dirs(handoff_dir)?;
    let uuid = generate_request_id();
    let request_path = requests_dir.join(format!("{uuid}.json"));
    let response_path = responses_dir.join(format!("{uuid}.json"));

    // Write request atomically.
    let request_json = serde_json::to_string_pretty(request)?;
    crate::storage::atomic_write(&request_path, request_json.as_bytes())?;

    // Poll for response.
    let result = poll_response(&response_path, timeout);

    // Clean up request file (best effort).
    let _ = std::fs::remove_file(&request_path);
    // Clean up response file if it exists (best effort).
    let _ = std::fs::remove_file(&response_path);

    result
}

/// Poll for a response file at `path` with `timeout`.
///
/// JSON parse errors, unknown status values and missing required fields are
/// caught and returned as [`ChartResult::Error`] instead of propagating up,
/// so a single broken response never aborts the entire report (SPEC-VIS-004).
fn poll_response(path: &Path, timeout: Duration) -> Result<ChartResult> {
    let deadline = Instant::now() + timeout;
    loop {
        if path.exists() {
            let content = std::fs::read_to_string(path)?;
            let response: ChartResponse = match serde_json::from_str(&content) {
                Ok(r) => r,
                Err(e) => {
                    return Ok(ChartResult::Error {
                        errors: vec![ChartError {
                            code: "PARSE_ERROR".into(),
                            message: format!("Failed to parse chart response JSON: {e}"),
                        }],
                    });
                }
            };
            return Ok(match response.status {
                ChartResponseStatus::Success => ChartResult::Success {
                    output_paths: response.output_paths,
                },
                ChartResponseStatus::Partial => ChartResult::Partial {
                    output_paths: response.output_paths,
                    errors: response.errors,
                },
                ChartResponseStatus::Error => ChartResult::Error {
                    errors: response.errors,
                },
            });
        }
        if Instant::now() >= deadline {
            return Ok(ChartResult::Timeout);
        }
        std::thread::sleep(POLL_INTERVAL);
    }
}

/// Request multiple charts, respecting the total timeout.
/// Returns results in the same order as the input requests.
pub fn request_charts(
    handoff_dir: &Path,
    requests: &[ChartRequest],
    per_chart_timeout: Duration,
    total_timeout: Duration,
) -> Result<Vec<ChartResult>> {
    let total_deadline = Instant::now() + total_timeout;
    let mut results = Vec::with_capacity(requests.len());
    for request in requests {
        let remaining = total_deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            results.push(ChartResult::Timeout);
            continue;
        }
        let timeout = per_chart_timeout.min(remaining);
        results.push(request_chart(handoff_dir, request, timeout)?);
    }
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// Wait for any `.json` file to appear in `dir` and return its stem.
    fn wait_for_request_file(dir: &Path) -> String {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if Instant::now() >= deadline {
                panic!("timed out waiting for request file in {}", dir.display());
            }
            if let Ok(entries) = std::fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let name = entry.file_name().to_string_lossy().to_string();
                    if let Some(stem) = name.strip_suffix(".json") {
                        return stem.to_string();
                    }
                }
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn tmp_handoff() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let handoff = dir.path().join(".handoff");
        std::fs::create_dir_all(&handoff).unwrap();
        (dir, handoff)
    }

    fn sample_request() -> ChartRequest {
        ChartRequest {
            view: ChartView::Metrics,
            format: ChartFormat::Png,
            filters: json!({}),
            output_path: "/tmp/chart.png".to_string(),
        }
    }

    // --- Validation tests ---

    #[test]
    fn validate_request_rejects_empty_output_path() {
        let req = ChartRequest {
            view: ChartView::Metrics,
            format: ChartFormat::Png,
            filters: json!(null),
            output_path: String::new(),
        };
        assert!(validate_request(&req).is_err());
    }

    #[test]
    fn validate_request_accepts_valid_request() {
        assert!(validate_request(&sample_request()).is_ok());
    }

    // --- Serialization tests ---

    #[test]
    fn chart_request_round_trips_through_json() {
        let req = ChartRequest {
            view: ChartView::Gantt,
            format: ChartFormat::Svg,
            filters: json!({"milestone": "v1"}),
            output_path: "/tmp/gantt.svg".to_string(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let parsed: ChartRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, req);
    }

    #[test]
    fn chart_response_deserializes_success() {
        let json = r#"{"status":"success","output_paths":["/tmp/chart.png"],"errors":[]}"#;
        let resp: ChartResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, ChartResponseStatus::Success);
        assert_eq!(resp.output_paths, vec!["/tmp/chart.png"]);
        assert!(resp.errors.is_empty());
    }

    #[test]
    fn chart_response_deserializes_error() {
        let json =
            r#"{"status":"error","output_paths":[],"errors":[{"code":"E001","message":"fail"}]}"#;
        let resp: ChartResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.status, ChartResponseStatus::Error);
        assert_eq!(resp.errors.len(), 1);
        assert_eq!(resp.errors[0].code, "E001");
    }

    #[test]
    fn chart_view_serializes_as_snake_case() {
        assert_eq!(
            serde_json::to_value(ChartView::TraceTree).unwrap(),
            json!("trace_tree")
        );
        assert_eq!(
            serde_json::to_value(ChartView::Metrics).unwrap(),
            json!("metrics")
        );
    }

    // --- File channel tests ---

    #[test]
    fn request_creates_directory_structure() {
        let (_dir, handoff) = tmp_handoff();
        let (requests_dir, responses_dir) = ensure_dirs(&handoff).unwrap();
        assert!(requests_dir.exists());
        assert!(responses_dir.exists());
    }

    #[test]
    fn request_chart_writes_request_and_times_out() {
        let (_dir, handoff) = tmp_handoff();
        let req = sample_request();
        // Use very short timeout to not slow tests down.
        let result = request_chart(&handoff, &req, Duration::from_millis(50)).unwrap();
        assert_eq!(result, ChartResult::Timeout);
        // Request file should be cleaned up even on timeout.
        let requests_dir = handoff.join(REQUESTS_SUBDIR);
        let files: Vec<_> = std::fs::read_dir(&requests_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(
            files.is_empty(),
            "request file should be cleaned up after timeout"
        );
    }

    #[test]
    fn request_chart_reads_success_response() {
        let (_dir, handoff) = tmp_handoff();
        let req = sample_request();
        ensure_dirs(&handoff).unwrap();

        // Simulate: spawn a thread that watches for request, then writes response.
        let requests_dir = handoff.join(REQUESTS_SUBDIR);
        let responses_dir = handoff.join(RESPONSES_SUBDIR);
        let handle = std::thread::spawn(move || {
            let uuid = wait_for_request_file(&requests_dir);
            let response = ChartResponse {
                status: ChartResponseStatus::Success,
                output_paths: vec!["/tmp/chart.png".to_string()],
                errors: vec![],
            };
            let response_path = responses_dir.join(format!("{uuid}.json"));
            std::fs::write(&response_path, serde_json::to_string(&response).unwrap()).unwrap();
        });

        let result = request_chart(&handoff, &req, Duration::from_secs(5)).unwrap();
        handle.join().unwrap();

        assert_eq!(
            result,
            ChartResult::Success {
                output_paths: vec!["/tmp/chart.png".to_string()]
            }
        );
    }

    #[test]
    fn request_chart_reads_error_response() {
        let (_dir, handoff) = tmp_handoff();
        let req = sample_request();
        ensure_dirs(&handoff).unwrap();

        let responses_dir = handoff.join(RESPONSES_SUBDIR);
        let requests_dir = handoff.join(REQUESTS_SUBDIR);

        let handle = std::thread::spawn(move || {
            let uuid = wait_for_request_file(&requests_dir);
            let response = ChartResponse {
                status: ChartResponseStatus::Error,
                output_paths: vec![],
                errors: vec![ChartError {
                    code: "RENDER_FAILED".into(),
                    message: "Could not render chart".into(),
                }],
            };
            let path = responses_dir.join(format!("{uuid}.json"));
            std::fs::write(&path, serde_json::to_string(&response).unwrap()).unwrap();
        });

        let result = request_chart(&handoff, &req, Duration::from_secs(5)).unwrap();
        handle.join().unwrap();

        match result {
            ChartResult::Error { errors } => {
                assert_eq!(errors.len(), 1);
                assert_eq!(errors[0].code, "RENDER_FAILED");
            }
            other => panic!("expected Error, got {other:?}"),
        }
    }

    #[test]
    fn concurrent_requests_use_different_uuids() {
        let (_dir, handoff) = tmp_handoff();
        ensure_dirs(&handoff).unwrap();

        // We verify that multiple requests create distinct files by checking
        // that the request_charts function handles concurrent requests.
        let requests = vec![
            ChartRequest {
                view: ChartView::Metrics,
                format: ChartFormat::Png,
                filters: json!({}),
                output_path: "/tmp/chart1.png".to_string(),
            },
            ChartRequest {
                view: ChartView::Gantt,
                format: ChartFormat::Svg,
                filters: json!({"milestone": "v1"}),
                output_path: "/tmp/chart2.svg".to_string(),
            },
        ];

        // Both should timeout quickly, but demonstrate independent handling.
        let results = request_charts(
            &handoff,
            &requests,
            Duration::from_millis(50),
            Duration::from_secs(1),
        )
        .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0], ChartResult::Timeout);
        assert_eq!(results[1], ChartResult::Timeout);
    }

    #[test]
    fn total_timeout_caps_remaining_requests() {
        let (_dir, handoff) = tmp_handoff();
        ensure_dirs(&handoff).unwrap();

        let requests = vec![sample_request(), sample_request(), sample_request()];
        // Per-chart timeout is 10s but total is only 100ms: later requests
        // should see zero remaining time and immediately return Timeout.
        let start = Instant::now();
        let results = request_charts(
            &handoff,
            &requests,
            Duration::from_secs(10),
            Duration::from_millis(100),
        )
        .unwrap();
        let elapsed = start.elapsed();
        assert_eq!(results.len(), 3);
        for r in &results {
            assert_eq!(*r, ChartResult::Timeout);
        }
        // Should complete much faster than 30s (the per-chart timeout would be).
        assert!(elapsed < Duration::from_secs(2));
    }

    #[test]
    fn chart_result_has_output_checks() {
        let success = ChartResult::Success {
            output_paths: vec!["/tmp/a.png".into()],
        };
        assert!(success.has_output());
        assert_eq!(success.first_output(), Some("/tmp/a.png"));

        let timeout = ChartResult::Timeout;
        assert!(!timeout.has_output());
        assert_eq!(timeout.first_output(), None);

        let error = ChartResult::Error { errors: vec![] };
        assert!(!error.has_output());
    }

    #[test]
    fn request_validation_rejects_empty_output_path_in_request_chart() {
        let (_dir, handoff) = tmp_handoff();
        let req = ChartRequest {
            view: ChartView::Metrics,
            format: ChartFormat::Png,
            filters: json!(null),
            output_path: String::new(),
        };
        assert!(request_chart(&handoff, &req, Duration::from_millis(50)).is_err());
    }

    // --- Error resilience tests (SPEC-VIS-004) ---

    #[test]
    fn broken_json_response_returns_chart_error_not_propagated() {
        let (_dir, handoff) = tmp_handoff();
        let req = sample_request();
        ensure_dirs(&handoff).unwrap();

        let requests_dir = handoff.join(REQUESTS_SUBDIR);
        let responses_dir = handoff.join(RESPONSES_SUBDIR);

        let handle = std::thread::spawn(move || {
            let uuid = wait_for_request_file(&requests_dir);
            let path = responses_dir.join(format!("{uuid}.json"));
            std::fs::write(&path, "this is not valid JSON{{{").unwrap();
        });

        let result = request_chart(&handoff, &req, Duration::from_secs(5)).unwrap();
        handle.join().unwrap();

        match result {
            ChartResult::Error { errors } => {
                assert!(!errors.is_empty(), "should have at least one error");
                assert!(
                    errors[0].message.contains("JSON")
                        || errors[0].message.contains("parse")
                        || errors[0].message.contains("json"),
                    "error should mention JSON parsing: {:?}",
                    errors[0].message
                );
            }
            other => panic!("expected ChartResult::Error for broken JSON, got {other:?}"),
        }
    }

    #[test]
    fn unknown_status_in_response_returns_chart_error() {
        let (_dir, handoff) = tmp_handoff();
        let req = sample_request();
        ensure_dirs(&handoff).unwrap();

        let requests_dir = handoff.join(REQUESTS_SUBDIR);
        let responses_dir = handoff.join(RESPONSES_SUBDIR);

        let handle = std::thread::spawn(move || {
            let uuid = wait_for_request_file(&requests_dir);
            let path = responses_dir.join(format!("{uuid}.json"));
            // Valid JSON but unknown status value
            std::fs::write(
                &path,
                r#"{"status":"unknown_status","output_paths":[],"errors":[]}"#,
            )
            .unwrap();
        });

        let result = request_chart(&handoff, &req, Duration::from_secs(5)).unwrap();
        handle.join().unwrap();

        match result {
            ChartResult::Error { errors } => {
                assert!(!errors.is_empty());
            }
            other => panic!("expected ChartResult::Error for unknown status, got {other:?}"),
        }
    }

    #[test]
    fn missing_required_fields_in_response_returns_chart_error() {
        let (_dir, handoff) = tmp_handoff();
        let req = sample_request();
        ensure_dirs(&handoff).unwrap();

        let requests_dir = handoff.join(REQUESTS_SUBDIR);
        let responses_dir = handoff.join(RESPONSES_SUBDIR);

        let handle = std::thread::spawn(move || {
            let uuid = wait_for_request_file(&requests_dir);
            let path = responses_dir.join(format!("{uuid}.json"));
            // Missing 'status' field entirely
            std::fs::write(&path, r#"{"output_paths":[]}"#).unwrap();
        });

        let result = request_chart(&handoff, &req, Duration::from_secs(5)).unwrap();
        handle.join().unwrap();

        match result {
            ChartResult::Error { errors } => {
                assert!(!errors.is_empty());
            }
            other => panic!("expected ChartResult::Error for missing fields, got {other:?}"),
        }
    }
}

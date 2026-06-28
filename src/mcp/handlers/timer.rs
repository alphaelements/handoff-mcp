use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::resolve_project_dir;
use crate::storage::config::read_config;
use crate::storage::ensure_handoff_exists;
use crate::storage::tasks::{find_task_dir_by_id, read_modify_write_task};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Authority {
    pub version: u32,
    pub owner: String,
    pub owner_instance: String,
    pub heartbeat_at: String,
    pub ttl_secs: u64,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerEntry {
    pub state: String,
    pub elapsed_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(default)]
    pub paused_by_idle: bool,
    #[serde(default)]
    pub base_hours: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerState {
    pub version: u32,
    pub owner: String,
    #[serde(default)]
    pub timers: std::collections::HashMap<String, TimerEntry>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimerRequest {
    pub version: u32,
    pub id: String,
    pub cmd: String,
    pub task_id: String,
    pub issued_by: String,
    pub issued_at: String,
}

fn timer_dir(handoff: &Path) -> PathBuf {
    handoff.join("timer")
}

fn ensure_timer_dir(handoff: &Path) -> Result<PathBuf> {
    let dir = timer_dir(handoff);
    if !dir.exists() {
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("Failed to create timer dir: {}", dir.display()))?;
    }
    let requests = dir.join("requests");
    if !requests.exists() {
        std::fs::create_dir_all(&requests)
            .with_context(|| format!("Failed to create requests dir: {}", requests.display()))?;
    }
    Ok(dir)
}

fn read_authority(timer: &Path) -> Result<Option<Authority>> {
    let path = timer.join("authority.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read authority: {}", path.display()))?;
    let auth: Authority =
        serde_json::from_str(&content).with_context(|| "Failed to parse authority.json")?;
    Ok(Some(auth))
}

fn write_authority(timer: &Path, auth: &Authority) -> Result<()> {
    let path = timer.join("authority.json");
    let content =
        serde_json::to_string_pretty(auth).with_context(|| "Failed to serialize authority")?;
    crate::storage::atomic_write(&path, content.as_bytes())
}

fn read_state(timer: &Path) -> Result<Option<TimerState>> {
    let path = timer.join("state.json");
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read state: {}", path.display()))?;
    let state: TimerState =
        serde_json::from_str(&content).with_context(|| "Failed to parse state.json")?;
    Ok(Some(state))
}

fn write_state(timer: &Path, state: &TimerState) -> Result<()> {
    let path = timer.join("state.json");
    let content =
        serde_json::to_string_pretty(state).with_context(|| "Failed to serialize state")?;
    crate::storage::atomic_write(&path, content.as_bytes())
}

fn read_modify_write_state<F>(timer: &Path, mut mutate: F) -> Result<TimerState>
where
    F: FnMut(&mut TimerState) -> Result<()>,
{
    const MAX_RETRIES: usize = 5;
    for attempt in 0..=MAX_RETRIES {
        let mut state = read_state(timer)?.unwrap_or_else(|| TimerState {
            version: 1,
            owner: "mcp".to_string(),
            timers: std::collections::HashMap::new(),
            updated_at: Utc::now().to_rfc3339(),
        });
        let snapshot = state.updated_at.clone();
        mutate(&mut state)?;
        state.updated_at = Utc::now().to_rfc3339();

        let current = read_state(timer)?.map(|s| s.updated_at);
        if current.as_deref() != Some(&snapshot) && current.is_some() {
            if attempt == MAX_RETRIES {
                anyhow::bail!("Concurrent modification of state.json after {MAX_RETRIES} retries");
            }
            continue;
        }
        write_state(timer, &state)?;
        return Ok(state);
    }
    unreachable!()
}

fn write_request(timer: &Path, req: &TimerRequest) -> Result<()> {
    let path = timer.join("requests").join(format!("{}.json", req.id));
    let content =
        serde_json::to_string_pretty(req).with_context(|| "Failed to serialize request")?;
    crate::storage::atomic_write(&path, content.as_bytes())
}

fn generate_request_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let now = Utc::now();
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    format!(
        "{}-{}-{seq}",
        now.format("%Y%m%d%H%M%S%9f"),
        std::process::id()
    )
}

fn is_authority_alive(auth: &Authority) -> bool {
    let Ok(heartbeat) = DateTime::parse_from_rfc3339(&auth.heartbeat_at) else {
        return false;
    };
    let elapsed = Utc::now()
        .signed_duration_since(heartbeat)
        .num_seconds()
        .unsigned_abs();
    elapsed <= auth.ttl_secs
}

enum TimerProvider {
    Vscode { authority_alive: bool },
    McpFallback,
}

fn determine_provider(
    timer: &Path,
    config_provider: &str,
    ttl_secs: u64,
) -> Result<(TimerProvider, Option<Authority>)> {
    match config_provider {
        "off" => anyhow::bail!("Timer is disabled (timer_provider = off)"),
        "vscode" => {
            let auth = read_authority(timer)?;
            let alive = auth.as_ref().map(is_authority_alive).unwrap_or(false);
            Ok((
                TimerProvider::Vscode {
                    authority_alive: alive,
                },
                auth,
            ))
        }
        "mcp" => {
            let auth = read_authority(timer)?;
            Ok((TimerProvider::McpFallback, auth))
        }
        _ => {
            let auth = read_authority(timer)?;
            match &auth {
                Some(a) if a.owner == "vscode" && is_authority_alive(a) => Ok((
                    TimerProvider::Vscode {
                        authority_alive: true,
                    },
                    auth,
                )),
                Some(a) if a.owner == "mcp" && is_authority_alive(a) => {
                    Ok((TimerProvider::McpFallback, auth))
                }
                _ => {
                    let new_auth = Authority {
                        version: 1,
                        owner: "mcp".to_string(),
                        owner_instance: std::process::id().to_string(),
                        heartbeat_at: Utc::now().to_rfc3339(),
                        ttl_secs,
                        updated_at: Utc::now().to_rfc3339(),
                    };
                    write_authority(timer, &new_auth)?;
                    Ok((TimerProvider::McpFallback, Some(new_auth)))
                }
            }
        }
    }
}

fn update_mcp_heartbeat(timer: &Path, ttl_secs: u64) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let auth = Authority {
        version: 1,
        owner: "mcp".to_string(),
        owner_instance: std::process::id().to_string(),
        heartbeat_at: now.clone(),
        ttl_secs,
        updated_at: now,
    };
    write_authority(timer, &auth)
}

fn get_base_hours(tasks_dir: &Path, task_id: &str) -> Result<f64> {
    let task_dir = find_task_dir_by_id(tasks_dir, task_id)?
        .ok_or_else(|| anyhow::anyhow!("Task not found: {task_id}"))?;
    let (data, _status) = crate::storage::tasks::read_task(&task_dir)?
        .ok_or_else(|| anyhow::anyhow!("Task file not found: {task_id}"))?;
    Ok(data
        .schedule
        .as_ref()
        .and_then(|s| s.actual_hours)
        .unwrap_or(0.0))
}

pub fn handle_start(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");
    let config_path = handoff.join("config.toml");
    let config = read_config(&config_path)?;

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' parameter is required"))?;

    find_task_dir_by_id(&tasks_dir, task_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Task not found: {task_id}. Use handoff_list_tasks to see available task IDs."
        )
    })?;

    let timer = ensure_timer_dir(&handoff)?;
    let ttl = config.settings.timer_authority_ttl_secs;

    let (provider, _auth) = determine_provider(&timer, &config.settings.timer_provider, ttl)?;

    match provider {
        TimerProvider::Vscode { authority_alive } => {
            let req = TimerRequest {
                version: 1,
                id: generate_request_id(),
                cmd: "start".to_string(),
                task_id: task_id.to_string(),
                issued_by: "mcp".to_string(),
                issued_at: Utc::now().to_rfc3339(),
            };
            write_request(&timer, &req)?;
            let warning = if !authority_alive {
                " WARNING: VSCode extension authority is stale or absent — the request may not be processed."
            } else {
                ""
            };
            Ok(format!(
                "Timer start delegated to VSCode extension for task {task_id} (request {}).{warning}",
                req.id
            ))
        }
        TimerProvider::McpFallback => {
            update_mcp_heartbeat(&timer, ttl)?;

            if let Some(existing_state) = read_state(&timer)? {
                if let Some(existing) = existing_state.timers.get(task_id) {
                    if existing.state == "tracking" {
                        return Ok(format!(
                            "Timer already running for task {task_id} (elapsed: {:.1}s)",
                            compute_live_elapsed_ms(existing) as f64 / 1000.0
                        ));
                    }
                }
            }

            let base = get_base_hours(&tasks_dir, task_id)?;
            let task_id_owned = task_id.to_string();
            read_modify_write_state(&timer, |state| {
                let now = Utc::now().to_rfc3339();
                state.timers.insert(
                    task_id_owned.clone(),
                    TimerEntry {
                        state: "tracking".to_string(),
                        elapsed_ms: 0,
                        started_at: Some(now),
                        paused_by_idle: false,
                        base_hours: base,
                    },
                );
                state.owner = "mcp".to_string();
                Ok(())
            })?;

            Ok(format!(
                "MCP fallback timer started for task {task_id} (base_hours: {base:.2}h)"
            ))
        }
    }
}

fn compute_live_elapsed_ms(entry: &TimerEntry) -> u64 {
    if entry.state != "tracking" {
        return entry.elapsed_ms;
    }
    let Some(ref started) = entry.started_at else {
        return entry.elapsed_ms;
    };
    let Ok(started_dt) = DateTime::parse_from_rfc3339(started) else {
        return entry.elapsed_ms;
    };
    let running_ms = Utc::now()
        .signed_duration_since(started_dt)
        .num_milliseconds()
        .max(0) as u64;
    entry.elapsed_ms + running_ms
}

pub fn handle_stop(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let tasks_dir = handoff.join("tasks");
    let config_path = handoff.join("config.toml");
    let config = read_config(&config_path)?;

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' parameter is required"))?;

    let task_dir = find_task_dir_by_id(&tasks_dir, task_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Task not found: {task_id}. Use handoff_list_tasks to see available task IDs."
        )
    })?;

    let timer = ensure_timer_dir(&handoff)?;
    let ttl = config.settings.timer_authority_ttl_secs;

    let (provider, _auth) = determine_provider(&timer, &config.settings.timer_provider, ttl)?;

    match provider {
        TimerProvider::Vscode { authority_alive } => {
            let req = TimerRequest {
                version: 1,
                id: generate_request_id(),
                cmd: "stop".to_string(),
                task_id: task_id.to_string(),
                issued_by: "mcp".to_string(),
                issued_at: Utc::now().to_rfc3339(),
            };
            write_request(&timer, &req)?;
            let warning = if !authority_alive {
                " WARNING: VSCode extension authority is stale or absent — the request may not be processed."
            } else {
                ""
            };
            Ok(format!(
                "Timer stop delegated to VSCode extension for task {task_id} (request {}).{warning}",
                req.id
            ))
        }
        TimerProvider::McpFallback => {
            update_mcp_heartbeat(&timer, ttl)?;

            let state = read_state(&timer)?.unwrap_or_else(|| TimerState {
                version: 1,
                owner: "mcp".to_string(),
                timers: std::collections::HashMap::new(),
                updated_at: Utc::now().to_rfc3339(),
            });

            let entry = state.timers.get(task_id).ok_or_else(|| {
                anyhow::anyhow!(
                    "No active timer for task {task_id}. Start one first with handoff_timer_start."
                )
            })?;

            if entry.state != "tracking" {
                anyhow::bail!(
                    "Timer for task {task_id} is not running (state: {})",
                    entry.state
                );
            }

            let total_ms = compute_live_elapsed_ms(entry);
            let hours_to_add = total_ms as f64 / 3_600_000.0;

            // Crash-safe ordering: remove timer entry FIRST (via optimistic lock),
            // then add actual_hours. If crash occurs between, the timer is already
            // gone (hours are lost rather than double-counted).
            let task_id_owned = task_id.to_string();
            read_modify_write_state(&timer, |s| {
                s.timers.remove(&task_id_owned);
                Ok(())
            })?;

            let new_actual = std::cell::Cell::new(0.0_f64);
            let new_remaining: std::cell::Cell<Option<f64>> = std::cell::Cell::new(None);

            read_modify_write_task(&task_dir, |data, status| {
                let schedule = data.schedule.get_or_insert_with(Default::default);
                let actual = schedule.actual_hours.unwrap_or(0.0) + hours_to_add;
                schedule.actual_hours = Some(actual);
                new_actual.set(actual);

                if let Some(rem) = schedule.remaining_hours {
                    let r = (rem - hours_to_add).max(0.0);
                    schedule.remaining_hours = Some(r);
                    new_remaining.set(Some(r));
                } else {
                    new_remaining.set(None);
                }

                data.updated_at = Some(Utc::now().to_rfc3339());
                Ok(status.to_string())
            })?;

            let remaining_msg = match new_remaining.get() {
                Some(r) => format!(", remaining={r:.2}h"),
                None => String::new(),
            };

            Ok(format!(
                "MCP fallback timer stopped for task {task_id}: +{hours_to_add:.4}h logged, actual={:.2}h{remaining_msg}",
                new_actual.get()
            ))
        }
    }
}

pub fn handle_get_time(arguments: &Value) -> Result<String> {
    let project_dir = resolve_project_dir(arguments)?;
    let handoff = ensure_handoff_exists(&project_dir)?;
    let config_path = handoff.join("config.toml");
    let config = read_config(&config_path)?;

    let task_id = arguments
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("'task_id' parameter is required"))?;

    let tasks_dir = handoff.join("tasks");
    find_task_dir_by_id(&tasks_dir, task_id)?.ok_or_else(|| {
        anyhow::anyhow!(
            "Task not found: {task_id}. Use handoff_list_tasks to see available task IDs."
        )
    })?;

    if config.settings.timer_provider == "off" {
        anyhow::bail!("Timer is disabled (timer_provider = off)");
    }

    let timer = timer_dir(&handoff);
    if !timer.exists() {
        return Ok(serde_json::to_string_pretty(&serde_json::json!({
            "task_id": task_id,
            "state": "stopped",
            "elapsed_ms": 0,
            "elapsed_hours": 0.0,
            "authority": null,
            "message": "No timer has been started for this task"
        }))?);
    }

    let auth = read_authority(&timer)?;
    let authority_owner = auth.as_ref().map(|a| a.owner.as_str());
    let authority_alive = auth.as_ref().map(is_authority_alive).unwrap_or(false);

    let state = read_state(&timer)?;
    let entry = state.as_ref().and_then(|s| s.timers.get(task_id));

    match entry {
        Some(e) => {
            let live_ms = compute_live_elapsed_ms(e);
            let elapsed_hours = live_ms as f64 / 3_600_000.0;
            let current_actual = get_base_hours(&tasks_dir, task_id).unwrap_or(e.base_hours);
            let projected_total = current_actual + elapsed_hours;
            Ok(serde_json::to_string_pretty(&serde_json::json!({
                "task_id": task_id,
                "state": e.state,
                "elapsed_ms": live_ms,
                "elapsed_hours": format!("{elapsed_hours:.4}"),
                "base_hours": e.base_hours,
                "current_actual_hours": current_actual,
                "projected_total_hours": format!("{projected_total:.4}"),
                "paused_by_idle": e.paused_by_idle,
                "authority": {
                    "owner": authority_owner,
                    "alive": authority_alive
                }
            }))?)
        }
        None => Ok(serde_json::to_string_pretty(&serde_json::json!({
            "task_id": task_id,
            "state": "stopped",
            "elapsed_ms": 0,
            "elapsed_hours": 0.0,
            "authority": {
                "owner": authority_owner,
                "alive": authority_alive
            },
            "message": "No active timer for this task"
        }))?),
    }
}

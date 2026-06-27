use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

const HOOK_SERVER: &str = "handoff";
const HOOK_TOOL_QUERY: &str = "handoff_memory_query";
const HOOK_TOOL_CLEANUP: &str = "handoff_memory_cleanup";

fn settings_path() -> Result<PathBuf> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .context("cannot determine home directory (HOME / USERPROFILE not set)")?;
    Ok(Path::new(&home).join(".claude").join("settings.json"))
}

fn read_settings(path: &Path) -> Result<Value> {
    if path.exists() {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        serde_json::from_str(&text).with_context(|| format!("failed to parse {}", path.display()))
    } else {
        Ok(Value::Object(serde_json::Map::new()))
    }
}

fn write_settings(path: &Path, value: &Value) -> Result<()> {
    let parent = path
        .parent()
        .context("settings path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;

    let text = serde_json::to_string_pretty(value)?;
    let tmp = parent.join(".settings.json.tmp");
    std::fs::write(&tmp, text + "\n")
        .with_context(|| format!("failed to write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("failed to rename {} -> {}", tmp.display(), path.display()))
}

fn mcp_tool_hook(tool: &str, input: Value) -> Value {
    serde_json::json!({
        "type": "mcp_tool",
        "server": HOOK_SERVER,
        "tool": tool,
        "input": input
    })
}

fn build_hooks_config() -> BTreeMap<&'static str, Value> {
    let mut hooks = BTreeMap::new();

    hooks.insert(
        "UserPromptSubmit",
        serde_json::json!([{
            "hooks": [mcp_tool_hook(HOOK_TOOL_QUERY, serde_json::json!({
                "project_dir": "${cwd}",
                "session_id": "${session_id}",
                "text": "${prompt}"
            }))]
        }]),
    );

    hooks.insert(
        "PreToolUse",
        serde_json::json!([{
            "matcher": "Edit|Write|MultiEdit",
            "hooks": [mcp_tool_hook(HOOK_TOOL_QUERY, serde_json::json!({
                "project_dir": "${cwd}",
                "session_id": "${session_id}",
                "tool_name": "${tool_name}",
                "text": "${tool_input.file_path}",
                "file_paths": ["${tool_input.file_path}"]
            }))]
        }]),
    );

    hooks.insert(
        "SessionStart",
        serde_json::json!([{
            "hooks": [mcp_tool_hook(HOOK_TOOL_CLEANUP, serde_json::json!({
                "project_dir": "${cwd}"
            }))]
        }]),
    );

    hooks
}

fn has_handoff_hook(arr: &Value) -> bool {
    let Some(entries) = arr.as_array() else {
        return false;
    };
    for entry in entries {
        let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) else {
            continue;
        };
        for hook in hooks {
            if hook.get("server").and_then(|v| v.as_str()) == Some(HOOK_SERVER) {
                return true;
            }
        }
    }
    false
}

pub fn run_setup(check_only: bool, uninstall: bool) -> Result<()> {
    anyhow::ensure!(
        !(check_only && uninstall),
        "--check and --uninstall cannot be used together"
    );

    let path = settings_path()?;
    let mut settings = read_settings(&path)?;

    if check_only {
        return run_check(&settings, &path);
    }

    if uninstall {
        return run_uninstall(&mut settings, &path);
    }

    run_install(&mut settings, &path)
}

fn run_check(settings: &Value, path: &Path) -> Result<()> {
    println!("Settings file: {}", path.display());

    let hooks_obj = settings.get("hooks");
    let desired = build_hooks_config();
    let mut all_ok = true;

    for event in desired.keys() {
        let installed = hooks_obj
            .and_then(|h| h.get(*event))
            .map(has_handoff_hook)
            .unwrap_or(false);

        if installed {
            println!("  {event}: installed");
        } else {
            println!("  {event}: NOT installed");
            all_ok = false;
        }
    }

    if all_ok {
        println!("\nAll hooks are configured. Memory auto-injection is active.");
    } else {
        println!("\nSome hooks are missing. Run `handoff-mcp setup` to install them.");
    }

    Ok(())
}

fn run_install(settings: &mut Value, path: &Path) -> Result<()> {
    let obj = settings
        .as_object_mut()
        .context("settings.json root is not an object")?;

    let hooks_val = obj
        .entry("hooks")
        .or_insert_with(|| Value::Object(serde_json::Map::new()));
    let hooks_obj = hooks_val
        .as_object_mut()
        .context("settings.json 'hooks' is not an object")?;

    let desired = build_hooks_config();
    let mut installed = 0u32;
    let mut skipped = 0u32;

    for (event, config) in desired {
        let existing = hooks_obj.get(event);
        if existing.map(has_handoff_hook).unwrap_or(false) {
            println!("  {event}: already installed, skipping");
            skipped += 1;
            continue;
        }

        match existing {
            Some(Value::Array(arr)) => {
                let mut merged = arr.clone();
                if let Some(new_entries) = config.as_array() {
                    merged.extend(new_entries.iter().cloned());
                }
                hooks_obj.insert(event.to_string(), Value::Array(merged));
                println!("  {event}: merged with existing hooks");
            }
            _ => {
                hooks_obj.insert(event.to_string(), config);
                println!("  {event}: installed");
            }
        }
        installed += 1;
    }

    if installed > 0 {
        write_settings(path, settings)?;
        println!("\nWrote {path}", path = path.display());
        println!("{installed} hook(s) installed, {skipped} already present.");
        println!("\nRestart Claude Code for hooks to take effect.");
    } else {
        println!("\nAll hooks already installed. Nothing to do.");
    }

    Ok(())
}

fn run_uninstall(settings: &mut Value, path: &Path) -> Result<()> {
    let Some(hooks_obj) = settings.get_mut("hooks").and_then(|v| v.as_object_mut()) else {
        println!("No hooks configured. Nothing to remove.");
        return Ok(());
    };

    let events: Vec<String> = hooks_obj.keys().cloned().collect();
    let mut removed = 0u32;

    for event in &events {
        let Some(arr) = hooks_obj.get_mut(event).and_then(|v| v.as_array_mut()) else {
            continue;
        };

        let before = arr.len();
        arr.retain(|entry| {
            let Some(hooks) = entry.get("hooks").and_then(|v| v.as_array()) else {
                return true;
            };
            !hooks
                .iter()
                .any(|h| h.get("server").and_then(|v| v.as_str()) == Some(HOOK_SERVER))
        });

        let after = arr.len();
        if before != after {
            println!("  {event}: removed handoff hook(s)");
            removed += 1;
        }

        if arr.is_empty() {
            hooks_obj.remove(event);
        }
    }

    if hooks_obj.is_empty() {
        if let Some(obj) = settings.as_object_mut() {
            obj.remove("hooks");
        }
    }

    if removed > 0 {
        write_settings(path, settings)?;
        println!("\nWrote {path}", path = path.display());
        println!("{removed} hook event(s) cleaned up.");
        println!("\nRestart Claude Code for changes to take effect.");
    } else {
        println!("No handoff hooks found. Nothing to remove.");
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_hooks_has_three_events() {
        let hooks = build_hooks_config();
        assert_eq!(hooks.len(), 3);
        assert!(hooks.contains_key("UserPromptSubmit"));
        assert!(hooks.contains_key("PreToolUse"));
        assert!(hooks.contains_key("SessionStart"));
    }

    #[test]
    fn has_handoff_hook_detects_presence() {
        let arr = serde_json::json!([{
            "hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query"}]
        }]);
        assert!(has_handoff_hook(&arr));
    }

    #[test]
    fn has_handoff_hook_returns_false_for_other_server() {
        let arr = serde_json::json!([{
            "hooks": [{"type": "mcp_tool", "server": "other", "tool": "other_tool"}]
        }]);
        assert!(!has_handoff_hook(&arr));
    }

    #[test]
    fn has_handoff_hook_returns_false_for_empty() {
        assert!(!has_handoff_hook(&serde_json::json!([])));
    }

    #[test]
    fn install_into_empty_settings() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Value::Object(serde_json::Map::new());
        run_install(&mut settings, &path).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let hooks = written.get("hooks").unwrap().as_object().unwrap();
        assert!(hooks.contains_key("UserPromptSubmit"));
        assert!(hooks.contains_key("PreToolUse"));
        assert!(hooks.contains_key("SessionStart"));
    }

    #[test]
    fn install_merges_with_existing_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [{
                    "hooks": [{"type": "command", "command": "my-other-hook"}]
                }]
            }
        });

        run_install(&mut settings, &path).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let user_prompt = written["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(user_prompt.len(), 2);
        assert!(has_handoff_hook(&written["hooks"]["UserPromptSubmit"]));
    }

    #[test]
    fn install_skips_if_already_present() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Value::Object(serde_json::Map::new());
        run_install(&mut settings, &path).unwrap();

        let mut settings2 = serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        run_install(&mut settings2, &path).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let user_prompt = written["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(user_prompt.len(), 1);
    }

    #[test]
    fn uninstall_removes_handoff_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = Value::Object(serde_json::Map::new());
        run_install(&mut settings, &path).unwrap();

        let mut settings2: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        run_uninstall(&mut settings2, &path).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert!(written.get("hooks").is_none());
    }

    #[test]
    fn check_and_uninstall_conflict_is_rejected() {
        assert!(run_setup(true, true).is_err());
    }

    #[test]
    fn install_preserves_key_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let original = r#"{
  "env": {"A": "1", "B": "2"},
  "model": "opus",
  "permissions": {"defaultMode": "auto"}
}
"#;
        std::fs::write(&path, original).unwrap();

        let mut settings: Value = serde_json::from_str(original).unwrap();
        run_install(&mut settings, &path).unwrap();

        let written = std::fs::read_to_string(&path).unwrap();
        let env_pos = written.find("\"env\"").unwrap();
        let hooks_pos = written.find("\"hooks\"").unwrap();
        let model_pos = written.find("\"model\"").unwrap();
        assert!(env_pos < model_pos, "env should come before model");
        assert!(
            hooks_pos > env_pos,
            "hooks should be appended after existing keys"
        );
    }

    #[test]
    fn uninstall_preserves_other_hooks() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");

        let mut settings = serde_json::json!({
            "hooks": {
                "UserPromptSubmit": [
                    {"hooks": [{"type": "command", "command": "my-hook"}]},
                    {"hooks": [{"type": "mcp_tool", "server": "handoff", "tool": "handoff_memory_query"}]}
                ]
            }
        });
        run_uninstall(&mut settings, &path).unwrap();

        let written: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        let user_prompt = written["hooks"]["UserPromptSubmit"].as_array().unwrap();
        assert_eq!(user_prompt.len(), 1);
        assert!(!has_handoff_hook(&written["hooks"]["UserPromptSubmit"]));
    }
}

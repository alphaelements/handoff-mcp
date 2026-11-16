//! The record of a milestone's pre-release check:
//! `.handoff/release/<milestone>.json` (SPEC-ONB-040).
//!
//! It holds the last judged result of each command (status, exit code, who
//! ran it, when) and the items a person confirmed (when). The
//! `handoff_release_check` tool writes it whenever results or confirmations
//! are passed in; releasing a milestone reads it instead of running anything
//! (SPEC-ONB-041).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::setup::doctor::{CheckStatus, ResultSource};

/// Directory under `.handoff/` that holds one record per milestone.
pub const RELEASE_DIR: &str = "release";

/// What was checked for one milestone.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReleaseRecord {
    /// The milestone, as named in `[milestones.<name>]`.
    pub milestone: String,
    /// When the record was last written (RFC 3339).
    #[serde(default)]
    pub updated_at: String,
    /// Last judged result of each command item, keyed by item id
    /// (`tests_pass`, ...).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub commands: BTreeMap<String, CommandRecord>,
    /// Items a person confirmed, keyed by item id (`manual:<name>`).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub manual: BTreeMap<String, ManualRecord>,
}

/// The judged result of one command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandRecord {
    pub status: CheckStatus,
    /// The command line that was run. A record only counts for the command
    /// `[commands]` names when it is read back.
    pub command: String,
    /// Absent when the command did not run to completion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    /// Who ran the command.
    pub source: ResultSource,
    /// When the result was judged (RFC 3339).
    pub checked_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    /// What the judgment said (exit code, error, output tail).
    #[serde(default)]
    pub detail: String,
}

/// A person's confirmation of one manual item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManualRecord {
    /// When it was confirmed (RFC 3339).
    pub confirmed_at: String,
}

/// File name of a milestone's record. A milestone name is free text, so
/// everything but lowercase ASCII letters, digits, `-`, `_` and an inner `.`
/// is percent-encoded: the name can then neither leave the directory nor
/// collide with another milestone's on a file system that ignores case.
fn record_file_name(milestone: &str) -> String {
    let mut name = String::with_capacity(milestone.len() + 5);
    for (index, byte) in milestone.bytes().enumerate() {
        let plain = byte.is_ascii_lowercase()
            || byte.is_ascii_digit()
            || byte == b'-'
            || byte == b'_'
            || (byte == b'.' && index > 0);
        if plain {
            name.push(char::from(byte));
        } else {
            name.push_str(&format!("%{byte:02X}"));
        }
    }
    name.push_str(".json");
    name
}

/// Where the record of `milestone` is kept.
pub fn record_path(handoff_dir: &Path, milestone: &str) -> PathBuf {
    handoff_dir
        .join(RELEASE_DIR)
        .join(record_file_name(milestone))
}

/// The record of `milestone`, or `None` when nothing was recorded yet. A file
/// that cannot be read or parsed is an error: treating it as "nothing
/// recorded" would let the next write drop the confirmations it holds.
pub fn read_record(handoff_dir: &Path, milestone: &str) -> Result<Option<ReleaseRecord>> {
    let path = record_path(handoff_dir, milestone);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e)
                .with_context(|| format!("cannot read the release record {}", path.display()))
        }
    };
    let record: ReleaseRecord = serde_json::from_str(&text).with_context(|| {
        format!(
            "the release record {} is not valid; repair or delete it",
            path.display()
        )
    })?;
    Ok(Some(record))
}

/// Writes the record of its milestone, replacing the previous one.
pub fn write_record(handoff_dir: &Path, record: &ReleaseRecord) -> Result<()> {
    let path = record_path(handoff_dir, &record.milestone);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("cannot create {}", dir.display()))?;
    }
    let mut text = serde_json::to_string_pretty(record)?;
    text.push('\n');
    crate::storage::atomic_write(&path, text.as_bytes())
        .with_context(|| format!("cannot write the release record {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> ReleaseRecord {
        let mut record = ReleaseRecord {
            milestone: "v1.0".to_string(),
            updated_at: "2026-11-15T01:02:03+00:00".to_string(),
            ..ReleaseRecord::default()
        };
        record.commands.insert(
            "tests_pass".to_string(),
            CommandRecord {
                status: CheckStatus::Fail,
                command: "npm test".to_string(),
                exit_code: Some(1),
                source: ResultSource::Reported,
                checked_at: "2026-11-15T01:02:03+00:00".to_string(),
                duration_ms: Some(1200),
                detail: "exit code 1".to_string(),
            },
        );
        record.manual.insert(
            "manual:docs_updated".to_string(),
            ManualRecord {
                confirmed_at: "2026-11-15T01:00:00+00:00".to_string(),
            },
        );
        record
    }

    #[test]
    fn a_record_round_trips_through_its_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read_record(dir.path(), "v1.0").unwrap(), None);
        let record = sample();
        write_record(dir.path(), &record).unwrap();
        assert!(dir.path().join("release/v1.0.json").is_file());
        assert_eq!(read_record(dir.path(), "v1.0").unwrap(), Some(record));
        assert_eq!(read_record(dir.path(), "v2").unwrap(), None);
    }

    // SPEC-ONB-040 AC6
    #[test]
    fn the_file_holds_status_exit_code_source_and_time_of_each_command() {
        let dir = tempfile::tempdir().unwrap();
        write_record(dir.path(), &sample()).unwrap();
        let json: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(dir.path().join("release/v1.0.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "milestone": "v1.0",
                "updated_at": "2026-11-15T01:02:03+00:00",
                "commands": {
                    "tests_pass": {
                        "status": "fail",
                        "command": "npm test",
                        "exit_code": 1,
                        "source": "reported",
                        "checked_at": "2026-11-15T01:02:03+00:00",
                        "duration_ms": 1200,
                        "detail": "exit code 1"
                    }
                },
                "manual": {
                    "manual:docs_updated": { "confirmed_at": "2026-11-15T01:00:00+00:00" }
                }
            })
        );
    }

    #[test]
    fn a_broken_record_is_an_error_not_an_empty_record() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(RELEASE_DIR)).unwrap();
        std::fs::write(dir.path().join("release/v1.json"), "{ not json").unwrap();
        let err = format!("{:#}", read_record(dir.path(), "v1").unwrap_err());
        assert!(err.contains("v1.json"), "{err}");
        assert!(err.contains("repair or delete"), "{err}");
    }

    #[test]
    fn a_milestone_name_cannot_leave_the_directory_or_collide_by_case() {
        for (milestone, file) in [
            ("v0.48.0", "v0.48.0.json"),
            ("beta-1_x", "beta-1_x.json"),
            ("../../etc/passwd", "%2E.%2F..%2Fetc%2Fpasswd.json"),
            ("..", "%2E..json"),
            (".hidden", "%2Ehidden.json"),
            ("a/b\\c", "a%2Fb%5Cc.json"),
            ("V1", "%561.json"),
            ("リリース", "%E3%83%AA%E3%83%AA%E3%83%BC%E3%82%B9.json"),
            ("100%", "100%25.json"),
            ("", ".json"),
        ] {
            assert_eq!(record_file_name(milestone), file, "{milestone:?}");
            let path = record_path(Path::new("/h"), milestone);
            assert_eq!(
                path.parent(),
                Some(Path::new("/h/release")),
                "{milestone:?}"
            );
        }
        assert_ne!(record_file_name("V1"), record_file_name("v1"));
        assert_ne!(record_file_name("a/b"), record_file_name("a%2Fb"));
    }

    #[test]
    fn writing_replaces_the_previous_record_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let mut record = sample();
        write_record(dir.path(), &record).unwrap();
        record.commands.clear();
        record.updated_at = "later".to_string();
        write_record(dir.path(), &record).unwrap();
        assert_eq!(read_record(dir.path(), "v1.0").unwrap(), Some(record));
        let names: Vec<String> = std::fs::read_dir(dir.path().join(RELEASE_DIR))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["v1.0.json"]);
    }
}

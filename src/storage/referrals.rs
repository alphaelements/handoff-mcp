use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

const VALID_REFERRAL_TYPES: &[&str] = &["improvement", "bug", "request", "info"];
const VALID_REFERRAL_STATUSES: &[&str] = &["open", "acknowledged", "resolved"];

pub fn is_valid_referral_type(t: &str) -> bool {
    VALID_REFERRAL_TYPES.contains(&t)
}

pub fn is_valid_referral_status(s: &str) -> bool {
    VALID_REFERRAL_STATUSES.contains(&s)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferralData {
    pub id: String,
    pub source_project: String,
    pub source_project_dir: String,
    pub created_at: String,
    pub referral_type: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tasks: Vec<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferralSummary {
    pub id: String,
    pub source_project: String,
    pub referral_type: String,
    pub summary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    pub status: String,
    pub created_at: String,
}

pub fn write_referral(referrals_dir: &Path, data: &ReferralData) -> Result<PathBuf> {
    std::fs::create_dir_all(referrals_dir).with_context(|| {
        format!(
            "Failed to create referrals dir: {}",
            referrals_dir.display()
        )
    })?;

    let slug = source_to_slug(&data.source_project);
    let id_suffix = data.id.replace("ref-", "");
    let filename = format!("{id_suffix}-{slug}.open.json");
    let file_path = referrals_dir.join(&filename);

    let content = serde_json::to_string_pretty(data).context("Failed to serialize referral")?;
    std::fs::write(&file_path, content)
        .with_context(|| format!("Failed to write referral: {}", file_path.display()))?;

    Ok(file_path)
}

pub fn read_referrals(
    referrals_dir: &Path,
    status_filter: Option<&str>,
) -> Result<Vec<(ReferralData, String)>> {
    let mut results = Vec::new();

    if !referrals_dir.exists() {
        return Ok(results);
    }

    let mut entries: Vec<_> = std::fs::read_dir(referrals_dir)?
        .filter_map(|e| e.ok())
        .collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(status) = parse_referral_status(&name) {
            if let Some(filter) = status_filter {
                if status != filter {
                    continue;
                }
            }
            let content = std::fs::read_to_string(entry.path())
                .with_context(|| format!("Failed to read referral: {}", entry.path().display()))?;
            let data: ReferralData = serde_json::from_str(&content)
                .with_context(|| format!("Failed to parse referral: {}", entry.path().display()))?;
            results.push((data, status));
        }
    }

    Ok(results)
}

pub fn read_referral_summaries(
    referrals_dir: &Path,
    status_filter: Option<&str>,
) -> Result<Vec<ReferralSummary>> {
    let referrals = read_referrals(referrals_dir, status_filter)?;
    Ok(referrals
        .into_iter()
        .map(|(data, status)| ReferralSummary {
            id: data.id,
            source_project: data.source_project,
            referral_type: data.referral_type,
            summary: data.summary,
            priority: data.priority,
            status,
            created_at: data.created_at,
        })
        .collect())
}

pub fn change_referral_status(
    referrals_dir: &Path,
    referral_id: &str,
    new_status: &str,
) -> Result<()> {
    if !is_valid_referral_status(new_status) {
        anyhow::bail!(
            "Invalid referral status: '{new_status}'. Must be one of: {}",
            VALID_REFERRAL_STATUSES.join(", ")
        );
    }

    let (old_path, old_status) = find_referral_file(referrals_dir, referral_id)?
        .ok_or_else(|| anyhow::anyhow!("Referral not found: {referral_id}"))?;

    if old_status == new_status {
        return Ok(());
    }

    let old_name = old_path
        .file_name()
        .ok_or_else(|| anyhow::anyhow!("Invalid referral path"))?
        .to_string_lossy()
        .to_string();

    let new_name = old_name.replace(
        &format!(".{old_status}.json"),
        &format!(".{new_status}.json"),
    );
    let new_path = referrals_dir.join(&new_name);

    std::fs::rename(&old_path, &new_path).with_context(|| {
        format!(
            "Failed to rename {} -> {}",
            old_path.display(),
            new_path.display()
        )
    })?;

    Ok(())
}

pub fn find_referral_file(
    referrals_dir: &Path,
    referral_id: &str,
) -> Result<Option<(PathBuf, String)>> {
    if !referrals_dir.exists() {
        return Ok(None);
    }

    for entry in std::fs::read_dir(referrals_dir)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().to_string();
        if let Some(status) = parse_referral_status(&name) {
            let content = std::fs::read_to_string(entry.path())?;
            if let Ok(data) = serde_json::from_str::<ReferralData>(&content) {
                if data.id == referral_id {
                    return Ok(Some((entry.path(), status)));
                }
            }
        }
    }

    Ok(None)
}

fn parse_referral_status(filename: &str) -> Option<String> {
    let name = filename.strip_suffix(".json")?;
    for status in VALID_REFERRAL_STATUSES {
        if name.ends_with(&format!(".{status}")) {
            return Some(status.to_string());
        }
    }
    None
}

fn source_to_slug(source: &str) -> String {
    let slug: String = source
        .chars()
        .take(30)
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect();
    slug.trim_matches('-').to_string()
}

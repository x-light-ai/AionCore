// FORK-CUSTOM: Skill market metadata (version/tags) persisted on disk.
//
// Stored in a per-skill sidecar file `.aionui-market.json` rather than the
// upstream skill DB schema, so upstream migrations stay untouched and upstream
// merges stay clean. All fork-only skill-market logic lives in this module;
// `skill_service.rs` only calls into it from a couple of thin backfill points.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::ExtensionError;
use crate::skill_service::SkillPaths;

/// Name of the per-skill sidecar file holding fork market metadata.
pub const MARKET_METADATA_FILE: &str = ".aionui-market.json";

/// Market metadata persisted next to each user skill.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct PersistedSkillMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
}

/// Reject names that could escape the user skills directory. Kept local to
/// this module so it does not depend on upstream-private helpers.
fn validate_skill_name(name: &str) -> Result<(), ExtensionError> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(ExtensionError::PathTraversal(name.to_string()));
    }
    Ok(())
}

/// Persist market metadata (description/version/tags) for the given user
/// skills into their `.aionui-market.json` sidecar files. A no-op when all
/// fields are empty.
pub async fn persist_skill_market_metadata(
    paths: &SkillPaths,
    skill_names: &[String],
    description: Option<&str>,
    version: Option<&str>,
    tags: &[String],
) -> Result<(), ExtensionError> {
    let description = description.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    let version = version.map(str::trim).filter(|value| !value.is_empty()).map(str::to_string);
    let mut persisted_tags = Vec::new();
    for tag in tags.iter().map(|tag| tag.trim()).filter(|tag| !tag.is_empty()) {
        let tag = tag.to_string();
        if !persisted_tags.contains(&tag) {
            persisted_tags.push(tag);
        }
    }

    if description.is_none() && version.is_none() && persisted_tags.is_empty() {
        return Ok(());
    }

    let metadata = PersistedSkillMetadata {
        description,
        version,
        tags: persisted_tags,
    };
    let content = serde_json::to_string_pretty(&metadata)?;
    for skill_name in skill_names {
        validate_skill_name(skill_name)?;
        let skill_dir = paths.user_skills_dir.join(skill_name);
        if skill_dir.is_dir() {
            tokio::fs::write(skill_dir.join(MARKET_METADATA_FILE), &content).await?;
        }
    }

    Ok(())
}

/// Read market metadata from a skill directory's sidecar file, returning
/// defaults when absent or malformed.
pub async fn read_persisted_skill_metadata(skill_dir: &Path) -> PersistedSkillMetadata {
    match tokio::fs::read_to_string(skill_dir.join(MARKET_METADATA_FILE)).await {
        Ok(content) => serde_json::from_str(&content).unwrap_or_default(),
        Err(_) => PersistedSkillMetadata::default(),
    }
}

// FORK-CUSTOM: Skill market metadata (version/tags) persisted on disk.
//
// Stored in a per-skill sidecar file `.aionui-market.json` rather than the
// upstream skill DB schema, so upstream migrations stay untouched and upstream
// merges stay clean. All fork-only skill-market logic lives in this module.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use aionui_api_types::{XaiworkInstalledSkillMetadata, XaiworkSkillInstallSource, XaiworkSkillVisibility};
use aionui_extension::{ExtensionError, SkillPaths};

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
    #[serde(default)]
    pub source: XaiworkSkillInstallSource,
    #[serde(default)]
    pub visibility: XaiworkSkillVisibility,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assistant_ids: Vec<String>,
}

pub type InstalledSkillMetadataSnapshot = HashMap<String, Option<PersistedSkillMetadata>>;

/// Reject names that could escape the user skills directory. Kept local to
/// this module so it does not depend on upstream-private helpers.
fn validate_skill_name(name: &str) -> Result<(), ExtensionError> {
    if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(ExtensionError::PathTraversal(name.to_string()));
    }
    Ok(())
}

/// Persist market metadata (description/version/tags) for the given user
/// skills into their `.aionui-market.json` sidecar files.
pub async fn persist_skill_market_metadata(
    paths: &SkillPaths,
    skill_names: &[String],
    description: Option<&str>,
    version: Option<&str>,
    tags: &[String],
    previous: &InstalledSkillMetadataSnapshot,
) -> Result<(), ExtensionError> {
    let description = description
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let version = version
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let mut persisted_tags = Vec::new();
    for tag in tags.iter().map(|tag| tag.trim()).filter(|tag| !tag.is_empty()) {
        let tag = tag.to_string();
        if !persisted_tags.contains(&tag) {
            persisted_tags.push(tag);
        }
    }

    for skill_name in skill_names {
        validate_skill_name(skill_name)?;
        let skill_dir = paths.user_skills_dir.join(skill_name);
        if skill_dir.is_dir() {
            let assistant_ids = previous
                .get(skill_name)
                .and_then(Option::as_ref)
                .map(|metadata| metadata.assistant_ids.clone())
                .unwrap_or_default();
            let metadata = PersistedSkillMetadata {
                description: description.clone(),
                version: version.clone(),
                tags: persisted_tags.clone(),
                source: XaiworkSkillInstallSource::Market,
                visibility: XaiworkSkillVisibility::User,
                assistant_ids,
            };
            write_persisted_skill_metadata(&skill_dir, &metadata).await?;
        }
    }

    Ok(())
}

pub async fn persist_assistant_bundle_metadata(
    paths: &SkillPaths,
    skill_names: &[String],
    assistant_id: &str,
    previous: &InstalledSkillMetadataSnapshot,
) -> Result<(), ExtensionError> {
    for skill_name in skill_names {
        validate_skill_name(skill_name)?;
        let skill_dir = paths.user_skills_dir.join(skill_name);
        if !skill_dir.is_dir() {
            continue;
        }
        let mut metadata = match previous.get(skill_name) {
            Some(Some(metadata)) => metadata.clone(),
            Some(None) => {
                // A pre-existing local skill is user-owned even without a
                // market sidecar. Remove any sidecar copied from the bundle.
                let _ = tokio::fs::remove_file(skill_dir.join(MARKET_METADATA_FILE)).await;
                continue;
            }
            None => PersistedSkillMetadata {
                source: XaiworkSkillInstallSource::AssistantBundle,
                visibility: XaiworkSkillVisibility::Dependency,
                ..Default::default()
            },
        };
        if !metadata.assistant_ids.iter().any(|id| id == assistant_id) {
            metadata.assistant_ids.push(assistant_id.to_owned());
            metadata.assistant_ids.sort();
        }
        write_persisted_skill_metadata(&skill_dir, &metadata).await?;
    }
    Ok(())
}

pub async fn snapshot_installed_skill_metadata(paths: &SkillPaths) -> InstalledSkillMetadataSnapshot {
    let Ok(mut entries) = tokio::fs::read_dir(&paths.user_skills_dir).await else {
        return HashMap::new();
    };
    let mut snapshot = HashMap::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if file_type.is_dir() {
            snapshot.insert(
                entry.file_name().to_string_lossy().into_owned(),
                try_read_persisted_skill_metadata(&entry.path()).await,
            );
        }
    }
    snapshot
}

pub async fn list_installed_skill_metadata(paths: &SkillPaths) -> Vec<XaiworkInstalledSkillMetadata> {
    let Ok(mut entries) = tokio::fs::read_dir(&paths.user_skills_dir).await else {
        return Vec::new();
    };
    let mut result = Vec::new();
    while let Ok(Some(entry)) = entries.next_entry().await {
        let Ok(file_type) = entry.file_type().await else {
            continue;
        };
        if !file_type.is_dir() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        let sidecar = entry.path().join(MARKET_METADATA_FILE);
        let Ok(content) = tokio::fs::read_to_string(sidecar).await else {
            continue;
        };
        let Ok(metadata) = serde_json::from_str::<PersistedSkillMetadata>(&content) else {
            continue;
        };
        result.push(XaiworkInstalledSkillMetadata {
            name,
            description: metadata.description,
            version: metadata.version,
            tags: metadata.tags,
            source: metadata.source,
            visibility: metadata.visibility,
            assistant_ids: metadata.assistant_ids,
        });
    }
    result.sort_by(|a, b| a.name.cmp(&b.name));
    result
}

/// Read market metadata from a skill directory's sidecar file.
async fn try_read_persisted_skill_metadata(skill_dir: &Path) -> Option<PersistedSkillMetadata> {
    let content = tokio::fs::read_to_string(skill_dir.join(MARKET_METADATA_FILE))
        .await
        .ok()?;
    serde_json::from_str(&content).ok()
}

async fn write_persisted_skill_metadata(
    skill_dir: &Path,
    metadata: &PersistedSkillMetadata,
) -> Result<(), ExtensionError> {
    let content = serde_json::to_string_pretty(metadata)?;
    tokio::fs::write(skill_dir.join(MARKET_METADATA_FILE), content).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(root: &Path) -> SkillPaths {
        SkillPaths {
            data_dir: root.to_path_buf(),
            user_skills_dir: root.join("skills"),
            cron_skills_dir: root.join("cron-skills"),
            builtin_skills_dir: root.join("builtin-skills"),
            builtin_rules_dir: root.join("builtin-rules"),
            assistant_rules_dir: root.join("assistant-rules"),
            assistant_skills_dir: root.join("assistant-skills"),
        }
    }

    #[tokio::test]
    async fn market_metadata_survives_reload() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        tokio::fs::create_dir_all(paths.user_skills_dir.join("demo"))
            .await
            .unwrap();

        persist_skill_market_metadata(
            &paths,
            &["demo".to_owned()],
            Some("Demo skill"),
            Some("1.2.3"),
            &["office".to_owned()],
            &HashMap::new(),
        )
        .await
        .unwrap();

        let metadata = list_installed_skill_metadata(&paths).await;
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].version.as_deref(), Some("1.2.3"));
        assert_eq!(metadata[0].visibility, XaiworkSkillVisibility::User);
    }

    #[tokio::test]
    async fn assistant_bundle_marks_new_skill_as_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        tokio::fs::create_dir_all(paths.user_skills_dir.join("demo"))
            .await
            .unwrap();

        persist_assistant_bundle_metadata(&paths, &["demo".to_owned()], "assistant-1", &HashMap::new())
            .await
            .unwrap();

        let metadata = list_installed_skill_metadata(&paths).await;
        assert_eq!(metadata[0].source, XaiworkSkillInstallSource::AssistantBundle);
        assert_eq!(metadata[0].visibility, XaiworkSkillVisibility::Dependency);
        assert_eq!(metadata[0].assistant_ids, vec!["assistant-1"]);
    }

    #[tokio::test]
    async fn assistant_bundle_preserves_existing_market_ownership() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        tokio::fs::create_dir_all(paths.user_skills_dir.join("demo"))
            .await
            .unwrap();
        persist_skill_market_metadata(&paths, &["demo".to_owned()], None, Some("2.0.0"), &[], &HashMap::new())
            .await
            .unwrap();
        let previous = snapshot_installed_skill_metadata(&paths).await;

        persist_assistant_bundle_metadata(&paths, &["demo".to_owned()], "assistant-1", &previous)
            .await
            .unwrap();

        let metadata = list_installed_skill_metadata(&paths).await;
        assert_eq!(metadata[0].source, XaiworkSkillInstallSource::Market);
        assert_eq!(metadata[0].visibility, XaiworkSkillVisibility::User);
        assert_eq!(metadata[0].version.as_deref(), Some("2.0.0"));
        assert_eq!(metadata[0].assistant_ids, vec!["assistant-1"]);
    }

    #[tokio::test]
    async fn market_install_promotes_existing_assistant_dependency() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        tokio::fs::create_dir_all(paths.user_skills_dir.join("demo"))
            .await
            .unwrap();
        persist_assistant_bundle_metadata(&paths, &["demo".to_owned()], "assistant-1", &HashMap::new())
            .await
            .unwrap();
        let previous = snapshot_installed_skill_metadata(&paths).await;

        persist_skill_market_metadata(&paths, &["demo".to_owned()], None, Some("3.0.0"), &[], &previous)
            .await
            .unwrap();

        let metadata = list_installed_skill_metadata(&paths).await;
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].source, XaiworkSkillInstallSource::Market);
        assert_eq!(metadata[0].visibility, XaiworkSkillVisibility::User);
        assert_eq!(metadata[0].assistant_ids, vec!["assistant-1"]);
    }

    #[tokio::test]
    async fn assistant_bundle_does_not_hide_existing_local_skill() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let skill_dir = paths.user_skills_dir.join("demo");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        let previous = snapshot_installed_skill_metadata(&paths).await;
        tokio::fs::write(skill_dir.join(MARKET_METADATA_FILE), r#"{"visibility":"dependency"}"#)
            .await
            .unwrap();

        persist_assistant_bundle_metadata(&paths, &["demo".to_owned()], "assistant-1", &previous)
            .await
            .unwrap();

        assert!(!skill_dir.join(MARKET_METADATA_FILE).exists());
    }

    #[tokio::test]
    async fn malformed_sidecar_is_ignored() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let skill_dir = paths.user_skills_dir.join("demo");
        tokio::fs::create_dir_all(&skill_dir).await.unwrap();
        tokio::fs::write(skill_dir.join(MARKET_METADATA_FILE), "not json")
            .await
            .unwrap();

        assert!(list_installed_skill_metadata(&paths).await.is_empty());
    }

    #[tokio::test]
    async fn invalid_skill_name_is_rejected() {
        let temp = tempfile::tempdir().unwrap();
        let paths = paths(temp.path());
        let error =
            persist_skill_market_metadata(&paths, &["../escape".to_owned()], None, Some("1"), &[], &HashMap::new())
                .await
                .unwrap_err();

        assert!(matches!(error, ExtensionError::PathTraversal(_)));
    }
}

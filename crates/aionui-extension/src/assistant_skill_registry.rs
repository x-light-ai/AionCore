// FORK-CUSTOM: tracks skills imported as part of a remote assistant package,
// so they can be hidden from the "My Skills" list in settings.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tracing::warn;

/// Persisted set of skill names that were bundled inside a remote assistant
/// package.  Skills in this set are excluded from the "My Skills" listing so
/// they do not clutter the user's personal skill library.
///
/// Backed by a simple JSON array at `{data_dir}/assistant-bundled-skills.json`.
/// All operations are best-effort: load/save failures are logged at `warn`
/// and never propagate to callers.
pub struct AssistantSkillRegistry {
    path: PathBuf,
    names: HashSet<String>,
}

impl AssistantSkillRegistry {
    const FILE_NAME: &'static str = "assistant-bundled-skills.json";

    /// Load the registry from disk.  Returns an empty registry when the file
    /// does not exist or cannot be parsed.
    pub fn load(data_dir: &Path) -> Self {
        let path = data_dir.join(Self::FILE_NAME);
        let names = std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashSet<String>>(&s).ok())
            .unwrap_or_default();
        Self { path, names }
    }

    /// Register a batch of skill names as assistant-bundled.
    pub fn register(&mut self, names: &[String]) {
        for name in names {
            self.names.insert(name.clone());
        }
    }

    /// Returns `true` if `name` was imported as part of an assistant package.
    pub fn is_bundled(&self, name: &str) -> bool {
        self.names.contains(name)
    }

    /// Persist the registry to disk.  Failures are logged and silently ignored.
    pub async fn save(&self) {
        // Ensure the parent directory exists before writing.
        if let Some(parent) = self.path.parent() {
            if let Err(e) = tokio::fs::create_dir_all(parent).await {
                warn!(path = %parent.display(), error = %e, "assistant skill registry: failed to create dir");
                return;
            }
        }
        // Serialize as a sorted Vec for a stable, diff-friendly file.
        let mut sorted: Vec<&String> = self.names.iter().collect();
        sorted.sort();
        let json = match serde_json::to_string(&sorted) {
            Ok(s) => s,
            Err(e) => {
                warn!(error = %e, "assistant skill registry: failed to serialize");
                return;
            }
        };
        if let Err(e) = tokio::fs::write(&self.path, json).await {
            warn!(
                path = %self.path.display(),
                error = %e,
                "assistant skill registry: failed to save"
            );
        }
    }
}

// FORK-CUSTOM: unit tests for the assistant-bundled-skill registry persistence.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn load_missing_file_yields_empty_registry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let reg = AssistantSkillRegistry::load(tmp.path());
        assert!(!reg.is_bundled("anything"));
    }

    #[test]
    fn load_corrupt_file_yields_empty_registry() {
        let tmp = tempfile::TempDir::new().unwrap();
        let path = tmp.path().join(AssistantSkillRegistry::FILE_NAME);
        std::fs::write(&path, "not valid json {[").unwrap();
        // Corrupt content must not panic and must degrade to an empty set.
        let reg = AssistantSkillRegistry::load(tmp.path());
        assert!(!reg.is_bundled("demo-skill"));
    }

    #[test]
    fn register_marks_names_as_bundled() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mut reg = AssistantSkillRegistry::load(tmp.path());
        reg.register(&["a".to_string(), "b".to_string()]);
        assert!(reg.is_bundled("a"));
        assert!(reg.is_bundled("b"));
        assert!(!reg.is_bundled("c"));
    }

    #[tokio::test]
    async fn save_then_load_round_trips_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        {
            let mut reg = AssistantSkillRegistry::load(tmp.path());
            reg.register(&["skill-x".to_string(), "skill-y".to_string()]);
            reg.save().await;
        }
        let reloaded = AssistantSkillRegistry::load(tmp.path());
        assert!(reloaded.is_bundled("skill-x"));
        assert!(reloaded.is_bundled("skill-y"));
        assert!(!reloaded.is_bundled("skill-z"));
    }

    #[tokio::test]
    async fn save_creates_missing_parent_dir_and_sorts_names() {
        let tmp = tempfile::TempDir::new().unwrap();
        // data_dir does not exist yet; save() must create it.
        let data_dir = tmp.path().join("nested").join("data");
        let mut reg = AssistantSkillRegistry::load(&data_dir);
        reg.register(&["zeta".to_string(), "alpha".to_string()]);
        reg.save().await;

        let path = data_dir.join(AssistantSkillRegistry::FILE_NAME);
        let raw = std::fs::read_to_string(&path).unwrap();
        // Persisted as a sorted JSON array for stable, diff-friendly output.
        assert_eq!(raw, r#"["alpha","zeta"]"#);
    }
}

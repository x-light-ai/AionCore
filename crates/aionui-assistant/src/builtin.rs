//! Built-in assistant registry — loads assets from disk at runtime.
//!
//! Default path: `<exe-dir>/assets/builtin-assistants`
//! Override: `AIONUI_BUILTIN_ASSISTANTS_PATH` env var

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;
use tracing::{error, warn};

fn default_assets_dir() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("assets/builtin-assistants")))
        .unwrap_or_else(|| PathBuf::from("assets/builtin-assistants"))
}

/// Single built-in assistant entry, loaded from `assistants.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct BuiltinAssistant {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub name_i18n: HashMap<String, String>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub description_i18n: HashMap<String, String>,
    #[serde(default)]
    pub avatar: Option<String>,
    pub preset_agent_type: String,
    #[serde(default)]
    pub enabled_skills: Vec<String>,
    #[serde(default)]
    pub custom_skill_names: Vec<String>,
    #[serde(default)]
    pub disabled_builtin_skills: Vec<String>,
    /// Relative to the asset root; may contain `{locale}`.
    #[serde(default)]
    pub rule_file: Option<String>,
    #[serde(default)]
    pub prompts: Vec<String>,
    #[serde(default)]
    pub prompts_i18n: HashMap<String, Vec<String>>,
    #[serde(default)]
    pub models: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct BuiltinManifest {
    #[serde(default)]
    #[allow(dead_code)]
    version: String,
    #[serde(default)]
    assistants: Vec<BuiltinAssistant>,
}

/// An avatar asset loaded from disk.
///
/// Carries the raw bytes plus the file extension (lower-case, without the
/// leading dot) so the HTTP layer can set `Content-Type`.
#[derive(Debug, Clone)]
pub struct AvatarAsset {
    pub bytes: Vec<u8>,
    pub extension: Option<String>,
}

/// In-memory registry of built-in assistants.
pub struct BuiltinAssistantRegistry {
    assistants: HashMap<String, BuiltinAssistant>,
    assets_dir: PathBuf,
}

impl BuiltinAssistantRegistry {
    /// Construct the registry.
    ///
    /// If `AIONUI_BUILTIN_ASSISTANTS_PATH` is set and points to a readable
    /// directory, read from that path. Otherwise use the default path
    /// relative to the executable.
    pub fn load() -> Self {
        if let Ok(env) = std::env::var("AIONUI_BUILTIN_ASSISTANTS_PATH") {
            let p = PathBuf::from(env);
            if p.exists() {
                return Self::load_from_dir(p);
            }
            warn!(
                "AIONUI_BUILTIN_ASSISTANTS_PATH points to missing directory; \
                 falling back to default path"
            );
        }
        Self::load_from_dir(default_assets_dir())
    }

    /// Load from an explicit on-disk directory.
    pub fn load_from_dir(assets_dir: PathBuf) -> Self {
        let manifest_path = assets_dir.join("assistants.json");
        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(c) => c,
            Err(e) => {
                warn!("Built-in manifest missing at {}: {}", manifest_path.display(), e);
                return Self { assistants: HashMap::new(), assets_dir };
            }
        };
        let assistants = parse_manifest_str(&content);
        Self { assistants, assets_dir }
    }

    /// Construct an empty registry (safe fallback + test helper).
    pub fn empty() -> Self {
        Self { assistants: HashMap::new(), assets_dir: PathBuf::new() }
    }

    pub fn has(&self, id: &str) -> bool {
        self.assistants.contains_key(id)
    }

    pub fn get(&self, id: &str) -> Option<&BuiltinAssistant> {
        self.assistants.get(id)
    }

    pub fn all(&self) -> impl Iterator<Item = &BuiltinAssistant> {
        self.assistants.values()
    }

    pub fn is_empty(&self) -> bool {
        self.assistants.is_empty()
    }

    pub fn len(&self) -> usize {
        self.assistants.len()
    }

    /// Read the rule file bytes for a built-in assistant. Substitutes
    /// `{locale}` in the manifest-declared `rule_file` path.
    pub fn rule_bytes(&self, id: &str, locale: &str) -> Option<Vec<u8>> {
        let rel = self.assistants.get(id)?.rule_file.as_ref()?;
        self.read_asset(&rel.replace("{locale}", locale))
    }

    /// Read the avatar asset for a built-in assistant along with its
    /// extension (for Content-Type inference). Returns `None` when the
    /// manifest does not declare an avatar or the file is missing.
    ///
    /// Note: when the manifest `avatar` field is an emoji string
    /// (like `"📝"`) rather than a relative path, no file is resolved and
    /// this method returns `None`.
    pub fn avatar_asset(&self, id: &str) -> Option<AvatarAsset> {
        let a = self.assistants.get(id)?;
        let rel = a.avatar.as_ref()?;
        if !looks_like_relative_path(rel) {
            return None;
        }
        let bytes = self.read_asset(rel)?;
        let extension = Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .map(|s| s.to_ascii_lowercase());
        Some(AvatarAsset { bytes, extension })
    }

    fn read_asset(&self, rel: &str) -> Option<Vec<u8>> {
        std::fs::read(self.assets_dir.join(rel)).ok()
    }
}

impl Default for BuiltinAssistantRegistry {
    fn default() -> Self {
        Self::empty()
    }
}

fn parse_manifest_str(content: &str) -> HashMap<String, BuiltinAssistant> {
    match serde_json::from_str::<Value>(content).and_then(parse_manifest_value) {
        Ok(m) => m.assistants.into_iter().map(|a| (a.id.clone(), a)).collect(),
        Err(e) => {
            error!("Built-in manifest parse failed: {e}");
            HashMap::new()
        }
    }
}

fn parse_manifest_value(value: Value) -> Result<BuiltinManifest, serde_json::Error> {
    if let Some(assistants) = value.get("assistants").and_then(Value::as_array) {
        for assistant in assistants {
            if assistant.get("skill_file").is_some() {
                return Err(serde_json::Error::io(std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "builtin assistant legacy field `skill_file` is no longer supported",
                )));
            }
        }
    }
    serde_json::from_value(value)
}

/// Heuristic for distinguishing a relative-path avatar (`"rules/x.svg"`)
/// from an inline emoji/text avatar (`"📝"`).
fn looks_like_relative_path(s: &str) -> bool {
    s.contains('/') || (Path::new(s).extension().is_some() && !s.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn write_manifest(dir: &Path, body: &str) {
        std::fs::write(dir.join("assistants.json"), body).unwrap();
    }

    #[test]
    fn load_from_dir_missing_dir_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let missing = tmp.path().join("nope");
        let reg = BuiltinAssistantRegistry::load_from_dir(missing);
        assert!(reg.is_empty());
    }

    #[test]
    fn load_from_dir_missing_manifest_returns_empty() {
        let tmp = TempDir::new().unwrap();
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.is_empty());
    }

    #[test]
    fn load_from_dir_malformed_manifest_returns_empty() {
        let tmp = TempDir::new().unwrap();
        write_manifest(tmp.path(), "{not valid json");
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.is_empty());
    }

    #[test]
    fn load_from_dir_rejects_legacy_skill_file_entries() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{
              "version": "1.0.0",
              "assistants": [{
                "id": "legacy",
                "name": "Legacy",
                "preset_agent_type": "gemini",
                "skill_file": "skills/legacy.en-US.md"
              }]
            }"#,
        );
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.is_empty(), "legacy skill_file manifest should be rejected");
    }

    #[test]
    fn load_from_dir_reads_bytes_from_disk() {
        let tmp = TempDir::new().unwrap();
        let rules_dir = tmp.path().join("rules");
        std::fs::create_dir_all(&rules_dir).unwrap();
        std::fs::write(rules_dir.join("office.en-US.md"), "office rule body").unwrap();
        write_manifest(
            tmp.path(),
            r#"{
                "version": "1.0.0",
                "assistants": [{
                    "id": "builtin-office",
                    "name": "Office",
                    "preset_agent_type": "gemini",
                    "rule_file": "rules/office.{locale}.md"
                }]
            }"#,
        );

        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert_eq!(reg.len(), 1);
        assert!(reg.has("builtin-office"));

        let bytes = reg
            .rule_bytes("builtin-office", "en-US")
            .expect("disk-source rule_bytes should read the fixture");
        assert_eq!(bytes, b"office rule body");
    }

    #[test]
    fn load_from_dir_missing_asset_returns_none() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{
                "assistants": [{
                    "id": "x",
                    "name": "X",
                    "preset_agent_type": "gemini",
                    "rule_file": "rules/x.{locale}.md"
                }]
            }"#,
        );
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.rule_bytes("x", "en-US").is_none());
    }

    #[test]
    fn load_respects_env_var_disk_override() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{"assistants":[{"id":"env-only","name":"E","preset_agent_type":"gemini"}]}"#,
        );
        let key = "AIONUI_BUILTIN_ASSISTANTS_PATH";
        let prev = std::env::var(key).ok();
        unsafe { std::env::set_var(key, tmp.path()) }
        let reg = BuiltinAssistantRegistry::load();
        assert!(reg.has("env-only"));
        match prev {
            Some(v) => unsafe { std::env::set_var(key, v) },
            None => unsafe { std::env::remove_var(key) },
        }
    }

    #[test]
    fn avatar_asset_is_none_for_inline_emoji_avatar() {
        let tmp = TempDir::new().unwrap();
        write_manifest(
            tmp.path(),
            r#"{"assistants":[{"id":"emoji-av","name":"E","preset_agent_type":"gemini","avatar":"📝"}]}"#,
        );
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        assert!(reg.avatar_asset("emoji-av").is_none());
    }

    #[test]
    fn avatar_asset_returns_bytes_and_extension_for_file_avatar() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("duck.svg"), b"<svg/>").unwrap();
        write_manifest(
            tmp.path(),
            r#"{"assistants":[{
                "id": "with-file-avatar",
                "name": "F",
                "preset_agent_type": "gemini",
                "avatar": "duck.svg"
            }]}"#,
        );
        let reg = BuiltinAssistantRegistry::load_from_dir(tmp.path().to_path_buf());
        let asset = reg.avatar_asset("with-file-avatar").unwrap();
        assert_eq!(asset.bytes, b"<svg/>");
        assert_eq!(asset.extension.as_deref(), Some("svg"));
    }
}

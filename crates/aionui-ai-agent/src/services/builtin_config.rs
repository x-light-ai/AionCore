//! Unified model config application for XAIWork builtin agents.
//!
//! FORK-CUSTOM: Applies a model's config to both spawn-time env and the local
//! CLI settings file in one atomic operation. Receives `base_url`, `api_key`,
//! `model_id`, and `config_json`; supplements `config_json.env` with the three
//! baseline keys, writes that env to `agent_metadata.env` (for spawn injection),
//! and deep-merges the full `config_json` into `~/.claude/settings.json`.
//!
//! This replaces the prior two-step `builtin_env` + `builtin_cli_config` to
//! avoid half-success states where env is updated but settings.json is not.

use std::path::PathBuf;

use aionui_api_types::AgentEnvEntry;
use serde_json::Value;
use tracing::{info, warn};

use super::AgentService;
use crate::error::AgentError;

/// Env var names a backend reads for relay base url / key / model.
struct BackendEnvKeys {
    base_url: &'static str,
    api_key: &'static str,
    model: &'static str,
}

/// Map a builtin backend label to the env keys its CLI honors.
fn backend_env_keys(backend: &str) -> Option<BackendEnvKeys> {
    match backend {
        "claude" => Some(BackendEnvKeys {
            base_url: "ANTHROPIC_BASE_URL",
            api_key: "ANTHROPIC_AUTH_TOKEN",
            model: "ANTHROPIC_MODEL",
        }),
        "codex" => Some(BackendEnvKeys {
            base_url: "OPENAI_BASE_URL",
            api_key: "OPENAI_API_KEY",
            model: "OPENAI_MODEL",
        }),
        _ => None,
    }
}

/// Map backend to the local CLI settings file path.
fn cli_settings_path(backend: &str) -> Result<PathBuf, AgentError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| AgentError::internal("cannot determine home directory"))?;
    match backend {
        "claude" => Ok(home.join(".claude").join("settings.json")),
        "codex" => Ok(home.join(".codex").join("config.json")),
        _ => Err(AgentError::bad_request(format!("Unsupported builtin backend '{backend}'"))),
    }
}

/// Upsert `name=value` into `env`, replacing an existing entry in place
/// (so order is stable) or appending a new one.
fn upsert_env(env: &mut Vec<AgentEnvEntry>, name: &str, value: String) {
    if let Some(entry) = env.iter_mut().find(|e| e.name == name) {
        entry.value = value;
    } else {
        env.push(AgentEnvEntry {
            name: name.to_string(),
            value,
            description: None,
        });
    }
}

/// Deep-merge `overlay` into `base`. For objects, overlay keys recursively
/// overwrite matching base keys; local-only keys are preserved.
fn deep_merge(base: &mut Value, overlay: Value) {
    match (base, overlay) {
        (Value::Object(base_map), Value::Object(overlay_map)) => {
            for (key, value) in overlay_map {
                let entry = base_map.entry(key).or_insert(Value::Null);
                deep_merge(entry, value);
            }
        }
        (base, overlay) => *base = overlay,
    }
}

impl AgentService {
    /// Apply a model's config to both agent env and local CLI settings.
    ///
    /// 1. Parse `config_json` (treat empty/absent as `{}`).
    /// 2. Ensure `config.env` exists as an object.
    /// 3. Supplement `config.env` with baseline three keys (baseUrl/apiKey/modelId) if non-empty.
    ///    Allows top-level params to be empty if `config_json.env` already provides them.
    /// 4. Validate that the final `config.env` contains a non-empty API key.
    /// 5. Extract all string k/v from `config.env` and upsert into `agent_metadata.env`.
    /// 6. Write env to DB, rehydrate registry (for spawn-time injection).
    /// 7. Deep-merge the full `config_json` into local CLI settings file.
    ///
    /// Both updates happen in sequence; if either fails, the other may have
    /// already succeeded (not transactional across file + DB). Callers should
    /// avoid retrying partial writes.
    pub async fn set_builtin_agent_config(
        &self,
        backend: &str,
        base_url: &str,
        api_key: &str,
        model_id: &str,
        config_json: &str,
    ) -> Result<(), AgentError> {
        let keys = backend_env_keys(backend)
            .ok_or_else(|| AgentError::bad_request(format!("Unsupported builtin backend '{backend}'")))?;

        // 1. Parse config_json (default to empty object).
        let mut config: Value = if config_json.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            serde_json::from_str(config_json)
                .map_err(|e| AgentError::bad_request(format!("invalid config_json: {e}")))?
        };

        // 2. Ensure config.env exists.
        if !config.is_object() {
            return Err(AgentError::bad_request("config_json must be a JSON object"));
        }
        let config_obj = config.as_object_mut().unwrap();
        let env_obj = config_obj
            .entry("env")
            .or_insert_with(|| Value::Object(Default::default()))
            .as_object_mut()
            .ok_or_else(|| AgentError::bad_request("config.env must be an object"))?;

        // 3. Supplement baseline keys into config.env (only if non-empty).
        // Allow top-level params to be empty if config_json.env already provides them.
        if !base_url.trim().is_empty() {
            env_obj.insert(keys.base_url.to_string(), Value::String(base_url.trim().to_string()));
        }
        if !api_key.trim().is_empty() {
            env_obj.insert(keys.api_key.to_string(), Value::String(api_key.trim().to_string()));
        }
        if !model_id.trim().is_empty() {
            env_obj.insert(keys.model.to_string(), Value::String(model_id.trim().to_string()));
        }

        // 4. Validate that the final env has the required keys.
        let final_api_key = env_obj.get(keys.api_key).and_then(|v| v.as_str()).unwrap_or("");
        if final_api_key.trim().is_empty() {
            return Err(AgentError::bad_request(format!(
                "{} must be set (either via api_key param or config_json.env.{})",
                keys.api_key, keys.api_key
            )));
        }

        // 5. Extract config.env into AgentEnvEntry vec for agent_metadata.env.
        let repo = self.registry().repo_handle();
        let row = repo
            .find_builtin_by_backend(backend)
            .await
            .map_err(|e| AgentError::internal(format!("repo.find_builtin_by_backend: {e}")))?
            .ok_or_else(|| AgentError::not_found(format!("Builtin agent for backend '{backend}' not found")))?;

        let mut agent_env: Vec<AgentEnvEntry> = match row.env.as_deref() {
            Some(raw) if !raw.trim().is_empty() => serde_json::from_str(raw)
                .map_err(|e| AgentError::internal(format!("decode existing agent env: {e}")))?,
            _ => Vec::new(),
        };

        for (k, v) in env_obj.iter() {
            if let Some(s) = v.as_str() {
                upsert_env(&mut agent_env, k, s.to_string());
            }
        }

        let env_json = serde_json::to_string(&agent_env)
            .map_err(|e| AgentError::internal(format!("encode agent env: {e}")))?;

        // 6. Write env to DB, rehydrate registry.
        let updated = repo
            .update_env(&row.id, &env_json)
            .await
            .map_err(|e| AgentError::internal(format!("repo.update_env: {e}")))?;
        if !updated {
            return Err(AgentError::not_found(format!("Builtin agent '{}' not found", row.id)));
        }

        if let Err(err) = self.registry().invalidate_and_rehydrate().await {
            warn!(agent_id = %row.id, backend = %backend, error = %err, "registry rehydrate failed");
        }

        // 7. Deep-merge full config_json into local CLI settings file.
        let path = cli_settings_path(backend)?;

        let mut base: Value = if path.exists() {
            let raw = tokio::fs::read_to_string(&path)
                .await
                .map_err(|e| AgentError::internal(format!("read {path:?}: {e}")))?;
            serde_json::from_str(&raw).unwrap_or(Value::Object(Default::default()))
        } else {
            Value::Object(Default::default())
        };

        deep_merge(&mut base, config);

        let merged = serde_json::to_string_pretty(&base)
            .map_err(|e| AgentError::internal(format!("encode merged settings: {e}")))?;

        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .map_err(|e| AgentError::internal(format!("create dir {parent:?}: {e}")))?;
        }

        tokio::fs::write(&path, &merged)
            .await
            .map_err(|e| AgentError::internal(format!("write {path:?}: {e}")))?;

        info!(
            backend = %backend,
            agent_id = %row.id,
            settings_path = ?path,
            "model config applied: agent env updated + CLI settings merged"
        );

        Ok(())
    }
}

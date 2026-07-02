//! Unified model config application for XAIWork builtin agents.
//!
//! FORK-CUSTOM: Applies a model's config to both spawn-time env and the local
//! CLI settings file in one atomic operation. Receives `base_url`, `api_key`,
//! `model_id`, and `config_json`; supplements `config_json.env` with the three
//! baseline keys, writes that env to `agent_metadata.env` (for spawn injection),
//! and deep-merges the full `config_json` into `~/.claude/settings.json`.
//!
//! 此文件为 XAIWork fork 新增文件，不存在于上游仓库，rebase 时无冲突风险。

use std::path::PathBuf;

use aionui_api_types::AgentEnvEntry;
use serde_json::Value;
use tracing::info;

use super::AgentService;
use crate::error::AgentError;

// ── 数据类型 ─────────────────────────────────────────────────────────────────

/// Env var names a backend reads for relay base url / key / model.
struct BackendEnvKeys {
    base_url: &'static str,
    api_key: &'static str,
    model: &'static str,
}

// ── 纯函数 helpers ────────────────────────────────────────────────────────────

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

// ── 步骤函数 ──────────────────────────────────────────────────────────────────

/// Step 1: 解析 config_json 字符串为 JSON object。空字符串视为 `{}`。
fn parse_config_json(config_json: &str) -> Result<Value, AgentError> {
    let config: Value = if config_json.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(config_json)
            .map_err(|e| AgentError::bad_request(format!("invalid config_json: {e}")))?
    };
    if !config.is_object() {
        return Err(AgentError::bad_request("config_json must be a JSON object"));
    }
    Ok(config)
}

/// Step 2: 将 base_url / api_key / model_id 注入 config.env，使用 backend 对应的 key 名。
///
/// 非空参数才写入（允许调用方传空串以便 config_json.env 里的已有值保留）。
/// 注入完成后校验 api_key 对应的 env key 非空。
fn inject_backend_env_keys(
    config: &mut Value,
    backend: &str,
    base_url: &str,
    api_key: &str,
    model_id: &str,
) -> Result<(), AgentError> {
    let keys = backend_env_keys(backend)
        .ok_or_else(|| AgentError::bad_request(format!("Unsupported builtin backend '{backend}'")))?;

    let env_obj = config
        .as_object_mut()
        .unwrap() // parse_config_json 已保证是 object
        .entry("env")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| AgentError::bad_request("config.env must be an object"))?;

    if !base_url.trim().is_empty() {
        env_obj.insert(keys.base_url.to_string(), Value::String(base_url.trim().to_string()));
    }
    if !api_key.trim().is_empty() {
        env_obj.insert(keys.api_key.to_string(), Value::String(api_key.trim().to_string()));
    }
    if !model_id.trim().is_empty() {
        env_obj.insert(keys.model.to_string(), Value::String(model_id.trim().to_string()));
    }

    // 校验最终 env 里 api_key 对应 key 非空
    let final_api_key = env_obj.get(keys.api_key).and_then(|v| v.as_str()).unwrap_or("");
    if final_api_key.trim().is_empty() {
        return Err(AgentError::bad_request(format!(
            "{} must be set (either via api_key param or config_json.env.{})",
            keys.api_key, keys.api_key
        )));
    }

    Ok(())
}

/// Step 3: 从 config.env 中提取所有 string 类型的 k/v，非 string 值返回 bad_request 错误。
fn extract_string_env_entries(config: &Value) -> Result<Vec<(String, String)>, AgentError> {
    let env_obj = config
        .get("env")
        .and_then(|v| v.as_object())
        .ok_or_else(|| AgentError::bad_request("config.env must be an object"))?;

    env_obj
        .iter()
        .map(|(k, v)| {
            v.as_str()
                .ok_or_else(|| {
                    AgentError::bad_request(format!(
                        "config.env.{k} must be a string, got {}",
                        v
                    ))
                })
                .map(|s| (k.clone(), s.to_string()))
        })
        .collect()
}

/// Step 4: 将 env 条目 upsert 到 SQLite agent_metadata.env，并 rehydrate registry。
///
/// rehydrate 失败时返回 Err，避免 DB 已写但 registry 未刷的撕裂状态。
async fn write_agent_metadata_env(
    service: &AgentService,
    backend: &str,
    env_entries: Vec<(String, String)>,
) -> Result<String, AgentError> {
    let repo = service.registry().repo_handle();
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

    for (k, v) in env_entries {
        upsert_env(&mut agent_env, &k, v);
    }

    let env_json = serde_json::to_string(&agent_env)
        .map_err(|e| AgentError::internal(format!("encode agent env: {e}")))?;

    let updated = repo
        .update_env(&row.id, &env_json)
        .await
        .map_err(|e| AgentError::internal(format!("repo.update_env: {e}")))?;
    if !updated {
        return Err(AgentError::not_found(format!("Builtin agent '{}' not found", row.id)));
    }

    service
        .registry()
        .invalidate_and_rehydrate()
        .await
        .map_err(|e| AgentError::internal(format!("registry rehydrate failed: {e}")))?;

    Ok(row.id)
}

/// Step 5: 将完整 config 原子性 deep-merge 到本地 CLI settings 文件（先写 .tmp，再 rename）。
///
/// env 段的所有值在写入前替换为 `"*"`，避免将真实凭证（api key / base_url 等）
/// 落盘到明文 settings.json。实际 env 值由 SQLite agent_metadata.env → 进程 env 通道注入。
async fn merge_cli_settings(backend: &str, mut config: Value) -> Result<(), AgentError> {
    // 用 * 遮盖 env 段所有值，仅保留 key 名作为占位（告知 CLI 该键存在但值由进程 env 提供）
    if let Some(env_obj) = config.get_mut("env").and_then(|v| v.as_object_mut()) {
        for val in env_obj.values_mut() {
            *val = Value::String("*".to_string());
        }
    }

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

    // 原子写：先写 .tmp，再 rename，避免进程崩溃写出半截文件
    let tmp = path.with_extension("tmp");
    tokio::fs::write(&tmp, &merged)
        .await
        .map_err(|e| AgentError::internal(format!("write {tmp:?}: {e}")))?;
    tokio::fs::rename(&tmp, &path)
        .await
        .map_err(|e| AgentError::internal(format!("rename {tmp:?} -> {path:?}: {e}")))?;

    Ok(())
}

// ── 公开 API ──────────────────────────────────────────────────────────────────

impl AgentService {
    /// Apply a model's config to both agent env and local CLI settings.
    ///
    /// 编排流程：
    /// 1. `parse_config_json`       — 解析/校验 config_json
    /// 2. `inject_backend_env_keys` — 注入 backend-specific 三键（base_url/api_key/model_id）
    /// 3. `extract_string_env_entries` — 提取 env 段，非 string 值报错
    /// 4. `write_agent_metadata_env` — 写 SQLite agent_metadata.env + rehydrate registry
    /// 5. `merge_cli_settings`      — 原子写 ~/.claude/settings.json（tmp → rename）
    ///
    /// DB 和文件写入顺序：DB 先写；若 settings.json 写失败，DB 已写不回滚。
    /// 调用方应避免在写失败后重试（可能导致 env 重复 upsert）。
    pub async fn set_builtin_agent_config(
        &self,
        backend: &str,
        base_url: &str,
        api_key: &str,
        model_id: &str,
        config_json: &str,
    ) -> Result<(), AgentError> {
        let mut config = parse_config_json(config_json)?;
        inject_backend_env_keys(&mut config, backend, base_url, api_key, model_id)?;
        let env_entries = extract_string_env_entries(&config)?;

        let agent_id = write_agent_metadata_env(self, backend, env_entries).await?;
        merge_cli_settings(backend, config).await?;

        let settings_path = cli_settings_path(backend).ok();
        info!(
            backend = %backend,
            agent_id = %agent_id,
            settings_path = ?settings_path,
            "model config applied: agent env updated + CLI settings merged"
        );

        Ok(())
    }
}

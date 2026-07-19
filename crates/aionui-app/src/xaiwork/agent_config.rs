// FORK-CUSTOM: XAIWork builtin Agent model configuration application.
//! Unified model config application for XAIWork builtin agents.
//!
//! FORK-CUSTOM: Applies a model's config to spawn-time env and, for Claude,
//! the local CLI settings file. Receives `base_url`, `api_key`,
//! `model_id`, and `config_json`; supplements `config_json.env` with the three
//! baseline keys, writes that env to `agent_metadata.env_override` (for spawn injection),
//! and deep-merges the full `config_json` into the local CLI settings file.
//! Codex ACP receives its provider override through `MODEL_PROVIDER` and
//! `CODEX_CONFIG`, while the selected model key is exposed only as the
//! provider's `OPENAI_API_KEY` process environment variable. It does not
//! consume a JSON settings file or modify the user's `auth.json`.
//!
//! 此文件为 XAIWork fork 新增文件，不存在于上游仓库，rebase 时无冲突风险。

use std::path::PathBuf;

use aionui_api_types::AgentEnvEntry;
use serde_json::Value;
use tracing::info;

use aionui_ai_agent::{AgentError, AgentRegistry};

// ── 数据类型 ─────────────────────────────────────────────────────────────────

const CLAUDE_BASE_URL_ENV: &str = "ANTHROPIC_BASE_URL";
const CLAUDE_API_KEY_ENV: &str = "ANTHROPIC_AUTH_TOKEN";
const CLAUDE_MODEL_ENV: &str = "ANTHROPIC_MODEL";
const CODEX_API_KEY_ENV: &str = "CODEX_API_KEY";
const OPENAI_API_KEY_ENV: &str = "OPENAI_API_KEY";
const CODEX_XAIWORK_PROVIDER: &str = "xaiwork";

// ── 纯函数 helpers ────────────────────────────────────────────────────────────

/// Return the local Claude settings file path.
fn claude_settings_path() -> Result<PathBuf, AgentError> {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .map(PathBuf::from)
        .map_err(|_| AgentError::internal("cannot determine home directory"))?;
    Ok(home.join(".claude").join("settings.json"))
}

/// Upsert `name=value` into `env`, replacing an existing entry in place
/// (so order is stable) or appending a new one.
fn upsert_env(env: &mut Vec<AgentEnvEntry>, name: &str, value: String) {
    if let Some(entry) = env.iter_mut().find(|e| e.name.eq_ignore_ascii_case(name)) {
        entry.name = name.to_string();
        entry.value = value;
    } else {
        env.push(AgentEnvEntry {
            name: name.to_string(),
            value,
            description: None,
        });
    }

    let mut found = false;
    env.retain(|entry| {
        if !entry.name.eq_ignore_ascii_case(name) {
            return true;
        }
        if found {
            return false;
        }
        found = true;
        true
    });
}

fn remove_json_env_key(env: &mut serde_json::Map<String, Value>, name: &str) {
    env.retain(|key, _| !key.eq_ignore_ascii_case(name));
}

fn upsert_json_env(env: &mut serde_json::Map<String, Value>, name: &str, value: String) {
    remove_json_env_key(env, name);
    env.insert(name.to_owned(), Value::String(value));
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

/// Mask only the Claude CLI settings keys that must be placeholders on disk.
fn mask_claude_cli_settings_env(config: &mut Value) {
    const CLAUDE_MASKED_ENV_KEYS: &[&str] = &[CLAUDE_API_KEY_ENV, CLAUDE_BASE_URL_ENV];

    if let Some(env_obj) = config.get_mut("env").and_then(|v| v.as_object_mut()) {
        for (name, val) in env_obj {
            if CLAUDE_MASKED_ENV_KEYS.iter().any(|key| name.eq_ignore_ascii_case(key)) {
                *val = Value::String("*".to_string());
            }
        }
    }
}

// ── 步骤函数 ──────────────────────────────────────────────────────────────────

/// Step 1: 解析 config_json 字符串为 JSON object。空字符串视为 `{}`。
fn parse_config_json(config_json: &str) -> Result<Value, AgentError> {
    let config: Value = if config_json.trim().is_empty() {
        Value::Object(Default::default())
    } else {
        serde_json::from_str(config_json).map_err(|e| AgentError::bad_request(format!("invalid config_json: {e}")))?
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
    if backend == "codex" {
        let base_url = base_url.trim();
        let api_key = api_key.trim();
        let model_id = model_id.trim();
        if base_url.is_empty() {
            return Err(AgentError::bad_request(
                "Codex base_url must be supplied by XAIWork OpenApi",
            ));
        }
        if api_key.is_empty() {
            return Err(AgentError::bad_request(
                "Codex api_key must be supplied by XAIWork OpenApi",
            ));
        }
        if model_id.is_empty() {
            return Err(AgentError::bad_request(
                "Codex model_id must be supplied by XAIWork OpenApi",
            ));
        }

        let codex_config = build_codex_runtime_config(config, base_url, model_id)?;
        let env_obj = config
            .as_object_mut()
            .expect("parse_config_json guarantees an object")
            .entry("env")
            .or_insert_with(|| Value::Object(Default::default()))
            .as_object_mut()
            .ok_or_else(|| AgentError::bad_request("config.env must be an object"))?;
        remove_json_env_key(env_obj, CODEX_API_KEY_ENV);
        upsert_json_env(env_obj, OPENAI_API_KEY_ENV, api_key.to_owned());
        upsert_json_env(env_obj, "MODEL_PROVIDER", CODEX_XAIWORK_PROVIDER.to_owned());
        upsert_json_env(env_obj, "CODEX_CONFIG", codex_config);
        return Ok(());
    }

    if backend != "claude" {
        return Err(AgentError::bad_request(format!(
            "Unsupported builtin backend '{backend}'"
        )));
    }

    // FORK-CUSTOM: Claude and Codex use the same credential source. The
    // selected model's key must come from XAIWork OpenApi; a stale token in
    // config_json.env must never become an implicit fallback.
    let api_key = api_key.trim();
    if api_key.is_empty() {
        return Err(AgentError::bad_request(format!(
            "{CLAUDE_API_KEY_ENV} must be supplied by XAIWork OpenApi"
        )));
    }

    let env_obj = config
        .as_object_mut()
        .unwrap()
        .entry("env")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| AgentError::bad_request("config.env must be an object"))?;

    if !base_url.trim().is_empty() {
        upsert_json_env(env_obj, CLAUDE_BASE_URL_ENV, base_url.trim().to_owned());
    }
    upsert_json_env(env_obj, CLAUDE_API_KEY_ENV, api_key.to_owned());
    if !model_id.trim().is_empty() {
        upsert_json_env(env_obj, CLAUDE_MODEL_ENV, model_id.trim().to_owned());
    }

    Ok(())
}

/// Build the configuration consumed by `@agentclientprotocol/codex-acp`.
///
/// Codex ACP reads `MODEL_PROVIDER` and merges `CODEX_CONFIG` into the Codex
/// session config. The custom provider reads `OPENAI_API_KEY` directly from
/// the child process environment, bypassing account login and `auth.json`.
fn build_codex_runtime_config(config: &Value, base_url: &str, model_id: &str) -> Result<String, AgentError> {
    let mut runtime_config = config.clone();
    let object = runtime_config
        .as_object_mut()
        .ok_or_else(|| AgentError::bad_request("Codex config must be a JSON object"))?;
    object.remove("env");

    object.insert(
        "model_provider".to_owned(),
        Value::String(CODEX_XAIWORK_PROVIDER.to_owned()),
    );
    object.insert("model".to_owned(), Value::String(model_id.to_owned()));

    let providers = object
        .entry("model_providers".to_owned())
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| AgentError::bad_request("Codex model_providers must be an object"))?;
    let provider = providers
        .entry(CODEX_XAIWORK_PROVIDER.to_owned())
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| AgentError::bad_request("Codex XAIWork provider must be an object"))?;
    provider.insert("name".to_owned(), Value::String("XAIWork".to_owned()));
    provider.insert("base_url".to_owned(), Value::String(base_url.to_owned()));
    provider
        .entry("wire_api".to_owned())
        .or_insert_with(|| Value::String("responses".to_owned()));
    provider.insert("env_key".to_owned(), Value::String(OPENAI_API_KEY_ENV.to_owned()));
    provider.insert("requires_openai_auth".to_owned(), Value::Bool(false));

    serde_json::to_string(&runtime_config)
        .map_err(|e| AgentError::internal(format!("encode Codex runtime config: {e}")))
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
                .ok_or_else(|| AgentError::bad_request(format!("config.env.{k} must be a string, got {}", v)))
                .map(|s| (k.clone(), s.to_string()))
        })
        .collect()
}

/// Step 4: 将 env 条目 upsert 到 SQLite agent_metadata.env，并刷新对应 registry row。
///
/// reload 失败时返回 Err，避免 DB 已写但 registry 未刷的撕裂状态。
async fn write_agent_metadata_env(
    registry: &AgentRegistry,
    backend: &str,
    env_entries: Vec<(String, String)>,
) -> Result<String, AgentError> {
    let repo = registry.repo_handle();
    let row = repo
        .find_builtin_by_backend(backend)
        .await
        .map_err(|e| AgentError::internal(format!("repo.find_builtin_by_backend: {e}")))?
        .ok_or_else(|| AgentError::not_found(format!("Builtin agent for backend '{backend}' not found")))?;

    let mut agent_env: Vec<AgentEnvEntry> = match row.env_override.as_deref() {
        Some(raw) if !raw.trim().is_empty() => {
            serde_json::from_str(raw).map_err(|e| AgentError::internal(format!("decode existing agent env: {e}")))?
        }
        _ => Vec::new(),
    };

    if backend == "codex" {
        agent_env.retain(|entry| !entry.name.eq_ignore_ascii_case(CODEX_API_KEY_ENV));
    }

    for (k, v) in env_entries {
        upsert_env(&mut agent_env, &k, v);
    }

    let env_json =
        serde_json::to_string(&agent_env).map_err(|e| AgentError::internal(format!("encode agent env: {e}")))?;

    repo.update_agent_overrides(&row.id, row.command_override.as_deref(), Some(&env_json))
        .await
        .map_err(|e| AgentError::internal(format!("repo.update_agent_overrides: {e}")))?;

    let reloaded = registry
        .reload_one(&row.id)
        .await
        .map_err(|e| AgentError::internal(format!("registry reload failed: {e}")))?;
    if reloaded.is_none() {
        return Err(AgentError::internal(format!(
            "registry reload did not find updated agent '{}'",
            row.id
        )));
    }

    Ok(row.id)
}

/// Step 5: 将完整 config 原子性 deep-merge 到本地 CLI settings 文件（先写 .tmp，再 rename）。
///
/// env 段写入 settings.json 前只遮盖 Claude CLI 会读取的敏感认证键；其他 env
/// 键保留原值，避免把非认证配置（例如默认模型、特性开关等）错误写成 `"*"`。
async fn merge_claude_cli_settings(mut config: Value) -> Result<PathBuf, AgentError> {
    mask_claude_cli_settings_env(&mut config);

    let path = claude_settings_path()?;

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

    Ok(path)
}

// ── 公开 API ──────────────────────────────────────────────────────────────────

/// Apply a model's config to both agent env and local CLI settings.
///
/// 编排流程：
/// 1. `parse_config_json`       — 解析/校验 config_json
/// 2. `inject_backend_env_keys` — 注入 backend-specific 三键（base_url/api_key/model_id）
/// 3. `extract_string_env_entries` — 提取 env 段，非 string 值报错
/// 4. `write_agent_metadata_env` — 写 SQLite agent_metadata.env + rehydrate registry
/// 5. `merge_claude_cli_settings` — Claude 原子写 settings.json（tmp → rename）
///
/// DB 和文件写入顺序：DB 先写；若 settings.json 写失败，DB 已写不回滚。
/// 调用方应避免在写失败后重试（可能导致 env 重复 upsert）。
pub async fn set_builtin_agent_config(
    registry: &AgentRegistry,
    backend: &str,
    base_url: &str,
    api_key: &str,
    model_id: &str,
    config_json: &str,
) -> Result<(), AgentError> {
    let mut config = parse_config_json(config_json)?;
    inject_backend_env_keys(&mut config, backend, base_url, api_key, model_id)?;
    let env_entries = extract_string_env_entries(&config)?;

    let agent_id = write_agent_metadata_env(registry, backend, env_entries).await?;
    let settings_path = if backend == "claude" {
        Some(merge_claude_cli_settings(config).await?)
    } else {
        None
    };
    info!(
        backend = %backend,
        agent_id = %agent_id,
        settings_path = ?settings_path,
        "model config applied: agent env updated + CLI settings merged"
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use aionui_api_types::AgentEnvEntry;
    use serde_json::{Value, json};

    use super::{inject_backend_env_keys, mask_claude_cli_settings_env, upsert_env};

    #[test]
    fn upsert_env_replaces_case_insensitive_match_and_normalizes_name() {
        let mut env = vec![
            AgentEnvEntry {
                name: "openai_api_key".to_owned(),
                value: "stale-key".to_owned(),
                description: None,
            },
            AgentEnvEntry {
                name: "OpenAI_Api_Key".to_owned(),
                value: "another-stale-key".to_owned(),
                description: None,
            },
        ];

        upsert_env(&mut env, "OPENAI_API_KEY", "selected-key".to_owned());

        assert_eq!(env.len(), 1);
        assert_eq!(env[0].name, "OPENAI_API_KEY");
        assert_eq!(env[0].value, "selected-key");
    }

    #[test]
    fn masks_only_claude_auth_token_and_base_url() {
        let mut config = json!({
            "env": {
                "ANTHROPIC_AUTH_TOKEN": "sk-secret",
                "ANTHROPIC_BASE_URL": "https://relay.example.com",
                "ANTHROPIC_MODEL": "claude-opus-4-8",
                "ANTHROPIC_DEFAULT_SONNET_MODEL": "claude-sonnet-4-6",
                "CUSTOM_FLAG": "enabled"
            }
        });

        mask_claude_cli_settings_env(&mut config);

        assert_eq!(config["env"]["ANTHROPIC_AUTH_TOKEN"], "*");
        assert_eq!(config["env"]["ANTHROPIC_BASE_URL"], "*");
        assert_eq!(config["env"]["ANTHROPIC_MODEL"], "claude-opus-4-8");
        assert_eq!(config["env"]["ANTHROPIC_DEFAULT_SONNET_MODEL"], "claude-sonnet-4-6");
        assert_eq!(config["env"]["CUSTOM_FLAG"], "enabled");
    }

    #[test]
    fn leaves_unrelated_env_values_unchanged() {
        let mut config = json!({
            "env": {
                "OPENAI_API_KEY": "sk-openai",
                "OPENAI_BASE_URL": "https://openai.example.com",
                "OPENAI_MODEL": "gpt-5"
            }
        });

        mask_claude_cli_settings_env(&mut config);

        assert_eq!(config["env"]["OPENAI_API_KEY"], "sk-openai");
        assert_eq!(config["env"]["OPENAI_BASE_URL"], "https://openai.example.com");
        assert_eq!(config["env"]["OPENAI_MODEL"], "gpt-5");
    }

    #[test]
    fn injects_codex_runtime_provider_from_selected_xaiwork_model() {
        let mut config = json!({"env": {"codex_api_key": "stale-local-key"}});

        inject_backend_env_keys(
            &mut config,
            "codex",
            "https://relay.example/v1",
            "sk-xaiwork",
            "gpt-5-codex",
        )
        .expect("Codex runtime config should be generated");

        assert_eq!(config["env"]["OPENAI_API_KEY"], "sk-xaiwork");
        assert!(config["env"].get("CODEX_API_KEY").is_none());
        assert!(config["env"].get("codex_api_key").is_none());
        assert_eq!(config["env"]["MODEL_PROVIDER"], "xaiwork");

        let runtime: Value = serde_json::from_str(config["env"]["CODEX_CONFIG"].as_str().unwrap())
            .expect("CODEX_CONFIG should contain JSON");
        assert_eq!(runtime["model_provider"], "xaiwork");
        assert_eq!(runtime["model"], "gpt-5-codex");
        assert_eq!(runtime["model_providers"]["xaiwork"]["name"], "XAIWork");
        assert_eq!(
            runtime["model_providers"]["xaiwork"]["base_url"],
            "https://relay.example/v1"
        );
        assert_eq!(runtime["model_providers"]["xaiwork"]["wire_api"], "responses");
        assert_eq!(runtime["model_providers"]["xaiwork"]["env_key"], "OPENAI_API_KEY");
        assert_eq!(runtime["model_providers"]["xaiwork"]["requires_openai_auth"], false);
        assert!(runtime.get("env").is_none());
    }

    #[test]
    fn rejects_codex_when_openapi_key_is_missing_instead_of_using_config_env() {
        let mut config = json!({"env": {"OPENAI_API_KEY": "stale-local-key"}});

        let error = inject_backend_env_keys(&mut config, "codex", "https://relay.example/v1", "", "gpt-5-codex")
            .expect_err("Codex must not fall back to a config env key");

        assert!(
            error
                .to_string()
                .contains("api_key must be supplied by XAIWork OpenApi")
        );
    }

    #[test]
    fn rejects_claude_when_openapi_key_is_missing_instead_of_using_config_env() {
        let mut config = json!({"env": {"ANTHROPIC_AUTH_TOKEN": "stale-local-key"}});

        let error = inject_backend_env_keys(&mut config, "claude", "https://relay.example/v1", "", "claude-sonnet")
            .expect_err("Claude must not fall back to a config env key");

        assert!(
            error
                .to_string()
                .contains("ANTHROPIC_AUTH_TOKEN must be supplied by XAIWork OpenApi")
        );
    }

    #[test]
    fn rejects_codex_config_with_non_object_model_providers() {
        let mut config = json!({"model_providers": []});

        let error = inject_backend_env_keys(
            &mut config,
            "codex",
            "https://relay.example/v1",
            "sk-xaiwork",
            "gpt-5-codex",
        )
        .expect_err("invalid model_providers shape should fail");

        assert!(error.to_string().contains("model_providers must be an object"));
    }
}

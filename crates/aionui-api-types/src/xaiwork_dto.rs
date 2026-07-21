// FORK-CUSTOM: DTOs for the XAIWork config broker flow. AionUi calls AionCore
// (`/api/agents/xaiwork/models` and `/api/agents/xaiwork/apply`), AionCore then
// server-to-server calls XAIWork OpenApi `/openapi/agent/config`. `api_key` and
// `config_json` are sensitive and never surface to the renderer.
//
// AionCore owns the trusted XAIWork host configuration. The renderer forwards
// only the user's short-lived XAIWork token for each broker request.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ImportRemoteAssistantsRequest {
    pub url: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ImportRemoteSkillRequest {
    pub url: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WechatLoginMode {
    #[default]
    Sa,
    Miniprogram,
}

#[derive(Debug, Deserialize)]
pub struct XaiworkLoginRequest {
    pub ticket: String,
    #[serde(default)]
    pub mode: WechatLoginMode,
}

#[derive(Debug, Serialize)]
pub struct XaiworkRemoteAuth {
    pub access_token: String,
    pub refresh_token: String,
    pub access_expires_in: i64,
}

#[derive(Debug, Serialize)]
pub struct XaiworkBridgePublicUser {
    pub id: String,
    pub username: String,
}

#[derive(Debug, Serialize)]
pub struct XaiworkLoginResponse {
    pub success: bool,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<XaiworkBridgePublicUser>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_auth: Option<XaiworkRemoteAuth>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_nickname: Option<String>,
}

impl XaiworkLoginResponse {
    pub fn pending() -> Self {
        Self {
            success: true,
            status: "pending",
            message: None,
            token: None,
            user: None,
            remote_auth: None,
            remote_nickname: None,
        }
    }

    pub fn expired() -> Self {
        Self {
            success: true,
            status: "expired",
            message: None,
            token: None,
            user: None,
            remote_auth: None,
            remote_nickname: None,
        }
    }
}

/// Public model info returned to AionUi. Deliberately excludes any credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiworkPublicModel {
    pub model_id: String,
    pub name: String,
    pub reasoning_efforts: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum XaiworkSkillInstallSource {
    #[default]
    Market,
    AssistantBundle,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum XaiworkSkillVisibility {
    #[default]
    User,
    Dependency,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct XaiworkInstalledSkillMetadata {
    pub name: String,
    pub description: Option<String>,
    pub version: Option<String>,
    pub tags: Vec<String>,
    pub source: XaiworkSkillInstallSource,
    pub visibility: XaiworkSkillVisibility,
    pub assistant_ids: Vec<String>,
}

/// Request from AionUi: list distributed models for a builtin agent backend.
///
/// `xaiwork_auth_token` is the frontend user's XAIWork JWT; AionCore forwards
/// it as `Authorization: Bearer <token>` when calling OpenApi and never stores
/// it. The field name intentionally contains `auth_token` so upstream
/// `httpBridge.ts::SENSITIVE_LOG_KEY_PATTERN` redacts it in dev-mode logs.
///
/// Fields deserialised as-is (`snake_case`) matching the upstream request-body
/// convention used across the AionCore HTTP surface. Custom `Debug` below
/// redacts the token so structured logging (`{req:?}`) can never leak it.
#[derive(Clone, Serialize, Deserialize)]
pub struct ListXaiworkModelsRequest {
    pub backend: String,
    pub xaiwork_auth_token: String,
}

impl std::fmt::Debug for ListXaiworkModelsRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListXaiworkModelsRequest")
            .field("backend", &self.backend)
            .field("xaiwork_auth_token", &"<redacted>")
            .finish()
    }
}

/// Request from AionUi: apply a distributed model to the local builtin agent.
///
/// AionCore fetches full configs via OpenApi, finds the entry matching
/// `model_id`, and calls the existing `set_builtin_agent_config` service.
#[derive(Clone, Serialize, Deserialize)]
pub struct ApplyXaiworkModelRequest {
    pub backend: String,
    pub model_id: String,
    pub xaiwork_auth_token: String,
}

impl std::fmt::Debug for ApplyXaiworkModelRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyXaiworkModelRequest")
            .field("backend", &self.backend)
            .field("model_id", &self.model_id)
            .field("xaiwork_auth_token", &"<redacted>")
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_request_debug_redacts_token() {
        let req = ListXaiworkModelsRequest {
            backend: "claude".into(),
            xaiwork_auth_token: "super-secret-jwt".into(),
        };
        let s = format!("{req:?}");
        assert!(!s.contains("super-secret-jwt"), "token leaked: {s}");
        assert!(s.contains("<redacted>"));
    }

    #[test]
    fn apply_request_debug_redacts_token() {
        let req = ApplyXaiworkModelRequest {
            backend: "claude".into(),
            model_id: "claude-opus-4-7".into(),
            xaiwork_auth_token: "super-secret-jwt".into(),
        };
        let s = format!("{req:?}");
        assert!(!s.contains("super-secret-jwt"), "token leaked: {s}");
        assert!(s.contains("<redacted>"));
    }

    #[test]
    fn public_model_serializes_reasoning_efforts_without_credentials() {
        let value = serde_json::to_value(XaiworkPublicModel {
            model_id: "gpt-5.4".into(),
            name: "GPT-5.4".into(),
            reasoning_efforts: vec!["low".into(), "medium".into(), "high".into()],
        })
        .unwrap();

        assert_eq!(value["modelId"], "gpt-5.4");
        assert_eq!(value["reasoningEfforts"], serde_json::json!(["low", "medium", "high"]));
        assert!(value.get("apiKey").is_none());
        assert!(value.get("configJson").is_none());
    }
}

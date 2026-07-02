// FORK-CUSTOM: DTOs for the XAIWork config broker flow. AionUi calls AionCore
// (`/api/agents/xaiwork/models` and `/api/agents/xaiwork/apply`), AionCore then
// server-to-server calls XAIWork OpenApi `/openapi/agent/config`. `api_key` and
// `config_json` are sensitive and never surface to the renderer.
//
// `xaiwork_host` and `xaiwork_token` are passed in request body so AionCore
// doesn't need to persist XAIWork configuration; per-request forwarding keeps
// the credential surface minimal (no storage, no ambient state).

use serde::{Deserialize, Serialize};

/// Public model info returned to AionUi. Deliberately excludes any credential.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiworkPublicModel {
    pub model_id: String,
    pub name: String,
}

/// Request from AionUi: list distributed models for a builtin agent backend.
///
/// `xaiwork_host` is the XAIWork OpenApi base URL (e.g. `https://xaiwork.example.com`).
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
    pub xaiwork_host: String,
    pub xaiwork_auth_token: String,
}

impl std::fmt::Debug for ListXaiworkModelsRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ListXaiworkModelsRequest")
            .field("backend", &self.backend)
            .field("xaiwork_host", &self.xaiwork_host)
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
    pub xaiwork_host: String,
    pub xaiwork_auth_token: String,
}

impl std::fmt::Debug for ApplyXaiworkModelRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ApplyXaiworkModelRequest")
            .field("backend", &self.backend)
            .field("model_id", &self.model_id)
            .field("xaiwork_host", &self.xaiwork_host)
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
            xaiwork_host: "https://x.example".into(),
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
            xaiwork_host: "https://x.example".into(),
            xaiwork_auth_token: "super-secret-jwt".into(),
        };
        let s = format!("{req:?}");
        assert!(!s.contains("super-secret-jwt"), "token leaked: {s}");
        assert!(s.contains("<redacted>"));
    }
}

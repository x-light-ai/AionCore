// FORK-CUSTOM: Server-to-server config broker. Fetches full agent model
// configurations (including api_key / config_json) from XAIWork OpenApi so
// the AionUi renderer never handles credentials directly. The frontend passes
// its user JWT along with each request; AionCore forwards it as a Bearer
// token and never persists it.

use std::time::Duration;

use serde::Deserialize;

use crate::error::AgentError;

/// XAIWork OpenApi wraps every response in `{ traceId, data, success, ... }`.
#[derive(Debug, Deserialize)]
struct XHubEnvelope<T> {
    data: Option<T>,
    #[serde(default)]
    success: bool,
    #[serde(default)]
    message: Option<String>,
}

/// One entry returned by `POST /openapi/agent/config`. Mirrors XAIWork's
/// `AgentConfigOutDto`. `api_key` / `config_json` are sensitive — see the
/// `Debug` impl below for the redaction contract.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiworkModelConfig {
    pub model_id: String,
    pub name: String,
    pub base_url: String,
    pub api_key: String,
    #[serde(default)]
    pub config_json: Option<String>,
}

impl std::fmt::Debug for XaiworkModelConfig {
    /// Redact `api_key` and `config_json`. `config_json` may embed env keys
    /// (e.g. `ANTHROPIC_AUTH_TOKEN`) so we never surface its contents in logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XaiworkModelConfig")
            .field("model_id", &self.model_id)
            .field("name", &self.name)
            .field("base_url", &self.base_url)
            .field("api_key", &"<redacted>")
            .field("config_json", &self.config_json.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// Outbound HTTP timeout for the OpenApi config call. Chosen conservatively
/// so a slow XAIWork server doesn't stall AionUi model-switch UX.
const CONFIG_FETCH_TIMEOUT: Duration = Duration::from_secs(10);

/// Fetch the full model config list for `backend` from XAIWork OpenApi.
///
/// `xaiwork_host` example: `https://xaiwork.example.com`. Trailing slash is
/// tolerated. `user_token` is forwarded as `Authorization: Bearer <token>`.
///
/// Errors map to `AgentError`:
/// - `bad_request` — client-side misuse (empty host/token, malformed URL)
/// - `internal` — network / upstream envelope decode failure
/// - upstream non-2xx or `success: false` propagates as `internal`
pub async fn fetch_xaiwork_configs(
    xaiwork_host: &str,
    user_token: &str,
    backend: &str,
) -> Result<Vec<XaiworkModelConfig>, AgentError> {
    let host = xaiwork_host.trim();
    if host.is_empty() {
        return Err(AgentError::bad_request("xaiwork_host must not be empty"));
    }
    if user_token.trim().is_empty() {
        return Err(AgentError::bad_request("xaiwork_auth_token must not be empty"));
    }

    let base = host.trim_end_matches('/');
    let url = format!("{base}/openapi/agent/config");

    let client = reqwest::Client::builder()
        .timeout(CONFIG_FETCH_TIMEOUT)
        .build()
        .map_err(|e| AgentError::internal(format!("build http client: {e}")))?;

    let body = serde_json::json!({ "backend": backend });

    let resp = client
        .post(&url)
        .bearer_auth(user_token)
        .header("Cache-Control", "no-store")
        .json(&body)
        .send()
        .await
        .map_err(|e| AgentError::internal(format!("xaiwork /openapi/agent/config request failed: {e}")))?;

    let status = resp.status();
    if !status.is_success() {
        return Err(AgentError::internal(format!(
            "xaiwork /openapi/agent/config returned HTTP {status}"
        )));
    }

    let envelope: XHubEnvelope<Vec<XaiworkModelConfig>> = resp
        .json()
        .await
        .map_err(|e| AgentError::internal(format!("decode xaiwork config envelope: {e}")))?;

    if !envelope.success {
        return Err(AgentError::internal(format!(
            "xaiwork /openapi/agent/config failed: {}",
            envelope.message.as_deref().unwrap_or("unknown error")
        )));
    }

    Ok(envelope.data.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_impl_redacts_sensitive_fields() {
        let cfg = XaiworkModelConfig {
            model_id: "claude-opus-4-7".into(),
            name: "Claude Opus".into(),
            base_url: "https://relay.example.com".into(),
            api_key: "sk-super-secret".into(),
            config_json: Some(r#"{"env":{"ANTHROPIC_AUTH_TOKEN":"sk-x"}}"#.into()),
        };
        let s = format!("{cfg:?}");
        assert!(!s.contains("sk-super-secret"), "api_key leaked: {s}");
        assert!(!s.contains("ANTHROPIC_AUTH_TOKEN"), "config_json leaked: {s}");
        assert!(s.contains("<redacted>"));
        assert!(s.contains("claude-opus-4-7"));
    }

    #[tokio::test]
    async fn empty_host_returns_bad_request() {
        let err = fetch_xaiwork_configs("", "token", "claude").await.unwrap_err();
        assert!(matches!(err, AgentError::BadRequest(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn empty_token_returns_bad_request() {
        let err = fetch_xaiwork_configs("https://example.com", "", "claude")
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::BadRequest(_)), "got {err:?}");
    }
}

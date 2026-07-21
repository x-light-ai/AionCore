// FORK-CUSTOM: Server-to-server config broker. Fetches full agent model
// configurations (including api_key / config_json) from XAIWork OpenApi so
// the AionUi renderer never handles credentials directly. The frontend passes
// its user JWT along with each request; AionCore forwards it as a Bearer
// token and never persists it.

use std::time::Duration;

use serde::Deserialize;

use aionui_ai_agent::AgentError;

/// XAIWork OpenApi wraps every response in `{ traceId, data, success, ... }`.
#[derive(Debug, Deserialize)]
struct XHubEnvelope<T> {
    data: Option<T>,
    #[serde(default)]
    success: bool,
}

/// One entry returned by `POST /openapi/agent/config`. Mirrors XAIWork's
/// `AgentConfigOutDto`. `api_key` / `config_json` are sensitive — see the
/// `Debug` impl below for the redaction contract.
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct XaiworkModelConfig {
    pub model_id: String,
    pub name: String,
    #[serde(default)]
    pub reasoning_efforts: Vec<String>,
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
            .field("reasoning_efforts", &self.reasoning_efforts)
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
/// `xaiwork_base_url` example: `https://xaiwork.example.com`. Trailing slash is
/// tolerated. `user_token` is forwarded as `Authorization: Bearer <token>`.
///
/// Errors map to `AgentError`:
/// - `internal` — missing or invalid server-owned host configuration
/// - `bad_request` — missing per-request user token
/// - `timeout` — upstream request timeout
/// - `bad_gateway` — network, non-2xx, or envelope decode failure
/// - upstream auth/rate-limit statuses preserve their HTTP semantics
pub async fn fetch_xaiwork_configs(
    xaiwork_base_url: &str,
    user_token: &str,
    backend: &str,
) -> Result<Vec<XaiworkModelConfig>, AgentError> {
    let host = xaiwork_base_url.trim();
    if host.is_empty() {
        return Err(AgentError::internal("xaiwork base URL is not configured"));
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
        .map_err(|error| {
            if error.is_timeout() {
                AgentError::timeout("xaiwork config request timed out")
            } else {
                AgentError::bad_gateway(format!("xaiwork config request failed: {error}"))
            }
        })?;

    let status = resp.status();
    if !status.is_success() {
        return Err(match status {
            reqwest::StatusCode::UNAUTHORIZED => AgentError::unauthorized("XAIWork session is invalid or expired"),
            reqwest::StatusCode::FORBIDDEN => AgentError::forbidden("XAIWork model access is forbidden"),
            reqwest::StatusCode::TOO_MANY_REQUESTS => AgentError::RateLimited,
            reqwest::StatusCode::REQUEST_TIMEOUT | reqwest::StatusCode::GATEWAY_TIMEOUT => {
                AgentError::timeout("xaiwork config service timed out")
            }
            _ => AgentError::bad_gateway(format!("xaiwork config service returned HTTP {status}")),
        });
    }

    let envelope: XHubEnvelope<Vec<XaiworkModelConfig>> = resp
        .json()
        .await
        .map_err(|error| AgentError::bad_gateway(format!("decode xaiwork config envelope: {error}")))?;

    if !envelope.success {
        return Err(AgentError::bad_gateway("xaiwork config service rejected the request"));
    }

    Ok(envelope.data.unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn debug_impl_redacts_sensitive_fields() {
        let cfg = XaiworkModelConfig {
            model_id: "claude-opus-4-7".into(),
            name: "Claude Opus".into(),
            reasoning_efforts: vec!["low".into(), "high".into()],
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
    async fn empty_server_host_returns_internal_error() {
        let err = fetch_xaiwork_configs("", "token", "claude").await.unwrap_err();
        assert!(matches!(err, AgentError::Internal(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn empty_token_returns_bad_request() {
        let err = fetch_xaiwork_configs("https://example.com", "", "claude")
            .await
            .unwrap_err();
        assert!(matches!(err, AgentError::BadRequest(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn fetches_configs_from_the_server_owned_host() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openapi/agent/config"))
            .and(header("authorization", "Bearer user-token"))
            .and(body_json(json!({ "backend": "codex" })))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": [{
                    "modelId": "gpt-5.4",
                    "name": "GPT-5.4",
                    "reasoningEfforts": ["low", "medium", "high", "xhigh"],
                    "baseUrl": "https://relay.example.com",
                    "apiKey": "secret-key",
                    "configJson": null
                }]
            })))
            .mount(&server)
            .await;

        let configs = fetch_xaiwork_configs(&server.uri(), "user-token", "codex")
            .await
            .unwrap();

        assert_eq!(configs.len(), 1);
        assert_eq!(configs[0].model_id, "gpt-5.4");
        assert_eq!(configs[0].reasoning_efforts, ["low", "medium", "high", "xhigh"]);
    }

    #[tokio::test]
    async fn missing_reasoning_efforts_remains_backward_compatible() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openapi/agent/config"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "success": true,
                "data": [{
                    "modelId": "legacy-model",
                    "name": "Legacy Model",
                    "baseUrl": "https://relay.example.com",
                    "apiKey": "secret-key",
                    "configJson": null
                }]
            })))
            .mount(&server)
            .await;

        let configs = fetch_xaiwork_configs(&server.uri(), "user-token", "codex")
            .await
            .unwrap();

        assert!(configs[0].reasoning_efforts.is_empty());
    }

    #[tokio::test]
    async fn upstream_service_failure_returns_bad_gateway() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openapi/agent/config"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let error = fetch_xaiwork_configs(&server.uri(), "user-token", "claude")
            .await
            .unwrap_err();

        assert!(matches!(error, AgentError::BadGateway(_)), "got {error:?}");
    }

    #[tokio::test]
    async fn upstream_auth_failure_returns_unauthorized() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openapi/agent/config"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let error = fetch_xaiwork_configs(&server.uri(), "expired-token", "claude")
            .await
            .unwrap_err();

        assert!(matches!(error, AgentError::Unauthorized(_)), "got {error:?}");
    }

    #[tokio::test]
    async fn malformed_upstream_response_returns_bad_gateway() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/openapi/agent/config"))
            .respond_with(ResponseTemplate::new(200).set_body_string("not-json"))
            .mount(&server)
            .await;

        let error = fetch_xaiwork_configs(&server.uri(), "user-token", "claude")
            .await
            .unwrap_err();

        assert!(matches!(error, AgentError::BadGateway(_)), "got {error:?}");
    }
}

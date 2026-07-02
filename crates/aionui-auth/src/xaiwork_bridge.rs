// FORK-CUSTOM: XAIWork WeChat public-account QR login bridge.
//
// Fork-only addition. Lets AionUi finish a WeChat QR login that is driven
// entirely by the remote XAIWork server, then mints a *local* AionCore
// session so the user is also logged into the local backend.
//
// Flow (see doc/aionui-wechat-login-design.md):
//   1. AionUi gets a QR ticket directly from XAIWork.
//   2. AionUi polls this bridge's `POST /api/auth/xaiwork/login` with the ticket.
//   3. The bridge pulls XAIWork `GET /openapi/WeixinAuth/login/{ticket}`:
//        - not yet scanned/subscribed -> `{ status: "pending" }`
//        - confirmed -> XAIWork returns a remote access/refresh token.
//   4. On confirm the bridge mints a local AionCore token for the primary
//      WebUI user (reusing `JwtService` + `CookieConfig`, exactly like
//      `qr_login_handler`) and returns the local session + remote tokens in
//      one response, setting the `aionui-session` cookie.
//
// Why a pull model (not XAIWork -> AionCore push): AionCore runs on the user's
// machine (127.0.0.1) and is generally NOT reachable from the remote XAIWork
// server, so the local side must initiate the call.
//
// Single file with a `fork_xaiwork_` prefix to minimise upstream merge
// conflicts. Upstream wiring is two appended lines (lib.rs mod + routes.rs merge).

use std::sync::OnceLock;
use std::time::Duration;

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router, extract::State};
use serde::{Deserialize, Serialize};

use crate::routes::AuthRouterState;

/// Upstream HTTP timeout for the XAIWork poll call.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Request / response DTOs (AionUi <-> bridge)
// ---------------------------------------------------------------------------

/// Request body for `POST /api/auth/xaiwork/login`.
#[derive(Debug, Deserialize)]
pub struct XaiworkLoginRequest {
    /// The QR `ticket` AionUi obtained directly from XAIWork.
    pub ticket: String,
}

/// Remote authentication issued by XAIWork.
#[derive(Debug, Serialize)]
pub struct RemoteAuth {
    pub access_token: String,
    pub refresh_token: String,
    /// Remote access-token lifetime in seconds (as reported by XAIWork).
    pub access_expires_in: i64,
}

/// Public local user info (mirrors the shape used by the existing login API).
#[derive(Debug, Serialize)]
pub struct BridgePublicUser {
    pub id: String,
    pub username: String,
}

/// Response body for `POST /api/auth/xaiwork/login`.
///
/// `status` is one of:
/// - `pending`   — not scanned / not subscribed yet, keep polling
/// - `expired`   — QR ticket no longer valid; stop polling and refresh the code
/// - `confirmed` — login complete; local + remote auth attached
#[derive(Debug, Serialize)]
pub struct XaiworkLoginResponse {
    pub success: bool,
    pub status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Local AionCore JWT (also set as `aionui-session` cookie). Present when confirmed.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user: Option<BridgePublicUser>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_auth: Option<RemoteAuth>,
    /// WeChat nickname reported by XAIWork, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_nickname: Option<String>,
}

impl XaiworkLoginResponse {
    fn pending() -> Self {
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

    fn expired() -> Self {
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

// ---------------------------------------------------------------------------
// Bridge error (kept crate-local; never use `ApiError` here per clippy.toml)
// ---------------------------------------------------------------------------

/// Errors returned by the bridge. Mapped to HTTP at the handler boundary.
#[derive(Debug)]
enum BridgeError {
    /// Bridge is not configured (empty XAIWork base URL).
    NotConfigured,
    /// Local AionCore is not initialized yet (no primary user).
    LocalSetupRequired,
    /// Could not reach or parse the XAIWork upstream.
    Upstream(String),
    /// Internal failure (token signing, db, etc.).
    Internal(String),
}

impl BridgeError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotConfigured => StatusCode::SERVICE_UNAVAILABLE,
            Self::LocalSetupRequired => StatusCode::CONFLICT,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured => "XAIWORK_NOT_CONFIGURED",
            Self::LocalSetupRequired => "LOCAL_SETUP_REQUIRED",
            Self::Upstream(_) => "XAIWORK_UPSTREAM_ERROR",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    fn public_message(&self) -> String {
        match self {
            Self::NotConfigured => "WeChat login is not configured on this server".to_owned(),
            Self::LocalSetupRequired => "Local AionCore is not initialized yet".to_owned(),
            Self::Upstream(_) => "Failed to reach the WeChat login service".to_owned(),
            // Never leak internal detail to clients.
            Self::Internal(_) => "Internal server error".to_owned(),
        }
    }
}

impl IntoResponse for BridgeError {
    fn into_response(self) -> Response {
        // Log full detail server-side; return only a safe message to clients.
        match &self {
            Self::Upstream(detail) => tracing::warn!(error = %detail, "xaiwork bridge upstream error"),
            Self::Internal(detail) => tracing::error!(error = %detail, "xaiwork bridge internal error"),
            Self::NotConfigured => tracing::warn!("xaiwork bridge called but not configured"),
            Self::LocalSetupRequired => tracing::info!("xaiwork bridge: local setup required"),
        }
        let body = Json(BridgeErrorBody {
            success: false,
            code: self.code(),
            error: self.public_message(),
        });
        (self.status(), body).into_response()
    }
}

/// Error response body (kept local to avoid pulling in serde_json at runtime).
#[derive(Debug, Serialize)]
struct BridgeErrorBody {
    success: bool,
    code: &'static str,
    error: String,
}

// ---------------------------------------------------------------------------
// XAIWork upstream client
// ---------------------------------------------------------------------------

/// Shared reqwest client (XAIWork poll is low-volume; one client is enough).
fn http_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .timeout(UPSTREAM_TIMEOUT)
            .build()
            .unwrap_or_default()
    })
}

/// XAIWork wraps every response as `{ success, message, code, data, traceId }`
/// with camelCase keys (XHub `ActionResponseResult`). `data` is only present
/// on a confirmed login.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct XaiworkEnvelope {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    data: Option<XaiworkLoginData>,
}

/// The `data` payload XAIWork returns once the QR code is scanned + subscribed.
/// Matches `WeixinAuthController.Login`'s anonymous object (camelCase).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct XaiworkLoginData {
    #[serde(default)]
    nick_name: Option<String>,
    access_token: String,
    refresh_token: String,
    #[serde(default)]
    access_expires_in: i64,
}

/// Outcome of polling XAIWork for a given ticket.
enum UpstreamOutcome {
    /// Not scanned / not subscribed yet.
    Pending,
    /// QR ticket no longer exists on XAIWork (expired or unknown).
    Expired,
    /// Confirmed: remote tokens issued.
    Confirmed(XaiworkLoginData),
}

/// XAIWork failure `code` returned when the QR ticket no longer exists.
/// Matches `WeixinAuthController.QrCodeExpiredCode`.
const XAIWORK_QRCODE_EXPIRED: &str = "QRCODE_EXPIRED";

/// Normalize the configured XAIWork base URL, trimming any trailing slash.
fn xaiwork_base_url(raw: &str) -> Result<String, BridgeError> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(BridgeError::NotConfigured);
    }
    Ok(trimmed.to_owned())
}

/// Poll XAIWork `GET /openapi/WeixinAuth/login/{ticket}` once.
///
/// XAIWork returns HTTP 200 in both pending and confirmed cases; the two are
/// distinguished by whether `data.accessToken` is present.
async fn poll_xaiwork(base_url: &str, ticket: &str) -> Result<UpstreamOutcome, BridgeError> {
    let base = xaiwork_base_url(base_url)?;
    // `ticket` is a WeChat-issued opaque token; percent-encode defensively.
    let encoded = urlencode_path_segment(ticket);
    let url = format!("{base}/openapi/WeixinAuth/login/{encoded}");

    let resp = http_client()
        .get(&url)
        .send()
        .await
        .map_err(|e| BridgeError::Upstream(format!("request failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(BridgeError::Upstream(format!("upstream status {}", resp.status())));
    }

    let envelope: XaiworkEnvelope = resp
        .json()
        .await
        .map_err(|e| BridgeError::Upstream(format!("invalid upstream body: {e}")))?;

    match envelope.data {
        Some(data) if !data.access_token.is_empty() => Ok(UpstreamOutcome::Confirmed(data)),
        // No token yet: distinguish an expired/unknown ticket (needs a refresh)
        // from a still-pending scan (keep polling) via XAIWork's failure `code`.
        _ if envelope.code.as_deref() == Some(XAIWORK_QRCODE_EXPIRED) => Ok(UpstreamOutcome::Expired),
        // success == false (e.g. "请扫码关注登录") or no token yet -> still pending.
        _ => {
            let _ = (envelope.success, envelope.message);
            Ok(UpstreamOutcome::Pending)
        }
    }
}

/// Minimal percent-encoding for a single path segment (RFC 3986 unreserved set).
fn urlencode_path_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Handler + router
// ---------------------------------------------------------------------------

/// Build the bridge router. Merged into the main auth router by `auth_routes`.
///
/// Endpoint:
/// - `POST /api/auth/xaiwork/login` — poll-and-exchange (anonymous; the QR
///   ticket itself is the proof of identity, exactly like `/api/auth/qr-login`).
pub fn fork_xaiwork_bridge_routes(state: AuthRouterState) -> Router {
    Router::new()
        .route("/api/auth/xaiwork/login", post(xaiwork_login_handler))
        .with_state(state)
}

/// `POST /api/auth/xaiwork/login`
///
/// Pulls XAIWork for the given ticket. If confirmed, mints a local AionCore
/// session for the primary WebUI user (reusing the same path as
/// `qr_login_handler`) and returns both local + remote auth.
async fn xaiwork_login_handler(
    State(state): State<AuthRouterState>,
    Json(req): Json<XaiworkLoginRequest>,
) -> Result<Response, BridgeError> {
    if req.ticket.trim().is_empty() {
        // Treat an empty ticket as still-pending rather than an error, so the
        // UI polling loop stays simple.
        return Ok(Json(XaiworkLoginResponse::pending()).into_response());
    }

    match poll_xaiwork(&state.xaiwork_base_url, &req.ticket).await? {
        UpstreamOutcome::Pending => Ok(Json(XaiworkLoginResponse::pending()).into_response()),
        UpstreamOutcome::Expired => Ok(Json(XaiworkLoginResponse::expired()).into_response()),
        UpstreamOutcome::Confirmed(remote) => mint_local_session(&state, remote).await,
    }
}

/// Mint a local AionCore session and attach the remote tokens.
///
/// Reuses `get_primary_webui_user` + `JwtService::sign` + `CookieConfig`,
/// mirroring `qr_login_handler` so local sessions are indistinguishable.
async fn mint_local_session(state: &AuthRouterState, remote: XaiworkLoginData) -> Result<Response, BridgeError> {
    let user = state
        .user_repo
        .get_primary_webui_user()
        .await
        .map_err(|e| BridgeError::Internal(format!("db error: {e}")))?
        .ok_or(BridgeError::LocalSetupRequired)?;

    let token = state
        .jwt_service
        .sign(&user.id, &user.username)
        .map_err(|e| BridgeError::Internal(format!("token signing error: {e}")))?;

    // Best-effort last-login update (matches qr_login_handler behaviour).
    if let Err(e) = state.user_repo.update_last_login(&user.id).await {
        tracing::warn!("Failed to update last login for {}: {e}", user.id);
    }

    let cookie = state.cookie_config.build_session_cookie(&token);

    let body = XaiworkLoginResponse {
        success: true,
        status: "confirmed",
        message: Some("Login successful".to_owned()),
        token: Some(token),
        user: Some(BridgePublicUser {
            id: user.id,
            username: user.username,
        }),
        remote_auth: Some(RemoteAuth {
            access_token: remote.access_token,
            refresh_token: remote.refresh_token,
            access_expires_in: remote.access_expires_in,
        }),
        remote_nickname: remote.nick_name,
    };

    Ok(([(header::SET_COOKIE, cookie)], Json(body)).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_response_has_no_secrets() {
        let r = XaiworkLoginResponse::pending();
        assert_eq!(r.status, "pending");
        assert!(r.token.is_none());
        assert!(r.remote_auth.is_none());
    }

    #[test]
    fn urlencode_keeps_unreserved_and_escapes_others() {
        assert_eq!(urlencode_path_segment("abcXYZ-_.~09"), "abcXYZ-_.~09");
        assert_eq!(urlencode_path_segment("a/b c"), "a%2Fb%20c");
        assert_eq!(urlencode_path_segment("t+k=1"), "t%2Bk%3D1");
    }

    #[test]
    fn bridge_error_status_and_code_mapping() {
        assert_eq!(BridgeError::NotConfigured.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(BridgeError::LocalSetupRequired.code(), "LOCAL_SETUP_REQUIRED");
        assert_eq!(BridgeError::Upstream("x".into()).status(), StatusCode::BAD_GATEWAY);
        // Internal detail must not leak into the public message.
        let msg = BridgeError::Internal("db password leaked".into()).public_message();
        assert!(!msg.contains("password"));
    }

    #[test]
    fn confirmed_envelope_parses_camel_case() {
        let json = r#"{"success":true,"data":{"nickName":"渔夫1号","accessToken":"a.b.c","refreshToken":"r.s.t","accessExpiresIn":7200}}"#;
        let env: XaiworkEnvelope = serde_json::from_str(json).unwrap();
        let data = env.data.expect("data present");
        assert_eq!(data.access_token, "a.b.c");
        assert_eq!(data.refresh_token, "r.s.t");
        assert_eq!(data.access_expires_in, 7200);
        assert_eq!(data.nick_name.as_deref(), Some("渔夫1号"));
    }

    #[test]
    fn pending_envelope_has_no_data() {
        let json = r#"{"success":false,"message":"请扫码关注登录","code":null}"#;
        let env: XaiworkEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.data.is_none());
        assert!(!env.success);
    }

    #[test]
    fn expired_envelope_carries_expired_code() {
        let json = r#"{"success":false,"message":"二维码已失效，请刷新二维码","code":"QRCODE_EXPIRED"}"#;
        let env: XaiworkEnvelope = serde_json::from_str(json).unwrap();
        assert!(env.data.is_none());
        assert_eq!(env.code.as_deref(), Some(XAIWORK_QRCODE_EXPIRED));
    }

    #[test]
    fn expired_response_has_no_secrets() {
        let r = XaiworkLoginResponse::expired();
        assert_eq!(r.status, "expired");
        assert!(r.token.is_none());
        assert!(r.remote_auth.is_none());
    }

}

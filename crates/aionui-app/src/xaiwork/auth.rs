// FORK-CUSTOM: XAIWork WeChat public-account QR login bridge.
//
// Fork-only addition. Lets AionUi finish a WeChat QR login that is driven
// entirely by the remote XAIWork server, then mints a *local* AionCore
// session so the user is also logged into the local backend.
//
// Flow (see doc/aionui-wechat-login-design.md):
//   1. AionUi gets a QR ticket directly from XAIWork.
//   2. AionUi polls this bridge's `POST /api/auth/xaiwork/login` with the ticket.
//   3. The bridge pulls XAIWork per `mode`: `SAAuth/login/{ticket}` (公众号) or
//      `MiniProgramAuth/status/{ticket}` (小程序):
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
// This implementation stays inside the App-level XAIWork integration boundary;
// upstream auth state and routes remain unchanged.

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use axum::extract::Request;
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router, extract::State};
use serde::{Deserialize, Serialize};

use aionui_api_types::{
    WechatLoginMode, XaiworkBridgePublicUser as BridgePublicUser, XaiworkLoginRequest, XaiworkLoginResponse,
    XaiworkRemoteAuth as RemoteAuth,
};
use aionui_auth::{CookieConfig, JwtService};
use aionui_db::IUserRepository;

#[derive(Clone)]
pub struct XaiworkAuthState {
    pub jwt_service: Arc<JwtService>,
    pub user_repo: Arc<dyn IUserRepository>,
    pub cookie_config: Arc<CookieConfig>,
    pub base_url: String,
}

/// Upstream HTTP timeout for the XAIWork poll call.
const UPSTREAM_TIMEOUT: Duration = Duration::from_secs(10);

fn is_masked_upstream_login_path(path: &str) -> bool {
    matches!(
        path,
        "/login"
            | "/qr-login"
            | "/api/auth/status"
            | "/api/auth/qr-login"
            | "/api/auth/change-password"
            | "/api/auth/refresh"
    ) || path.starts_with("/api/webui/")
        || path.starts_with("/api/auth/internal/external-")
        || path.starts_with("/api/auth/internal/users")
}

/// Keep the upstream account system unreachable in the XAIWork runtime.
///
/// Shared session infrastructure remains available to the fork login:
/// /api/auth/user, /logout, and /api/ws-token.
pub(crate) async fn mask_upstream_login_routes(request: Request, next: Next) -> Response {
    if is_masked_upstream_login_path(request.uri().path()) {
        return StatusCode::NOT_FOUND.into_response();
    }
    next.run(request).await
}

fn wechat_poll_url(mode: WechatLoginMode, base: &str, encoded_ticket: &str) -> String {
    match mode {
        WechatLoginMode::Sa => format!("{base}/openapi/weixin/SAAuth/login/{encoded_ticket}"),
        WechatLoginMode::Miniprogram => {
            format!("{base}/openapi/weixin/MiniProgramAuth/status/{encoded_ticket}")
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
    /// Could not reach or parse the XAIWork upstream.
    Upstream(String),
    /// Internal failure (token signing, db, etc.).
    Internal(String),
}

impl BridgeError {
    fn status(&self) -> StatusCode {
        match self {
            Self::NotConfigured => StatusCode::SERVICE_UNAVAILABLE,
            Self::Upstream(_) => StatusCode::BAD_GATEWAY,
            Self::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }

    fn code(&self) -> &'static str {
        match self {
            Self::NotConfigured => "XAIWORK_NOT_CONFIGURED",
            Self::Upstream(_) => "XAIWORK_UPSTREAM_ERROR",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    fn public_message(&self) -> String {
        match self {
            Self::NotConfigured => "WeChat login is not configured on this server".to_owned(),
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
struct XaiworkEnvelope<T = XaiworkLoginData> {
    #[serde(default)]
    success: bool,
    #[serde(default)]
    message: Option<String>,
    #[serde(default)]
    code: Option<String>,
    data: Option<T>,
}

/// The `data` payload XAIWork returns once the QR code is scanned + subscribed.
/// Matches SAAuth `Login` / MiniProgramAuth `CheckStatus`'s anonymous object
/// (camelCase). MiniProgramAuth omits `nickName`, hence it stays optional.
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
/// Matches SAAuth / MiniProgramAuth `QrCodeExpiredCode`.
const XAIWORK_QRCODE_EXPIRED: &str = "QRCODE_EXPIRED";

/// Normalize the configured XAIWork base URL, trimming any trailing slash.
fn xaiwork_base_url(raw: &str) -> Result<String, BridgeError> {
    let trimmed = raw.trim().trim_end_matches('/');
    if trimmed.is_empty() {
        return Err(BridgeError::NotConfigured);
    }
    Ok(trimmed.to_owned())
}

/// Poll XAIWork once for the given `mode` (SAAuth 或 MiniProgramAuth).
///
/// XAIWork returns HTTP 200 in both pending and confirmed cases; the two are
/// distinguished by whether `data.accessToken` is present.
async fn poll_xaiwork(base_url: &str, ticket: &str, mode: WechatLoginMode) -> Result<UpstreamOutcome, BridgeError> {
    let base = xaiwork_base_url(base_url)?;
    // `ticket` is a WeChat-issued opaque token; percent-encode defensively.
    let encoded = urlencode_path_segment(ticket);
    let url = wechat_poll_url(mode, &base, &encoded);

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

/// Member profile returned by MemberOpenApi's `POST /openapi/MemberAuth/profile`.
/// `name` is the stable, unique member account name (never the raw openid).
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MemberProfile {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    nick_name: Option<String>,
}

/// Fetch the current member's profile from MemberOpenApi using the remote access
/// token, so the local account can be keyed by the unique member `name`.
///
/// Weixin and Member are decoupled modules; login (Weixin) only mints tokens,
/// the member identity lives in Member. Hence this second, token-authenticated
/// call rather than coupling the two backends.
async fn fetch_member_profile(base_url: &str, access_token: &str) -> Result<MemberProfile, BridgeError> {
    let base = xaiwork_base_url(base_url)?;
    let url = format!("{base}/openapi/MemberAuth/profile");

    let resp = http_client()
        .post(&url)
        .bearer_auth(access_token)
        .send()
        .await
        .map_err(|e| BridgeError::Upstream(format!("member profile request failed: {e}")))?;

    if !resp.status().is_success() {
        return Err(BridgeError::Upstream(format!(
            "member profile status {}",
            resp.status()
        )));
    }

    let envelope: XaiworkEnvelope<MemberProfile> = resp
        .json()
        .await
        .map_err(|e| BridgeError::Upstream(format!("invalid member profile body: {e}")))?;

    envelope
        .data
        .ok_or_else(|| BridgeError::Upstream("member profile response has no data".to_owned()))
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
pub fn xaiwork_auth_routes(state: XaiworkAuthState) -> Router {
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
    State(state): State<XaiworkAuthState>,
    Json(req): Json<XaiworkLoginRequest>,
) -> Result<Response, BridgeError> {
    if req.ticket.trim().is_empty() {
        // Treat an empty ticket as still-pending rather than an error, so the
        // UI polling loop stays simple.
        return Ok(Json(XaiworkLoginResponse::pending()).into_response());
    }

    match poll_xaiwork(&state.base_url, &req.ticket, req.mode).await? {
        UpstreamOutcome::Pending => Ok(Json(XaiworkLoginResponse::pending()).into_response()),
        UpstreamOutcome::Expired => Ok(Json(XaiworkLoginResponse::expired()).into_response()),
        UpstreamOutcome::Confirmed(remote) => mint_local_session(&state, remote).await,
    }
}

/// Mint a local AionCore session and attach the remote tokens.
///
/// Multi-user: the local account is keyed by the XAIWork member `name` (stable
/// and unique), not a single shared primary user. `WeixinAuth/login` only issues
/// tokens (Weixin and Member are decoupled modules), so the member name is
/// fetched with a second, token-authenticated call to MemberOpenApi's
/// `POST /openapi/MemberAuth/profile`. The local account is then looked up by
/// that name and lazily created (empty password — WeChat is the only credential)
/// so each WeChat member gets an isolated local account. Then reuses
/// `JwtService::sign` + `CookieConfig`, mirroring `qr_login_handler`.
async fn mint_local_session(state: &XaiworkAuthState, remote: XaiworkLoginData) -> Result<Response, BridgeError> {
    let profile = fetch_member_profile(&state.base_url, &remote.access_token).await?;
    let username = profile
        .name
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| BridgeError::Upstream("member profile missing unique name".to_owned()))?;

    let user = find_or_create_local_user(state, username).await?;
    // The upstream multi-user model allows non-password identities without a
    // username. This flow is keyed by the validated XAIWork member name, so use
    // that stable identity if a legacy/adopted row has no local username.
    let local_username = user.username.as_deref().unwrap_or(username);

    let token = state
        .jwt_service
        .sign(&user.id, local_username)
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
            username: local_username.to_owned(),
        }),
        remote_auth: Some(RemoteAuth {
            access_token: remote.access_token,
            refresh_token: remote.refresh_token,
            access_expires_in: remote.access_expires_in,
        }),
        remote_nickname: profile.nick_name.or(remote.nick_name),
    };

    Ok(([(header::SET_COOKIE, cookie)], Json(body)).into_response())
}

/// Look up the local user by `username`, creating it on first login.
///
/// WeChat members authenticate only via the remote QR flow, so the local
/// account carries an empty password hash (same shape as `system_default_user`).
/// A concurrent double-login can race two creates for the same new member; the
/// unique-username `Conflict` is resolved by re-reading the row.
async fn find_or_create_local_user(
    state: &XaiworkAuthState,
    username: &str,
) -> Result<aionui_db::models::User, BridgeError> {
    if let Some(user) = state
        .user_repo
        .find_by_username(username)
        .await
        .map_err(|e| BridgeError::Internal(format!("db error: {e}")))?
    {
        return Ok(user);
    }

    match state.user_repo.create_user(username, "").await {
        Ok(user) => Ok(user),
        // Lost a create race: the row now exists, so read it back.
        Err(aionui_db::DbError::Conflict(_)) => state
            .user_repo
            .find_by_username(username)
            .await
            .map_err(|e| BridgeError::Internal(format!("db error: {e}")))?
            .ok_or_else(|| BridgeError::Internal("user vanished after create conflict".to_owned())),
        Err(e) => Err(BridgeError::Internal(format!("db error: {e}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn upstream_login_closure_is_masked_but_fork_login_remains_reachable() {
        use axum::middleware::from_fn;

        let app = Router::new()
            .route("/login", post(|| async { StatusCode::OK }))
            .route("/api/auth/status", post(|| async { StatusCode::OK }))
            .route("/api/auth/refresh", post(|| async { StatusCode::OK }))
            .route("/api/webui/reset-password", post(|| async { StatusCode::OK }))
            .route(
                "/api/auth/internal/external-sessions",
                post(|| async { StatusCode::OK }),
            )
            .route("/api/auth/xaiwork/login", post(|| async { StatusCode::OK }))
            .layer(from_fn(mask_upstream_login_routes));

        for path in [
            "/login",
            "/api/auth/status",
            "/api/auth/refresh",
            "/api/webui/reset-password",
            "/api/auth/internal/external-sessions",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(path).method("POST").body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path} must be masked");
        }

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/xaiwork/login")
                    .method("POST")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }

    #[test]
    fn pending_response_has_no_secrets() {
        let r = XaiworkLoginResponse::pending();
        assert_eq!(r.status, "pending");
        assert!(r.token.is_none());
        assert!(r.remote_auth.is_none());
    }

    #[test]
    fn poll_url_differs_by_mode() {
        let base = "http://localhost:5330";
        assert_eq!(
            wechat_poll_url(WechatLoginMode::Sa, base, "t-1"),
            "http://localhost:5330/openapi/weixin/SAAuth/login/t-1"
        );
        assert_eq!(
            wechat_poll_url(WechatLoginMode::Miniprogram, base, "t-1"),
            "http://localhost:5330/openapi/weixin/MiniProgramAuth/status/t-1"
        );
    }

    #[test]
    fn mode_defaults_to_sa_and_parses_lowercase() {
        // Older clients omit `mode` -> default sa.
        let req: XaiworkLoginRequest = serde_json::from_str(r#"{"ticket":"t"}"#).unwrap();
        assert!(matches!(req.mode, WechatLoginMode::Sa));
        let req: XaiworkLoginRequest = serde_json::from_str(r#"{"ticket":"t","mode":"miniprogram"}"#).unwrap();
        assert!(matches!(req.mode, WechatLoginMode::Miniprogram));
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
        assert_eq!(BridgeError::NotConfigured.code(), "XAIWORK_NOT_CONFIGURED");
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
    fn member_profile_envelope_parses_camel_case() {
        // MemberOpenApi wraps the profile in the same XHub envelope (camelCase).
        let json = r#"{"success":true,"data":{"name":"w123456","nickName":"渔夫1号"}}"#;
        let env: XaiworkEnvelope<MemberProfile> = serde_json::from_str(json).unwrap();
        let data = env.data.expect("data present");
        // The unique member name keys the local multi-user account.
        assert_eq!(data.name.as_deref(), Some("w123456"));
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

    // -----------------------------------------------------------------------
    // find_or_create_local_user: multi-user keying by member name
    // -----------------------------------------------------------------------

    use aionui_db::{IUserRepository, SqliteUserRepository, init_database_memory};

    async fn test_state() -> XaiworkAuthState {
        let db = init_database_memory().await.unwrap();
        let user_repo = Arc::new(SqliteUserRepository::new(db.pool().clone())) as Arc<dyn IUserRepository>;
        XaiworkAuthState {
            jwt_service: Arc::new(JwtService::new("test_secret".into())),
            user_repo,
            cookie_config: Arc::new(CookieConfig {
                secure: false,
                same_site: "Lax",
            }),
            base_url: "http://localhost:5330".to_owned(),
        }
    }

    #[tokio::test]
    async fn find_or_create_is_idempotent_by_name() {
        let state = test_state().await;
        // init_database_memory seeds a system_default_user, so measure the delta.
        let base = state.user_repo.count_users().await.unwrap();

        let first = find_or_create_local_user(&state, "w123456").await.unwrap();
        let again = find_or_create_local_user(&state, "w123456").await.unwrap();

        // Same member name -> same local account, no duplicate row.
        assert_eq!(first.id, again.id);
        assert_eq!(again.username.as_deref(), Some("w123456"));
        assert_eq!(state.user_repo.count_users().await.unwrap(), base + 1);
    }

    #[tokio::test]
    async fn distinct_names_create_distinct_users() {
        let state = test_state().await;
        let base = state.user_repo.count_users().await.unwrap();

        let a = find_or_create_local_user(&state, "w111").await.unwrap();
        let b = find_or_create_local_user(&state, "w222").await.unwrap();

        // Different WeChat members map to isolated local accounts.
        assert_ne!(a.id, b.id);
        assert_eq!(state.user_repo.count_users().await.unwrap(), base + 2);
    }

    #[tokio::test]
    async fn login_route_rejects_missing_ticket() {
        let app = xaiwork_auth_routes(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/xaiwork/login")
                    .method("POST")
                    .header("content-type", "application/json")
                    .body(Body::from("{}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[tokio::test]
    async fn login_route_rejects_get() {
        let app = xaiwork_auth_routes(test_state().await);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/auth/xaiwork/login")
                    .method("GET")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }
}

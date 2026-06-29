//! FORK-CUSTOM: Integration tests for XAIWork WeChat QR login bridge.
//!
//! Tests the bridge endpoint POST /api/auth/xaiwork/login that polls XAIWork's
//! remote WeChat QR login status and mints a local AionCore session on confirm.

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use serde_json::json;

use aionui_auth::{AuthRouterState, CookieConfig, JwtService, QrTokenStore, auth_routes};
use aionui_db::{IUserRepository, SqliteUserRepository, init_database_memory};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

/// Create a test app with the XAIWork bridge enabled.
async fn test_bridge_app() -> Router {
    let db = init_database_memory().await.unwrap();
    let user_repo = Arc::new(SqliteUserRepository::new(db.pool().clone())) as Arc<dyn IUserRepository>;
    let jwt_service = Arc::new(JwtService::new("test_secret_for_bridge".into()));
    let cookie_config = Arc::new(CookieConfig {
        secure: false,
        same_site: "Lax",
    });
    let qr_token_store = Arc::new(QrTokenStore::new());

    let state = AuthRouterState {
        jwt_service: jwt_service.clone(),
        user_repo: user_repo.clone(),
        cookie_config,
        qr_token_store: qr_token_store.clone(),
        local: false,
        xaiwork_base_url: "http://localhost:5330".to_owned(),
    };

    // `auth_routes` already merges the XAIWork bridge routes internally
    // (see routes.rs), so we must not merge them a second time here or the
    // router panics with an overlapping-route error.
    auth_routes(state)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn test_xaiwork_login_endpoint_exists() {
    let app = test_bridge_app().await;

    let request = Request::builder()
        .uri("/api/auth/xaiwork/login")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(json!({"ticket": "test-ticket"}).to_string()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Endpoint should exist (not 404)
    // Will fail with 502 or other error because XAIWork is not running,
    // but that proves the route is registered
    assert_ne!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_xaiwork_login_requires_ticket() {
    let app = test_bridge_app().await;

    // Request without ticket field
    let request = Request::builder()
        .uri("/api/auth/xaiwork/login")
        .method("POST")
        .header("content-type", "application/json")
        .body(Body::from(json!({}).to_string()))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should fail validation (400 or 422)
    assert!(
        response.status() == StatusCode::BAD_REQUEST
            || response.status() == StatusCode::UNPROCESSABLE_ENTITY
    );
}

#[tokio::test]
async fn test_xaiwork_login_rejects_get() {
    let app = test_bridge_app().await;

    let request = Request::builder()
        .uri("/api/auth/xaiwork/login")
        .method("GET")
        .body(Body::empty())
        .unwrap();

    let response = app.oneshot(request).await.unwrap();

    // Should reject GET (405 Method Not Allowed)
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
}

// ---------------------------------------------------------------------------
// Note: Full integration tests with mock XAIWork server would go here.
// For now, these tests verify the endpoint routing and basic validation.
// ---------------------------------------------------------------------------

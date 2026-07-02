// FORK-CUSTOM: HTTP routes for the XAIWork config broker.
//
// AionUi calls these endpoints instead of hitting XAIWork OpenApi directly,
// so credentials (`api_key`, `config_json`) never leave the AionCore backend.
//
// - `POST /api/agents/xaiwork/models` — list public model info (id + name) for
//   a builtin agent backend. AionCore fetches full configs from XAIWork
//   server-to-server and strips credentials before returning.
// - `POST /api/agents/xaiwork/apply` — apply a selected model to the local
//   builtin agent. AionCore fetches configs, matches the requested `model_id`,
//   and delegates to the existing `set_builtin_agent_config` service.
//
// The frontend JWT (`xaiwork_token`) is forwarded once per request and never
// stored in AionCore.

use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::routing::post;
use axum::Router;

use aionui_api_types::{
    ApiResponse, ApplyXaiworkModelRequest, ListXaiworkModelsRequest, XaiworkPublicModel,
};
use aionui_auth::CurrentUser;
use aionui_common::ApiError;

use crate::error::AgentError;
use crate::routes::error_mapping::agent_error_to_api_error;
use crate::routes::state::AgentRouterState;
use crate::services::xaiwork_config::fetch_xaiwork_configs;

/// Single wire-up point registered by `agent.rs::fork_agent_routes()`.
pub fn fork_xaiwork_routes() -> Router<AgentRouterState> {
    Router::new()
        .route("/api/agents/xaiwork/models", post(list_xaiwork_models))
        .route("/api/agents/xaiwork/apply", post(apply_xaiwork_model))
}

/// Return public (credential-free) model info for the given backend.
async fn list_xaiwork_models(
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<ListXaiworkModelsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Vec<XaiworkPublicModel>>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let configs = fetch_xaiwork_configs(&req.xaiwork_host, &req.xaiwork_auth_token, &req.backend)
        .await
        .map_err(agent_error_to_api_error)?;
    let public: Vec<XaiworkPublicModel> = configs
        .into_iter()
        .map(|c| XaiworkPublicModel {
            model_id: c.model_id,
            name: c.name,
        })
        .collect();
    tracing::info!(
        backend = %req.backend,
        model_count = public.len(),
        "xaiwork: listed public models"
    );
    Ok(Json(ApiResponse::ok(public)))
}

/// Apply the selected model to the local builtin agent.
///
/// Flow: fetch full configs from XAIWork -> match `model_id` -> delegate to
/// `set_builtin_agent_config` (writes agent env + local CLI settings).
async fn apply_xaiwork_model(
    State(state): State<AgentRouterState>,
    Extension(_user): Extension<CurrentUser>,
    body: Result<Json<ApplyXaiworkModelRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let configs = fetch_xaiwork_configs(&req.xaiwork_host, &req.xaiwork_auth_token, &req.backend)
        .await
        .map_err(agent_error_to_api_error)?;
    let cfg = configs
        .into_iter()
        .find(|c| c.model_id == req.model_id)
        .ok_or_else(|| {
            agent_error_to_api_error(AgentError::not_found(format!(
                "XAIWork model '{}' not found for backend '{}'",
                req.model_id, req.backend
            )))
        })?;
    let config_json = cfg.config_json.as_deref().unwrap_or("");
    state
        .service
        .set_builtin_agent_config(&req.backend, &cfg.base_url, &cfg.api_key, &cfg.model_id, config_json)
        .await
        .map_err(agent_error_to_api_error)?;
    tracing::info!(
        backend = %req.backend,
        model_id = %req.model_id,
        "xaiwork: applied distributed model config"
    );
    Ok(Json(ApiResponse::ok(())))
}

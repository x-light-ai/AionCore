use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::Router;

use aionui_api_types::{
    ApiResponse, BatchImportMcpServersRequest, CreateMcpServerRequest, McpServerResponse,
    UpdateMcpServerRequest,
};
use aionui_common::AppError;

use crate::service::McpConfigService;

// ---------------------------------------------------------------------------
// Router state
// ---------------------------------------------------------------------------

/// Shared state for MCP route handlers.
#[derive(Clone)]
pub struct McpRouterState {
    pub config_service: McpConfigService,
}

// ---------------------------------------------------------------------------
// Router builder
// ---------------------------------------------------------------------------

/// Build the MCP router with all `/api/mcp/*` routes.
///
/// CRUD routes are mounted here. Agent sync, connection test, and OAuth
/// routes will be added in subsequent tasks.
///
/// All routes require authentication (applied by the caller).
pub fn mcp_routes(state: McpRouterState) -> Router {
    Router::new()
        .route(
            "/api/mcp/servers",
            get(list_servers).post(add_server),
        )
        .route("/api/mcp/servers/import", post(batch_import))
        .route(
            "/api/mcp/servers/{id}",
            get(get_server).put(edit_server).delete(delete_server),
        )
        .route("/api/mcp/servers/{id}/toggle", post(toggle_server))
        .with_state(state)
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// `GET /api/mcp/servers` — list all MCP servers.
async fn list_servers(
    State(state): State<McpRouterState>,
) -> Result<Json<ApiResponse<Vec<McpServerResponse>>>, AppError> {
    let servers = state.config_service.list_servers().await?;
    Ok(Json(ApiResponse::ok(servers)))
}

/// `GET /api/mcp/servers/:id` — get a single MCP server.
async fn get_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<McpServerResponse>>, AppError> {
    let server = state.config_service.get_server(&id).await?;
    Ok(Json(ApiResponse::ok(server)))
}

/// `POST /api/mcp/servers` — create (or upsert by name) an MCP server.
async fn add_server(
    State(state): State<McpRouterState>,
    body: Result<Json<CreateMcpServerRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiResponse<McpServerResponse>>), AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let server = state.config_service.add_server(req).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(server))))
}

/// `PUT /api/mcp/servers/:id` — partial update an MCP server.
async fn edit_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
    body: Result<Json<UpdateMcpServerRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<McpServerResponse>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let server = state.config_service.edit_server(&id, req).await?;
    Ok(Json(ApiResponse::ok(server)))
}

/// `DELETE /api/mcp/servers/:id` — delete an MCP server.
async fn delete_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<()>>, AppError> {
    // TODO: if was_enabled, trigger remove-from-agents (task 8.8)
    let _was_enabled = state.config_service.delete_server(&id).await?;
    Ok(Json(ApiResponse::success()))
}

/// `POST /api/mcp/servers/:id/toggle` — toggle enabled state.
async fn toggle_server(
    State(state): State<McpRouterState>,
    Path(id): Path<String>,
) -> Result<Json<ApiResponse<McpServerResponse>>, AppError> {
    // TODO: trigger sync/remove based on new enabled state (task 8.8)
    let server = state.config_service.toggle_server(&id).await?;
    Ok(Json(ApiResponse::ok(server)))
}

/// `POST /api/mcp/servers/import` — batch import MCP servers.
async fn batch_import(
    State(state): State<McpRouterState>,
    body: Result<Json<BatchImportMcpServersRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<Vec<McpServerResponse>>>, AppError> {
    let Json(req) = body.map_err(|e| AppError::BadRequest(e.to_string()))?;
    let servers = state.config_service.batch_import(req).await?;
    Ok(Json(ApiResponse::ok(servers)))
}

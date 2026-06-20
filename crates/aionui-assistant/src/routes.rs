#![allow(clippy::disallowed_types)]

//! HTTP route handlers for `/api/assistants/*`.

use std::fs;
use std::path::Path;

use axum::Router;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Path as AxumPath, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::Response;
use axum::routing::{get, patch, post};

use aionui_api_types::{
    ApiResponse, AssistantDetailResponse, AssistantResponse, CreateAssistantRequest, ImportAssistantsRequest,
    ImportAssistantsResult, ImportRemoteAssistantsRequest, SetAssistantStateRequest, UpdateAssistantRequest,
};
use aionui_common::ApiError;
use tempfile::tempdir;
use zip::ZipArchive;

use crate::error::AssistantError;
pub use crate::state::AssistantRouterState;

/// Build the router for `/api/assistants/*`.
pub fn assistant_routes(state: AssistantRouterState) -> Router {
    Router::new()
        .route("/api/assistants", get(list).post(create))
        .route("/api/assistants/{id}", get(get_one).put(update).delete(delete_one))
        .route("/api/assistants/{id}/state", patch(set_state))
        .route("/api/assistants/{id}/avatar", get(get_avatar))
        .route("/api/assistants/import", post(import))
        .route("/api/assistants/import-remote", post(import_remote))
        .with_state(state)
}

#[derive(Debug, serde::Deserialize, Default)]
struct GetAssistantDetailQuery {
    locale: Option<String>,
}

impl From<AssistantError> for ApiError {
    fn from(error: AssistantError) -> Self {
        match error {
            AssistantError::NotFound(message) => Self::NotFound(message),
            AssistantError::BadRequest(message) => Self::BadRequest(message),
            AssistantError::Forbidden(message) => Self::Forbidden(message),
            AssistantError::Conflict(message) => Self::Conflict(message),
            AssistantError::Internal(message) => Self::Internal(message),
        }
    }
}

async fn list(
    State(state): State<AssistantRouterState>,
) -> Result<Json<ApiResponse<Vec<AssistantResponse>>>, ApiError> {
    let items = state.service.list().await?;
    Ok(Json(ApiResponse::ok(items)))
}

async fn create(
    State(state): State<AssistantRouterState>,
    body: Result<Json<CreateAssistantRequest>, JsonRejection>,
) -> Result<(StatusCode, Json<ApiResponse<AssistantResponse>>), ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let created = state.service.create(req).await?;
    Ok((StatusCode::CREATED, Json(ApiResponse::ok(created))))
}

async fn get_one(
    State(state): State<AssistantRouterState>,
    AxumPath(id): AxumPath<String>,
    Query(query): Query<GetAssistantDetailQuery>,
) -> Result<Json<ApiResponse<AssistantDetailResponse>>, ApiError> {
    let detail = state.service.get_detail(&id, query.locale.as_deref()).await?;
    Ok(Json(ApiResponse::ok(detail)))
}

async fn update(
    State(state): State<AssistantRouterState>,
    AxumPath(id): AxumPath<String>,
    body: Result<Json<UpdateAssistantRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssistantResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let updated = state.service.update(&id, req).await?;
    Ok(Json(ApiResponse::ok(updated)))
}

async fn delete_one(
    State(state): State<AssistantRouterState>,
    AxumPath(id): AxumPath<String>,
) -> Result<Json<ApiResponse<()>>, ApiError> {
    state.service.delete(&id).await?;
    Ok(Json(ApiResponse::success()))
}

async fn set_state(
    State(state): State<AssistantRouterState>,
    AxumPath(id): AxumPath<String>,
    body: Result<Json<SetAssistantStateRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<AssistantResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let resp = state.service.set_state(&id, req).await?;
    Ok(Json(ApiResponse::ok(resp)))
}

async fn import(
    State(state): State<AssistantRouterState>,
    body: Result<Json<ImportAssistantsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ImportAssistantsResult>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let result = state.service.import(req).await?;
    Ok(Json(ApiResponse::ok(result)))
}

async fn import_remote(
    State(state): State<AssistantRouterState>,
    body: Result<Json<ImportRemoteAssistantsRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ImportAssistantsResult>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let temp_dir = tempdir().map_err(|error| ApiError::Internal(format!("create temp dir failed: {error}")))?;
    let archive_path = temp_dir.path().join("assistant-market.zip");

    let response = reqwest::get(&req.url)
        .await
        .map_err(|error| ApiError::BadRequest(format!("download remote assistant failed: {error}")))?;
    if !response.status().is_success() {
        return Err(ApiError::BadRequest(format!(
            "download remote assistant failed with status {}",
            response.status()
        )));
    }
    let bytes = response
        .bytes()
        .await
        .map_err(|error| ApiError::BadRequest(format!("read remote assistant bytes failed: {error}")))?;
    fs::write(&archive_path, &bytes)
        .map_err(|error| ApiError::Internal(format!("write remote assistant archive failed: {error}")))?;

    let extract_dir = temp_dir.path().join("assistant-market");
    fs::create_dir_all(&extract_dir)
        .map_err(|error| ApiError::Internal(format!("create assistant extract dir failed: {error}")))?;
    extract_zip_archive(&archive_path, &extract_dir)?;

    let manifest_path = extract_dir.join("assistants.json");
    let manifest = fs::read_to_string(&manifest_path)
        .map_err(|error| ApiError::BadRequest(format!("read assistants.json failed: {error}")))?;
    let req = serde_json::from_str::<ImportAssistantsRequest>(&manifest)
        .map_err(|error| ApiError::BadRequest(format!("parse assistants.json failed: {error}")))?;
    let result = state.service.import(req).await?;
    Ok(Json(ApiResponse::ok(result)))
}

/// Serve the raw avatar bytes for an assistant. Content-Type inferred from the
/// file extension (png/jpg/svg default). Extensions return 404 — the frontend
/// serves those via `aion-asset://`.
async fn get_avatar(State(state): State<AssistantRouterState>, AxumPath(id): AxumPath<String>) -> Result<Response, ApiError> {
    let asset = state
        .service
        .avatar_asset(&id)
        .await
        .ok_or_else(|| ApiError::NotFound(format!("avatar '{id}' not found")))?;

    let content_type = content_type_for_extension(asset.extension.as_deref());

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(asset.bytes))
        .map_err(|e| ApiError::Internal(e.to_string()))
}

fn content_type_for_extension(ext: Option<&str>) -> HeaderValue {
    let mime = match ext {
        Some("svg") => "image/svg+xml",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("webp") => "image/webp",
        _ => "application/octet-stream",
    };
    HeaderValue::from_static(mime)
}

fn extract_zip_archive(archive_path: &Path, destination: &Path) -> Result<(), ApiError> {
    let file = std::fs::File::open(archive_path)
        .map_err(|error| ApiError::BadRequest(format!("open assistant zip failed: {error}")))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| ApiError::BadRequest(format!("invalid assistant zip archive: {error}")))?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| ApiError::BadRequest(format!("read assistant zip entry failed: {error}")))?;
        let entry_name = entry.name().to_string();
        if entry_name.contains("..") || entry_name.starts_with('/') || entry_name.contains(':') {
            return Err(ApiError::BadRequest("invalid assistant zip entry path".into()));
        }

        let out_path = destination.join(&entry_name);
        if entry.is_dir() {
            std::fs::create_dir_all(&out_path)
                .map_err(|error| ApiError::Internal(format!("create assistant entry dir failed: {error}")))?;
            continue;
        }

        if let Some(parent) = out_path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|error| ApiError::Internal(format!("create assistant entry parent failed: {error}")))?;
        }

        let mut out_file = std::fs::File::create(&out_path)
            .map_err(|error| ApiError::Internal(format!("create assistant extracted file failed: {error}")))?;
        std::io::copy(&mut entry, &mut out_file)
            .map_err(|error| ApiError::Internal(format!("extract assistant file failed: {error}")))?;
    }

    Ok(())
}

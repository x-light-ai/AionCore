// FORK-CUSTOM: XAIWork Skill import and metadata routes.
#![allow(clippy::disallowed_types)]

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use aionui_api_types::{
    ApiResponse, ImportRemoteSkillRequest, ImportSkillFailureResponse, ImportSkillResponse,
    XaiworkInstalledSkillMetadata,
};
use aionui_auth::CurrentUser;
use aionui_common::ApiError;
use aionui_db::ISkillRepository;
use aionui_extension::{self, SkillPaths};
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, State};
use axum::routing::{get, post};
use tempfile::NamedTempFile;
use tracing::warn;

use super::skill_metadata::{
    list_installed_skill_metadata_for_user, persist_skill_market_metadata_for_user,
    snapshot_installed_skill_metadata_for_user,
};

#[derive(Clone)]
pub struct XaiworkSkillState {
    pub skill_paths: Arc<SkillPaths>,
    pub skill_repo: Arc<dyn ISkillRepository>,
}

pub fn xaiwork_skill_routes(state: XaiworkSkillState) -> Router {
    Router::new()
        .route("/api/skills/import-remote", post(import_remote_skill))
        .route("/api/xaiwork/skills/metadata", get(list_metadata))
        .with_state(state)
}

async fn list_metadata(
    State(state): State<XaiworkSkillState>,
    Extension(current_user): Extension<CurrentUser>,
) -> Json<ApiResponse<Vec<XaiworkInstalledSkillMetadata>>> {
    Json(ApiResponse::ok(
        list_installed_skill_metadata_for_user(&state.skill_paths, &current_user.id).await,
    ))
}

async fn import_remote_skill(
    State(state): State<XaiworkSkillState>,
    Extension(current_user): Extension<CurrentUser>,
    body: Result<Json<ImportRemoteSkillRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ImportSkillResponse>>, ApiError> {
    let Json(req) = body.map_err(ApiError::from)?;
    let archive = download_remote_archive(&req.url).await?;
    let previous = snapshot_installed_skill_metadata_for_user(&state.skill_paths, &current_user.id).await;
    let outcome = aionui_extension::skill_service::import_skills_with_repo_for_user(
        &state.skill_paths,
        state.skill_repo.as_ref(),
        &current_user.id,
        archive.path(),
    )
    .await?;
    if !outcome.failed.is_empty() {
        warn!(
            url = %req.url,
            imported_count = outcome.imported.len(),
            failed_count = outcome.failed.len(),
            failures = ?outcome.failed,
            "remote skill import completed with failures"
        );
    }

    let names = outcome.imported;
    persist_skill_market_metadata_for_user(
        &state.skill_paths,
        &current_user.id,
        &names,
        req.description.as_deref(),
        req.version.as_deref(),
        &req.tags,
        &previous,
    )
    .await?;
    let first_name = names.first().cloned().unwrap_or_default();
    let failed = outcome
        .failed
        .into_iter()
        .map(|failure| ImportSkillFailureResponse {
            source_name: failure.source_name,
            code: failure.code,
            error_path: failure.error_path,
            actual_bytes: failure.actual_bytes,
            limit_bytes: failure.limit_bytes,
            line: failure.line,
            column: failure.column,
        })
        .collect();
    Ok(Json(ApiResponse::ok(ImportSkillResponse {
        skill_name: first_name,
        skill_names: names,
        failed,
    })))
}

async fn download_remote_archive(url: &str) -> Result<NamedTempFile, ApiError> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(120))
        .build()
        .map_err(|error| ApiError::Internal(format!("create remote skill client failed: {error}")))?;
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|error| ApiError::BadRequest(format!("download remote skill failed: {error}")))?;
    if !response.status().is_success() {
        return Err(ApiError::BadRequest(format!(
            "download remote skill failed with status {}",
            response.status()
        )));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|error| ApiError::BadRequest(format!("read remote skill bytes failed: {error}")))?;
    let mut file = tempfile::Builder::new()
        .suffix(".zip")
        .tempfile()
        .map_err(|error| ApiError::Internal(format!("create temp file failed: {error}")))?;
    file.write_all(&bytes)
        .map_err(|error| ApiError::Internal(format!("write temp file failed: {error}")))?;
    Ok(file)
}

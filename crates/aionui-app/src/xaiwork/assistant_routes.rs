#![allow(clippy::disallowed_types)]

use std::fs;
use std::io;
use std::sync::Arc;

use aionui_api_types::{ApiResponse, ImportAssistantsRequest, ImportAssistantsResult, ImportRemoteAssistantsRequest};
use aionui_assistant::AssistantService;
use aionui_common::ApiError;
use aionui_db::ISkillRepository;
use aionui_extension::SkillPaths;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, State};
use axum::routing::post;
use tempfile::tempdir;
use zip::ZipArchive;

use super::assistant_import::{apply_bundled_rule, ensure_packaged_assistant_id, import_bundled_skills};

#[derive(Clone)]
pub struct XaiworkAssistantState {
    pub service: Arc<AssistantService>,
    pub skill_paths: Arc<SkillPaths>,
    pub skill_repo: Arc<dyn ISkillRepository>,
}

pub fn xaiwork_assistant_routes(state: XaiworkAssistantState) -> Router {
    Router::new()
        .route("/api/assistants/import-remote", post(import_remote))
        .with_state(state)
}

async fn import_remote(
    State(state): State<XaiworkAssistantState>,
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

    let manifest = fs::read_to_string(extract_dir.join("assistants.json"))
        .map_err(|error| ApiError::BadRequest(format!("read assistants.json failed: {error}")))?;
    let mut import_request = serde_json::from_str::<ImportAssistantsRequest>(&manifest)
        .map_err(|error| ApiError::BadRequest(format!("parse assistants.json failed: {error}")))?;
    let assistant_id = ensure_packaged_assistant_id(&mut import_request);

    import_bundled_skills(&state, &extract_dir, assistant_id.as_deref()).await?;
    let result = state.service.import(import_request).await.map_err(ApiError::from)?;
    if let Some(id) = assistant_id.as_deref() {
        apply_bundled_rule(&state, &extract_dir, id).await;
    }

    Ok(Json(ApiResponse::ok(result)))
}

fn extract_zip_archive(archive_path: &std::path::Path, destination: &std::path::Path) -> Result<(), ApiError> {
    let file = fs::File::open(archive_path)
        .map_err(|error| ApiError::BadRequest(format!("open assistant zip failed: {error}")))?;
    let mut archive = ZipArchive::new(file)
        .map_err(|error| ApiError::BadRequest(format!("invalid assistant zip archive: {error}")))?;

    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|error| ApiError::BadRequest(format!("read assistant zip entry failed: {error}")))?;
        let relative = entry
            .enclosed_name()
            .ok_or_else(|| ApiError::BadRequest("invalid assistant zip entry path".into()))?;
        let out_path = destination.join(relative);
        if entry.is_dir() {
            fs::create_dir_all(&out_path)
                .map_err(|error| ApiError::Internal(format!("create assistant entry dir failed: {error}")))?;
            continue;
        }
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| ApiError::Internal(format!("create assistant entry parent failed: {error}")))?;
        }
        let mut output = fs::File::create(&out_path)
            .map_err(|error| ApiError::Internal(format!("create assistant extracted file failed: {error}")))?;
        io::copy(&mut entry, &mut output)
            .map_err(|error| ApiError::Internal(format!("extract assistant file failed: {error}")))?;
    }
    Ok(())
}

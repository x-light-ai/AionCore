// FORK-CUSTOM: XAIWork workspace sandbox HTTP routes.
#![allow(clippy::disallowed_types)]
//! Sandboxed server-workspace browsing for the XAIWork WebUI.

use std::fs;
use std::path::{Path, PathBuf};

use aionui_api_types::{ApiResponse, BrowseDirectoryQuery, BrowseDirectoryResponse};
use aionui_common::ApiError;
use aionui_file::{FileError, browse};
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Query};
use axum::routing::{get, post};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct CreateDirectoryRequest {
    path: String,
}

pub(crate) fn xaiwork_workspace_routes() -> Router {
    Router::new()
        .route("/api/xaiwork/fs/browse", get(browse_directory))
        .route("/api/xaiwork/fs/mkdir", post(create_directory))
}

async fn browse_directory(
    Query(query): Query<BrowseDirectoryQuery>,
) -> Result<Json<ApiResponse<BrowseDirectoryResponse>>, ApiError> {
    let root = workspace_root();
    let requested = query
        .path
        .filter(|path| !path.trim().is_empty())
        .unwrap_or_else(|| root.to_string_lossy().into_owned());
    let show_files = matches!(query.show_files.as_deref(), Some("true") | Some("1"));

    let response = tokio::task::spawn_blocking(move || browse::browse(Some(&requested), show_files, &[root]))
        .await
        .map_err(|error| ApiError::Internal(format!("workspace browse task failed: {error}")))??;

    Ok(Json(ApiResponse::ok(response)))
}

async fn create_directory(
    body: Result<Json<CreateDirectoryRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<String>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    let raw = request.path.trim();
    if raw.is_empty() || raw.contains('\0') {
        return Err(ApiError::BadRequest("directory path is required".to_owned()));
    }

    let root = workspace_root();
    let raw = raw.to_owned();
    let target = tokio::task::spawn_blocking(move || create_sandboxed_directory(&raw, &[root]))
        .await
        .map_err(|error| ApiError::Internal(format!("workspace mkdir task failed: {error}")))??;

    Ok(Json(ApiResponse::ok(response_path(&target))))
}

fn workspace_root() -> PathBuf {
    std::env::var_os("AIONUI_WORKSPACE_ROOT")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
}

fn create_sandboxed_directory(raw: &str, allowed_roots: &[PathBuf]) -> Result<PathBuf, FileError> {
    let requested = PathBuf::from(raw.trim());
    let name = requested
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| FileError::BadRequest("invalid directory name".into()))?;
    let parent = requested
        .parent()
        .ok_or_else(|| FileError::BadRequest("directory must have a parent".into()))?;
    let parent = browse::resolve_browse_path(&parent.to_string_lossy(), allowed_roots)?;
    let target = parent.join(name);

    fs::create_dir(&target).map_err(|error| match error.kind() {
        std::io::ErrorKind::AlreadyExists => FileError::BadRequest("directory already exists".into()),
        _ => FileError::BadRequest(format!("cannot create directory: {error}")),
    })?;
    Ok(target)
}

fn response_path(path: &Path) -> String {
    let path = path.to_string_lossy();
    if let Some(rest) = path.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = path.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        path.into_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_directory_inside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let target = workspace.path().join("new-project");

        let created = create_sandboxed_directory(target.to_str().unwrap(), &[workspace.path().to_owned()]).unwrap();

        assert!(created.is_dir());
    }

    #[test]
    fn rejects_directory_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("escaped-project");

        let error = create_sandboxed_directory(target.to_str().unwrap(), &[workspace.path().to_owned()]).unwrap_err();

        assert!(matches!(error, FileError::PathOutsideSandbox { .. }));
    }
}

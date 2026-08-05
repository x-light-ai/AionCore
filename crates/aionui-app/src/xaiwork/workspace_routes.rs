// FORK-CUSTOM: XAIWork workspace sandbox HTTP routes.
#![allow(clippy::disallowed_types)]
//! Sandboxed server-workspace browsing for the XAIWork WebUI.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use aionui_api_types::ApiResponse;
use aionui_common::ApiError;
use aionui_file::FileError;
use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Json, Query};
use axum::routing::{get, post};
use serde::{Deserialize, Serialize};

const MAX_BROWSE_ITEMS: usize = 500;

#[derive(Debug, Deserialize)]
struct BrowseDirectoryQuery {
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    show_files: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowseEntry {
    name: String,
    path: String,
    is_directory: bool,
    is_file: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modified: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct BrowseDirectoryResponse {
    current_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_path: Option<String>,
    items: Vec<BrowseEntry>,
    can_go_up: bool,
    truncated: bool,
}

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

    let response = tokio::task::spawn_blocking(move || browse_workspace(&requested, show_files, &root))
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
    let root = allowed_roots
        .first()
        .ok_or_else(|| FileError::Internal("workspace root is not configured".into()))?;
    let parent = resolve_workspace_path(&parent.to_string_lossy(), root)?;
    let target = parent.join(name);

    fs::create_dir(&target).map_err(|error| match error.kind() {
        std::io::ErrorKind::AlreadyExists => FileError::BadRequest("directory already exists".into()),
        _ => FileError::BadRequest(format!("cannot create directory: {error}")),
    })?;
    Ok(target)
}

fn browse_workspace(raw: &str, show_files: bool, root: &Path) -> Result<BrowseDirectoryResponse, FileError> {
    let dir = resolve_workspace_path(raw, root)?;
    if !dir.is_dir() {
        return Err(FileError::BadRequest("path is not a directory".into()));
    }

    let read = fs::read_dir(&dir).map_err(|error| FileError::Internal(format!("readdir failed: {error}")))?;
    let mut items = Vec::new();
    for entry in read.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        let is_directory = metadata.is_dir();
        let is_file = metadata.is_file();
        if !show_files && !is_directory {
            continue;
        }
        items.push(BrowseEntry {
            name,
            path: response_path(&path),
            is_directory,
            is_file,
            size: Some(metadata.len()),
            modified: system_time_to_millis(metadata.modified().ok()),
        });
    }

    items.sort_by(|left, right| match (left.is_directory, right.is_directory) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => left.name.cmp(&right.name),
    });
    let truncated = items.len() > MAX_BROWSE_ITEMS;
    items.truncate(MAX_BROWSE_ITEMS);

    let canonical_root = fs::canonicalize(root)
        .map_err(|error| FileError::BadRequest(format!("cannot resolve workspace root: {error}")))?;
    let parent_path = dir
        .parent()
        .filter(|parent| parent.starts_with(&canonical_root))
        .map(response_path);

    Ok(BrowseDirectoryResponse {
        current_path: response_path(&dir),
        can_go_up: parent_path.is_some(),
        parent_path,
        items,
        truncated,
    })
}

fn resolve_workspace_path(raw: &str, root: &Path) -> Result<PathBuf, FileError> {
    if raw.contains('\0') {
        return Err(FileError::BadRequest("path contains null byte".into()));
    }
    let canonical_root = fs::canonicalize(root)
        .map_err(|error| FileError::BadRequest(format!("cannot resolve workspace root: {error}")))?;
    let requested = if raw.trim().is_empty() {
        root
    } else {
        Path::new(raw.trim())
    };
    let canonical = fs::canonicalize(requested).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => FileError::NotFound(format!("path not found: {raw}")),
        _ => FileError::BadRequest(format!("cannot resolve path '{raw}': {error}")),
    })?;
    if canonical.starts_with(&canonical_root) {
        Ok(canonical)
    } else {
        Err(FileError::PathOutsideSandbox {
            message: format!("path '{raw}' is outside the workspace sandbox"),
            field: Some("directory"),
            operation: Some("browse"),
        })
    }
}

fn system_time_to_millis(time: Option<SystemTime>) -> Option<i64> {
    time?.duration_since(UNIX_EPOCH).ok()?.as_millis().try_into().ok()
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

    #[test]
    fn browse_lists_directories_and_filters_files() {
        let workspace = tempfile::tempdir().unwrap();
        fs::create_dir(workspace.path().join("project")).unwrap();
        fs::write(workspace.path().join("notes.txt"), "notes").unwrap();

        let directories = browse_workspace("", false, workspace.path()).unwrap();
        assert_eq!(directories.items.len(), 1);
        assert!(directories.items[0].is_directory);

        let all = browse_workspace("", true, workspace.path()).unwrap();
        assert_eq!(all.items.len(), 2);
        assert!(all.items[0].is_directory);
        assert!(all.items[1].is_file);
    }

    #[test]
    fn browse_rejects_path_outside_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let error = browse_workspace(outside.path().to_str().unwrap(), false, workspace.path()).unwrap_err();

        assert!(matches!(error, FileError::PathOutsideSandbox { .. }));
    }
}

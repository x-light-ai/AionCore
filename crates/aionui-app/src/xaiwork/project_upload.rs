// FORK-CUSTOM: XAIWork WebHost uploads browser files directly into the authenticated project workspace.
#![allow(clippy::disallowed_types)]
//! Project-scoped multipart upload for the XAIWork WebHost.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Component, Path};

use aionui_api_types::ApiResponse;
use aionui_auth::CurrentUser;
use aionui_common::ApiError;
use aionui_project::{FileOp, ProjectService, ReferenceInput};
use axum::extract::{DefaultBodyLimit, Extension, Multipart, State};
use axum::routing::post;
use axum::{Json, Router};
use serde::Serialize;
use tower_http::limit::RequestBodyLimitLayer;

const PROJECT_UPLOAD_MAX_SIZE: usize = 30 * 1024 * 1024;

#[derive(Clone)]
pub(crate) struct XaiworkProjectUploadState {
    pub project: ProjectService,
}

#[derive(Debug, Serialize)]
struct ProjectUploadResponse {
    pe_id: String,
    relative_path: String,
}

struct ProjectUploadFields {
    pe_id: String,
    relative_path: String,
    file_data: Vec<u8>,
}

pub(crate) fn xaiwork_project_upload_routes(state: XaiworkProjectUploadState) -> Router {
    Router::new()
        .route("/api/xaiwork/project-files/upload", post(upload_project_file))
        .layer(DefaultBodyLimit::disable())
        .layer(RequestBodyLimitLayer::new(PROJECT_UPLOAD_MAX_SIZE))
        .with_state(state)
}

async fn upload_project_file(
    State(state): State<XaiworkProjectUploadState>,
    Extension(user): Extension<CurrentUser>,
    multipart: Multipart,
) -> Result<Json<ApiResponse<ProjectUploadResponse>>, ApiError> {
    let fields = extract_project_upload(multipart).await?;
    let root = state
        .project
        .resolve_reference(
            &user.id,
            ReferenceInput {
                pe_id: fields.pe_id.clone(),
                relative_path: String::new(),
                op: FileOp::Write,
            },
        )
        .await
        .map_err(ApiError::from)?;
    let target = state
        .project
        .resolve_reference(
            &user.id,
            ReferenceInput {
                pe_id: fields.pe_id.clone(),
                relative_path: fields.relative_path.clone(),
                op: FileOp::Write,
            },
        )
        .await
        .map_err(ApiError::from)?;

    let root_path = root
        .absolute_path
        .ok_or_else(|| ApiError::BadRequest("project workspace is not a local path".to_owned()))?;
    let target_path = target
        .absolute_path
        .ok_or_else(|| ApiError::BadRequest("upload target is not a local path".to_owned()))?;
    let data = fields.file_data;
    let stored_relative = tokio::task::spawn_blocking(move || {
        store_project_upload(Path::new(&root_path), Path::new(&target_path), &data)
    })
    .await
    .map_err(|error| ApiError::Internal(format!("project upload task failed: {error}")))??;

    Ok(Json(ApiResponse::ok(ProjectUploadResponse {
        pe_id: fields.pe_id,
        relative_path: stored_relative,
    })))
}

async fn extract_project_upload(mut multipart: Multipart) -> Result<ProjectUploadFields, ApiError> {
    let mut pe_id: Option<String> = None;
    let mut relative_path: Option<String> = None;
    let mut disposition_name: Option<String> = None;
    let mut file_data: Option<Vec<u8>> = None;

    while let Some(field) = multipart
        .next_field()
        .await
        .map_err(|error| ApiError::BadRequest(format!("multipart error: {error}")))?
    {
        let name = field.name().unwrap_or_default().to_owned();
        match name.as_str() {
            "file" => {
                disposition_name = field.file_name().map(str::to_owned);
                file_data = Some(
                    field
                        .bytes()
                        .await
                        .map_err(|error| ApiError::BadRequest(format!("failed to read file: {error}")))?
                        .to_vec(),
                );
            }
            "pe_id" => {
                pe_id = Some(
                    field
                        .text()
                        .await
                        .map_err(|error| ApiError::BadRequest(format!("failed to read pe_id: {error}")))?,
                );
            }
            "relative_path" => {
                relative_path = Some(
                    field
                        .text()
                        .await
                        .map_err(|error| ApiError::BadRequest(format!("failed to read relative_path: {error}")))?,
                );
            }
            _ => {}
        }
    }

    let pe_id = pe_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ApiError::BadRequest("missing pe_id".to_owned()))?;
    let relative_path = normalize_relative_path(relative_path.as_deref(), disposition_name.as_deref())?;
    let file_data = file_data.ok_or_else(|| ApiError::BadRequest("missing file".to_owned()))?;

    Ok(ProjectUploadFields {
        pe_id,
        relative_path,
        file_data,
    })
}

fn normalize_relative_path(relative_path: Option<&str>, fallback_name: Option<&str>) -> Result<String, ApiError> {
    let raw = relative_path
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .or_else(|| fallback_name.map(str::trim).filter(|value| !value.is_empty()))
        .ok_or_else(|| ApiError::BadRequest("missing relative_path or file name".to_owned()))?;
    if raw.contains('\0') {
        return Err(ApiError::BadRequest("relative_path contains a null byte".to_owned()));
    }

    let normalized = raw.replace('\\', "/");
    let path = Path::new(&normalized);
    if path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(ApiError::BadRequest(
            "relative_path must stay inside the project workspace".to_owned(),
        ));
    }
    Ok(normalized)
}

fn store_project_upload(root: &Path, requested: &Path, data: &[u8]) -> Result<String, ApiError> {
    let canonical_root = std::fs::canonicalize(root)
        .map_err(|error| ApiError::BadRequest(format!("cannot resolve project workspace: {error}")))?;
    let parent = requested
        .parent()
        .ok_or_else(|| ApiError::BadRequest("upload target must have a parent directory".to_owned()))?;
    let mut existing_ancestor = parent;
    while !existing_ancestor.exists() {
        existing_ancestor = existing_ancestor
            .parent()
            .ok_or_else(|| ApiError::BadRequest("upload target is outside the project workspace".to_owned()))?;
    }
    let canonical_ancestor = std::fs::canonicalize(existing_ancestor)
        .map_err(|error| ApiError::BadRequest(format!("cannot resolve upload target: {error}")))?;
    if !canonical_ancestor.starts_with(&canonical_root) {
        return Err(ApiError::BadRequest(
            "upload target is outside the project workspace".to_owned(),
        ));
    }

    std::fs::create_dir_all(parent)
        .map_err(|error| ApiError::Internal(format!("cannot create upload directory: {error}")))?;
    let canonical_parent = std::fs::canonicalize(parent)
        .map_err(|error| ApiError::BadRequest(format!("cannot resolve upload directory: {error}")))?;
    if !canonical_parent.starts_with(&canonical_root) {
        return Err(ApiError::BadRequest(
            "upload target is outside the project workspace".to_owned(),
        ));
    }

    let requested_name = requested
        .file_name()
        .ok_or_else(|| ApiError::BadRequest("upload target must include a file name".to_owned()))?;
    let mut counter = 1_u32;
    loop {
        let candidate_name = if counter == 1 {
            requested_name.to_owned()
        } else {
            suffixed_file_name(requested_name, counter)
        };
        let candidate = canonical_parent.join(candidate_name);
        match OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(mut file) => {
                if let Err(error) = file.write_all(data) {
                    let _ = std::fs::remove_file(&candidate);
                    return Err(ApiError::Internal(format!("cannot write uploaded file: {error}")));
                }
                let relative = candidate
                    .strip_prefix(&canonical_root)
                    .map_err(|_| ApiError::BadRequest("upload target is outside the project workspace".to_owned()))?;
                return Ok(relative.to_string_lossy().replace('\\', "/"));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists && counter < 1000 => {
                counter += 1;
            }
            Err(error) => return Err(ApiError::Internal(format!("cannot create uploaded file: {error}"))),
        }
    }
}

fn suffixed_file_name(name: &std::ffi::OsStr, counter: u32) -> std::ffi::OsString {
    let path = Path::new(name);
    let stem = path.file_stem().unwrap_or(name).to_string_lossy();
    match path.extension() {
        Some(extension) => format!("{stem}({counter}).{}", extension.to_string_lossy()).into(),
        None => format!("{stem}({counter})").into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_upload_preserves_relative_path_and_never_overwrites() {
        let workspace = tempfile::tempdir().unwrap();
        let requested = workspace.path().join("folder/readme.md");

        let first = store_project_upload(workspace.path(), &requested, b"first").unwrap();
        let second = store_project_upload(workspace.path(), &requested, b"second").unwrap();

        assert_eq!(first, "folder/readme.md");
        assert_eq!(second, "folder/readme(2).md");
        assert_eq!(std::fs::read(workspace.path().join(first)).unwrap(), b"first");
        assert_eq!(std::fs::read(workspace.path().join(second)).unwrap(), b"second");
    }

    #[test]
    fn relative_path_rejects_traversal_and_absolute_paths() {
        assert!(normalize_relative_path(Some("../secret.txt"), None).is_err());
        assert!(normalize_relative_path(Some("/secret.txt"), None).is_err());
        assert!(normalize_relative_path(Some(r"C:\\secret.txt"), None).is_err());
    }

    #[test]
    fn storage_rejects_a_parent_outside_the_workspace() {
        let workspace = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();

        let result = store_project_upload(workspace.path(), &outside.path().join("secret.txt"), b"secret");

        assert!(result.is_err());
        assert!(!outside.path().join("secret.txt").exists());
    }
}

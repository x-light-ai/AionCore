// FORK-CUSTOM: XAIWork project file copy/move operations stay behind project refs.
#![allow(clippy::disallowed_types)]
//! Authenticated project-scoped file and directory transfer operations.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use aionui_api_types::ApiResponse;
use aionui_auth::CurrentUser;
use aionui_common::ApiError;
use aionui_project::{FileOp, ProjectService, ReferenceInput, ResolvedResource};
use axum::Router;
use axum::body::Body;
use axum::extract::rejection::JsonRejection;
use axum::extract::{Extension, Json, Query, State};
use axum::http::header;
use axum::response::Response;
use axum::routing::post;
use serde::{Deserialize, Serialize};

#[derive(Clone)]
pub(crate) struct XaiworkProjectFileOpsState {
    pub project: ProjectService,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(rename_all = "snake_case")]
enum TransferOperation {
    Copy,
    Move,
}

#[derive(Debug, Deserialize)]
struct ProjectFileRef {
    pe_id: String,
    relative_path: String,
}

#[derive(Debug, Deserialize)]
struct ProjectFileTransferRequest {
    source: ProjectFileRef,
    target: ProjectFileRef,
    operation: TransferOperation,
}

#[derive(Debug, Serialize)]
struct ProjectFileTransferResponse {
    pe_id: String,
    relative_path: String,
}

#[derive(Debug, Deserialize)]
struct ProjectFileDownloadQuery {
    pe_id: String,
    relative_path: String,
}

pub(crate) fn xaiwork_project_file_ops_routes(state: XaiworkProjectFileOpsState) -> Router {
    Router::new()
        .route("/api/xaiwork/project-files/transfer", post(transfer_project_file))
        .route(
            "/api/xaiwork/project-files/download",
            axum::routing::get(download_project_file),
        )
        .with_state(state)
}

async fn download_project_file(
    State(state): State<XaiworkProjectFileOpsState>,
    Extension(user): Extension<CurrentUser>,
    Query(query): Query<ProjectFileDownloadQuery>,
) -> Result<Response, ApiError> {
    let resolved = resolve_local(&state.project, &user.id, query.pe_id, query.relative_path, FileOp::Read).await?;
    let path = PathBuf::from(resolved.absolute_path.as_deref().expect("validated local path"));
    let name = path
        .file_name()
        .ok_or_else(|| ApiError::BadRequest("project entry has no name".to_owned()))?
        .to_string_lossy()
        .into_owned();
    let (bytes, filename, content_type) = tokio::task::spawn_blocking(move || {
        ensure_no_symlinks(&path)?;
        if path.is_file() {
            let bytes = fs::read(&path).map_err(internal_io("cannot read project file"))?;
            Ok::<_, ApiError>((bytes, name, "application/octet-stream".to_owned()))
        } else if path.is_dir() {
            let mut archive = Vec::new();
            let mut zip = zip::ZipWriter::new(std::io::Cursor::new(&mut archive));
            append_directory_to_zip(&mut zip, &path, Path::new(&name))?;
            zip.finish().map_err(archive_error("cannot finish project archive"))?;
            Ok((archive, format!("{name}.zip"), "application/zip".to_owned()))
        } else {
            Err(ApiError::BadRequest("project entry type is not supported".to_owned()))
        }
    })
    .await
    .map_err(|error| ApiError::Internal(format!("project download task failed: {error}")))??;
    let disposition = format!("attachment; filename=\"{}\"", filename.replace('"', ""));
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        .header(header::CONTENT_DISPOSITION, disposition)
        .body(Body::from(bytes))
        .map_err(|_| ApiError::Internal("cannot build project download response".to_owned()))
}

fn append_directory_to_zip(
    zip: &mut zip::ZipWriter<std::io::Cursor<&mut Vec<u8>>>,
    directory: &Path,
    archive_path: &Path,
) -> Result<(), ApiError> {
    let options = zip::write::SimpleFileOptions::default();
    zip.add_directory(
        format!("{}/", archive_path.to_string_lossy().replace('\\', "/")),
        options,
    )
    .map_err(archive_error("cannot create project archive directory"))?;
    for entry in fs::read_dir(directory).map_err(internal_io("cannot read project directory"))? {
        let entry = entry.map_err(internal_io("cannot read project directory"))?;
        let path = entry.path();
        let child = archive_path.join(entry.file_name());
        ensure_no_symlinks(&path)?;
        if path.is_dir() {
            append_directory_to_zip(zip, &path, &child)?;
        } else {
            zip.start_file(child.to_string_lossy().replace('\\', "/"), options)
                .map_err(archive_error("cannot create project archive file"))?;
            let contents = fs::read(&path).map_err(internal_io("cannot read project archive file"))?;
            std::io::Write::write_all(zip, &contents).map_err(internal_io("cannot write project archive file"))?;
        }
    }
    Ok(())
}

async fn transfer_project_file(
    State(state): State<XaiworkProjectFileOpsState>,
    Extension(user): Extension<CurrentUser>,
    body: Result<Json<ProjectFileTransferRequest>, JsonRejection>,
) -> Result<Json<ApiResponse<ProjectFileTransferResponse>>, ApiError> {
    let Json(request) = body.map_err(ApiError::from)?;
    if request.source.relative_path.trim().is_empty() {
        return Err(ApiError::BadRequest("a project root cannot be transferred".to_owned()));
    }

    let source_op = match request.operation {
        TransferOperation::Copy => FileOp::Read,
        TransferOperation::Move => FileOp::Remove,
    };
    let source = resolve_local(
        &state.project,
        &user.id,
        request.source.pe_id,
        request.source.relative_path,
        source_op,
    )
    .await?;
    let source_root = resolve_local(
        &state.project,
        &user.id,
        source.pe_id.clone(),
        String::new(),
        FileOp::Browse,
    )
    .await?;
    let target = resolve_local(
        &state.project,
        &user.id,
        request.target.pe_id.clone(),
        request.target.relative_path,
        FileOp::Write,
    )
    .await?;
    let target_root = resolve_local(
        &state.project,
        &user.id,
        target.pe_id.clone(),
        String::new(),
        FileOp::Browse,
    )
    .await?;
    if source.project_id != target.project_id {
        return Err(ApiError::BadRequest(
            "source and target must belong to the same project".to_owned(),
        ));
    }

    let source_pe_id = source.pe_id.clone();
    let target_pe_id = request.target.pe_id;
    let operation = request.operation;
    let stored_relative = tokio::task::spawn_blocking(move || {
        transfer_local_entry(&source, &source_root, &target, &target_root, operation)
    })
    .await
    .map_err(|error| ApiError::Internal(format!("project transfer task failed: {error}")))??;

    tracing::info!(
        operation = ?operation,
        source_pe_id = %source_pe_id,
        target_pe_id = %target_pe_id,
        "xaiwork project file transfer completed"
    );
    Ok(Json(ApiResponse::ok(ProjectFileTransferResponse {
        pe_id: target_pe_id,
        relative_path: stored_relative,
    })))
}

async fn resolve_local(
    project: &ProjectService,
    user_id: &str,
    pe_id: String,
    relative_path: String,
    op: FileOp,
) -> Result<ResolvedResource, ApiError> {
    let resolved = project
        .resolve_reference(
            user_id,
            ReferenceInput {
                pe_id,
                relative_path,
                op,
            },
        )
        .await
        .map_err(ApiError::from)?;
    if resolved.absolute_path.is_none() {
        return Err(ApiError::BadRequest("project entry is not a local path".to_owned()));
    }
    Ok(resolved)
}

fn transfer_local_entry(
    source: &ResolvedResource,
    source_root: &ResolvedResource,
    target_dir: &ResolvedResource,
    target_root: &ResolvedResource,
    operation: TransferOperation,
) -> Result<String, ApiError> {
    let source_path = Path::new(source.absolute_path.as_deref().expect("validated local source"));
    let target_dir_path = Path::new(target_dir.absolute_path.as_deref().expect("validated local target"));
    let source_root = fs::canonicalize(
        source_root
            .absolute_path
            .as_deref()
            .expect("validated local source root"),
    )
    .map_err(|_| ApiError::BadRequest("project workspace is unavailable".to_owned()))?;
    let target_root = fs::canonicalize(
        target_root
            .absolute_path
            .as_deref()
            .expect("validated local target root"),
    )
    .map_err(|_| ApiError::BadRequest("project workspace is unavailable".to_owned()))?;
    ensure_no_symlinks(source_path)?;
    let canonical_source =
        fs::canonicalize(source_path).map_err(|_| ApiError::BadRequest("source entry does not exist".to_owned()))?;
    let canonical_target_dir = fs::canonicalize(target_dir_path)
        .map_err(|_| ApiError::BadRequest("target directory does not exist".to_owned()))?;

    if !canonical_source.starts_with(&source_root) || !canonical_target_dir.starts_with(&target_root) {
        return Err(ApiError::BadRequest(
            "project entry is outside its workspace".to_owned(),
        ));
    }
    if !canonical_target_dir.is_dir() {
        return Err(ApiError::BadRequest("transfer target must be a directory".to_owned()));
    }
    let source_name = canonical_source
        .file_name()
        .ok_or_else(|| ApiError::BadRequest("source entry has no name".to_owned()))?;
    let source_parent = canonical_source.parent();
    if matches!(operation, TransferOperation::Move)
        && source.pe_id == target_dir.pe_id
        && source_parent == Some(canonical_target_dir.as_path())
    {
        return Ok(source.relative_path.clone());
    }
    if canonical_source.is_dir() && canonical_target_dir.starts_with(&canonical_source) {
        return Err(ApiError::BadRequest(
            "a directory cannot be transferred into itself".to_owned(),
        ));
    }

    let destination = unique_destination(&canonical_target_dir, source_name)?;
    match operation {
        TransferOperation::Copy => copy_entry(&canonical_source, &destination)?,
        TransferOperation::Move => move_entry(&canonical_source, &destination)?,
    }

    destination
        .strip_prefix(&target_root)
        .map(|relative| relative.to_string_lossy().replace('\\', "/"))
        .map_err(|_| ApiError::BadRequest("transfer target is outside the workspace".to_owned()))
}

fn ensure_no_symlinks(source: &Path) -> Result<(), ApiError> {
    let metadata =
        fs::symlink_metadata(source).map_err(|_| ApiError::BadRequest("source entry does not exist".to_owned()))?;
    if metadata.file_type().is_symlink() {
        return Err(ApiError::BadRequest("symbolic links cannot be transferred".to_owned()));
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(source).map_err(internal_io("cannot read source directory"))? {
            let entry = entry.map_err(internal_io("cannot read source directory"))?;
            ensure_no_symlinks(&entry.path())?;
        }
    }
    Ok(())
}

fn unique_destination(parent: &Path, name: &OsStr) -> Result<PathBuf, ApiError> {
    for counter in 1..=1000_u32 {
        let candidate_name = if counter == 1 {
            name.to_owned()
        } else {
            suffixed_name(name, counter)
        };
        let candidate = parent.join(candidate_name);
        if !candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(ApiError::BadRequest(
        "too many entries share the same name in the target directory".to_owned(),
    ))
}

fn suffixed_name(name: &OsStr, counter: u32) -> OsString {
    let path = Path::new(name);
    let stem = path.file_stem().unwrap_or(name).to_string_lossy();
    match path.extension() {
        Some(extension) => format!("{stem}({counter}).{}", extension.to_string_lossy()).into(),
        None => format!("{stem}({counter})").into(),
    }
}

fn copy_entry(source: &Path, destination: &Path) -> Result<(), ApiError> {
    let metadata = fs::symlink_metadata(source).map_err(internal_io("cannot inspect source entry"))?;
    if metadata.file_type().is_symlink() {
        return Err(ApiError::BadRequest("symbolic links cannot be transferred".to_owned()));
    }
    if metadata.is_file() {
        fs::copy(source, destination).map_err(internal_io("cannot copy source file"))?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(ApiError::BadRequest("source entry type is not supported".to_owned()));
    }

    fs::create_dir(destination).map_err(internal_io("cannot create target directory"))?;
    let result = (|| {
        for entry in fs::read_dir(source).map_err(internal_io("cannot read source directory"))? {
            let entry = entry.map_err(internal_io("cannot read source directory"))?;
            copy_entry(&entry.path(), &destination.join(entry.file_name()))?;
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_dir_all(destination);
    }
    result
}

fn move_entry(source: &Path, destination: &Path) -> Result<(), ApiError> {
    if fs::rename(source, destination).is_ok() {
        return Ok(());
    }

    copy_entry(source, destination)?;
    let remove_result = if source.is_dir() {
        fs::remove_dir_all(source)
    } else {
        fs::remove_file(source)
    };
    if remove_result.is_err() {
        if destination.is_dir() {
            let _ = fs::remove_dir_all(destination);
        } else {
            let _ = fs::remove_file(destination);
        }
        return Err(ApiError::Internal("cannot remove source after moving it".to_owned()));
    }
    Ok(())
}

fn internal_io(context: &'static str) -> impl FnOnce(std::io::Error) -> ApiError {
    move |error| ApiError::Internal(format!("{context}: {error}"))
}

fn archive_error(context: &'static str) -> impl FnOnce(zip::result::ZipError) -> ApiError {
    move |error| ApiError::Internal(format!("{context}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recursive_copy_preserves_files_and_empty_directories() {
        let source_root = tempfile::tempdir().unwrap();
        let target_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("docs");
        fs::create_dir(&source).unwrap();
        fs::create_dir(source.join("empty")).unwrap();
        fs::write(source.join("readme.md"), "hello").unwrap();

        let destination = target_root.path().join("docs");
        copy_entry(&source, &destination).unwrap();

        assert_eq!(fs::read_to_string(destination.join("readme.md")).unwrap(), "hello");
        assert!(destination.join("empty").is_dir());
    }

    #[test]
    fn recursive_copy_rejects_symlinks() {
        let source_root = tempfile::tempdir().unwrap();
        let target_root = tempfile::tempdir().unwrap();
        let source = source_root.path().join("docs");
        fs::create_dir(&source).unwrap();

        #[cfg(unix)]
        std::os::unix::fs::symlink(source_root.path(), source.join("linked")).unwrap();
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(source_root.path(), source.join("linked")).unwrap();

        let result = copy_entry(&source, &target_root.path().join("docs"));

        assert!(result.is_err());
        assert!(!target_root.path().join("docs").exists());
    }

    #[test]
    fn conflicting_names_receive_a_suffix() {
        let target = tempfile::tempdir().unwrap();
        fs::write(target.path().join("notes.txt"), "first").unwrap();

        let destination = unique_destination(target.path(), OsStr::new("notes.txt")).unwrap();

        assert_eq!(destination.file_name().unwrap(), "notes(2).txt");
    }
}

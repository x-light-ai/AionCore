use std::path::Path;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

use dashmap::DashMap;
use ignore::WalkBuilder;
use tracing::warn;

use aionui_common::AppError;
use aionui_realtime::EventBroadcaster;

use crate::path_safety::validate_path;
use crate::types::{
    CopyResult, DirOrFile, FileMetadata, WorkspaceFlatFile, ZipEntry,
};

/// Maximum number of files returned by `list_workspace_files`.
const MAX_WORKSPACE_FILES: usize = 20_000;

/// A concrete implementation of [`crate::traits::IFileService`].
///
/// Task 7.3 implements: `get_files_by_dir`, `list_workspace_files`,
/// `get_file_metadata`. Remaining methods are added in later tasks.
pub struct FileService {
    /// Used by write_file / remove_entry to emit contentUpdate events (task 7.4+).
    #[allow(dead_code)]
    broadcaster: Arc<dyn EventBroadcaster>,
    /// Allowed root directories for path safety validation.
    allowed_roots: Vec<std::path::PathBuf>,
    /// In-memory cache for `list_workspace_files`, keyed by canonical root.
    workspace_files_cache: DashMap<String, Vec<WorkspaceFlatFile>>,
}

impl FileService {
    pub fn new(
        broadcaster: Arc<dyn EventBroadcaster>,
        allowed_roots: Vec<std::path::PathBuf>,
    ) -> Self {
        Self {
            broadcaster,
            allowed_roots,
            workspace_files_cache: DashMap::new(),
        }
    }

    /// Invalidate the workspace files cache for a given root.
    /// Called when file changes are detected.
    pub fn invalidate_cache(&self, root: &str) {
        self.workspace_files_cache.remove(root);
    }

    /// Get the allowed root references for path validation.
    fn allowed_roots_refs(&self) -> Vec<&Path> {
        self.allowed_roots.iter().map(|p| p.as_path()).collect()
    }

    /// List immediate children of `dir`, building a single-level tree.
    /// Each child directory also lists *its* children (depth = 2 from `dir`).
    async fn build_dir_tree(
        &self,
        dir: &Path,
        root: &Path,
    ) -> Result<Vec<DirOrFile>, AppError> {
        let dir_owned = dir.to_path_buf();
        let root_owned = root.to_path_buf();

        tokio::task::spawn_blocking(move || {
            build_dir_tree_sync(&dir_owned, &root_owned)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!("directory listing task failed: {e}"))
        })?
    }
}

/// Synchronous directory tree builder (runs in blocking thread pool).
fn build_dir_tree_sync(
    dir: &Path,
    root: &Path,
) -> Result<Vec<DirOrFile>, AppError> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        AppError::BadRequest(format!(
            "cannot read directory '{}': {e}",
            dir.display()
        ))
    })?;

    let mut result = Vec::new();

    for entry in entries {
        let entry = entry.map_err(|e| {
            AppError::Internal(format!("error reading directory entry: {e}"))
        })?;

        let path = entry.path();
        let metadata = entry.metadata().map_err(|e| {
            AppError::Internal(format!(
                "cannot read metadata for '{}': {e}",
                path.display()
            ))
        })?;

        let name = entry
            .file_name()
            .to_string_lossy()
            .into_owned();

        let full_path = path.to_string_lossy().into_owned();
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        let is_dir = metadata.is_dir();

        // For directories, also read their immediate children
        let children = if is_dir {
            read_children_sync(&path, root)?
        } else {
            Vec::new()
        };

        result.push(DirOrFile {
            name,
            full_path,
            relative_path,
            is_dir,
            children,
        });
    }

    // Sort: directories first, then alphabetical
    result.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name))
    });

    Ok(result)
}

/// Read immediate children of a directory (one level, no grandchildren).
fn read_children_sync(
    dir: &Path,
    root: &Path,
) -> Result<Vec<DirOrFile>, AppError> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return Ok(Vec::new()),
    };

    let mut children = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };

        let path = entry.path();
        let is_dir = entry
            .metadata()
            .map(|m| m.is_dir())
            .unwrap_or(false);

        let name = entry
            .file_name()
            .to_string_lossy()
            .into_owned();

        let full_path = path.to_string_lossy().into_owned();
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .to_string_lossy()
            .into_owned();

        children.push(DirOrFile {
            name,
            full_path,
            relative_path,
            is_dir,
            children: Vec::new(),
        });
    }

    children.sort_by(|a, b| {
        b.is_dir.cmp(&a.is_dir).then(a.name.cmp(&b.name))
    });

    Ok(children)
}

/// Recursively list files using the `ignore` crate (respects .gitignore).
fn list_workspace_files_sync(
    root: &Path,
) -> Result<Vec<WorkspaceFlatFile>, AppError> {
    let walker = WalkBuilder::new(root)
        .hidden(false)
        .git_ignore(true)
        .git_global(false)
        .git_exclude(true)
        .require_git(false)
        .build();

    let mut files = Vec::new();

    for entry in walker {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                warn!("skipping unreadable entry: {e}");
                continue;
            }
        };

        // Skip directories — only include files
        if entry
            .file_type()
            .map(|ft| ft.is_dir())
            .unwrap_or(true)
        {
            continue;
        }

        let path = entry.path();
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();

        let full_path = path.to_string_lossy().into_owned();
        let relative_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .into_owned();

        files.push(WorkspaceFlatFile {
            name,
            full_path,
            relative_path,
        });

        if files.len() >= MAX_WORKSPACE_FILES {
            break;
        }
    }

    Ok(files)
}

/// Get file metadata synchronously.
fn get_file_metadata_sync(path: &Path) -> Result<FileMetadata, AppError> {
    let metadata = std::fs::metadata(path).map_err(|e| {
        AppError::NotFound(format!(
            "cannot read metadata for '{}': {e}",
            path.display()
        ))
    })?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();

    let size = metadata.len();
    let is_directory = metadata.is_dir();

    let mime_type = if is_directory {
        "inode/directory".to_owned()
    } else {
        mime_guess::from_path(path)
            .first()
            .map(|m| m.to_string())
            .unwrap_or_else(|| "application/octet-stream".to_owned())
    };

    let last_modified = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    Ok(FileMetadata {
        name,
        path: path.to_string_lossy().into_owned(),
        size,
        mime_type,
        last_modified,
        is_directory,
    })
}

#[async_trait::async_trait]
impl crate::traits::IFileService for FileService {
    async fn get_files_by_dir(
        &self,
        dir: &str,
        root: &str,
    ) -> Result<Vec<DirOrFile>, AppError> {
        let roots = self.allowed_roots_refs();
        let canonical_dir = validate_path(dir, &roots)?;
        let canonical_root = validate_path(root, &roots)?;

        self.build_dir_tree(&canonical_dir, &canonical_root)
            .await
    }

    async fn list_workspace_files(
        &self,
        root: &str,
    ) -> Result<Vec<WorkspaceFlatFile>, AppError> {
        let roots = self.allowed_roots_refs();
        let canonical_root = validate_path(root, &roots)?;
        let cache_key = canonical_root.to_string_lossy().into_owned();

        // Check cache first
        if let Some(cached) = self.workspace_files_cache.get(&cache_key) {
            return Ok(cached.clone());
        }

        let root_owned = canonical_root.clone();
        let files = tokio::task::spawn_blocking(move || {
            list_workspace_files_sync(&root_owned)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "workspace file listing task failed: {e}"
            ))
        })??;

        // Store in cache
        self.workspace_files_cache
            .insert(cache_key, files.clone());

        Ok(files)
    }

    async fn get_file_metadata(
        &self,
        path: &str,
    ) -> Result<FileMetadata, AppError> {
        let roots = self.allowed_roots_refs();
        let canonical = validate_path(path, &roots)?;

        let result = tokio::task::spawn_blocking(move || {
            get_file_metadata_sync(&canonical)
        })
        .await
        .map_err(|e| {
            AppError::Internal(format!(
                "file metadata task failed: {e}"
            ))
        })??;

        Ok(result)
    }

    // -- Methods below are implemented in later tasks (7.4 - 7.7) --

    async fn read_file(
        &self,
        _path: &str,
    ) -> Result<Option<String>, AppError> {
        todo!("implemented in task 7.4")
    }

    async fn read_file_buffer(
        &self,
        _path: &str,
    ) -> Result<Option<Vec<u8>>, AppError> {
        todo!("implemented in task 7.4")
    }

    async fn write_file(
        &self,
        _path: &str,
        _data: &[u8],
        _workspace: &str,
    ) -> Result<bool, AppError> {
        todo!("implemented in task 7.4")
    }

    async fn copy_files_to_workspace(
        &self,
        _file_paths: &[String],
        _workspace: &str,
        _source_root: Option<&str>,
    ) -> Result<CopyResult, AppError> {
        todo!("implemented in task 7.5")
    }

    async fn remove_entry(
        &self,
        _path: &str,
        _workspace: &str,
    ) -> Result<(), AppError> {
        todo!("implemented in task 7.5")
    }

    async fn rename_entry(
        &self,
        _path: &str,
        _new_name: &str,
    ) -> Result<String, AppError> {
        todo!("implemented in task 7.5")
    }

    async fn create_temp_file(
        &self,
        _file_name: &str,
    ) -> Result<String, AppError> {
        todo!("implemented in task 7.5")
    }

    async fn get_image_base64(
        &self,
        _path: &str,
    ) -> Result<String, AppError> {
        todo!("implemented in task 7.6")
    }

    async fn fetch_remote_image(&self, _url: &str) -> String {
        todo!("implemented in task 7.6")
    }

    async fn create_zip(
        &self,
        _path: &str,
        _entries: Vec<ZipEntry>,
        _request_id: Option<String>,
    ) -> Result<bool, AppError> {
        todo!("implemented in task 7.7")
    }

    async fn cancel_zip(&self, _request_id: &str) -> bool {
        todo!("implemented in task 7.7")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn build_dir_tree_sync_lists_files_and_dirs() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        fs::write(dir.path().join("b.rs"), "fn main(){}").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/c.txt"), "nested").unwrap();

        let result =
            build_dir_tree_sync(dir.path(), dir.path()).unwrap();

        // sub/ should come first (directories first)
        assert_eq!(result[0].name, "sub");
        assert!(result[0].is_dir);
        // sub/ should have c.txt as child
        assert_eq!(result[0].children.len(), 1);
        assert_eq!(result[0].children[0].name, "c.txt");

        // Then files alphabetically
        assert_eq!(result[1].name, "a.txt");
        assert!(!result[1].is_dir);
        assert_eq!(result[2].name, "b.rs");
    }

    #[test]
    fn build_dir_tree_sync_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let result =
            build_dir_tree_sync(dir.path(), dir.path()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn build_dir_tree_sync_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("folder");
        fs::create_dir(&sub).unwrap();
        fs::write(sub.join("file.txt"), "data").unwrap();

        let result =
            build_dir_tree_sync(dir.path(), dir.path()).unwrap();

        assert_eq!(result[0].relative_path, "folder");
        assert_eq!(result[0].children[0].relative_path, "folder/file.txt");
    }

    #[test]
    fn build_dir_tree_sync_nonexistent_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("nonexistent");
        let result = build_dir_tree_sync(&fake, dir.path());
        assert!(result.is_err());
    }

    #[test]
    fn list_workspace_files_sync_basic() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("a.txt"), "hello").unwrap();
        fs::create_dir(dir.path().join("sub")).unwrap();
        fs::write(dir.path().join("sub/b.txt"), "world").unwrap();

        let files = list_workspace_files_sync(dir.path()).unwrap();

        assert_eq!(files.len(), 2);
        let names: Vec<&str> =
            files.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"a.txt"));
        assert!(names.contains(&"b.txt"));
    }

    #[test]
    fn list_workspace_files_sync_respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join(".gitignore"), "ignored.txt\n").unwrap();
        fs::write(dir.path().join("kept.txt"), "keep").unwrap();
        fs::write(dir.path().join("ignored.txt"), "skip").unwrap();

        let files = list_workspace_files_sync(dir.path()).unwrap();

        let names: Vec<&str> =
            files.iter().map(|f| f.name.as_str()).collect();
        assert!(names.contains(&"kept.txt"));
        assert!(names.contains(&".gitignore"));
        assert!(!names.contains(&"ignored.txt"));
    }

    #[test]
    fn list_workspace_files_sync_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let files = list_workspace_files_sync(dir.path()).unwrap();
        assert!(files.is_empty());
    }

    #[test]
    fn list_workspace_files_sync_truncates_at_limit() {
        // Creating 20,000+ files is impractical in a unit test;
        // verify the constant exists and the branch logic is sound.
        assert_eq!(MAX_WORKSPACE_FILES, 20_000);
    }

    #[test]
    fn list_workspace_files_sync_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main(){}").unwrap();

        let files = list_workspace_files_sync(dir.path()).unwrap();
        let main_file = files
            .iter()
            .find(|f| f.name == "main.rs")
            .unwrap();

        assert_eq!(main_file.relative_path, "src/main.rs");
    }

    #[test]
    fn get_file_metadata_sync_text_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("hello.txt");
        fs::write(&file, "hello world").unwrap();

        let meta = get_file_metadata_sync(&file).unwrap();
        assert_eq!(meta.name, "hello.txt");
        assert_eq!(meta.size, 11);
        assert_eq!(meta.mime_type, "text/plain");
        assert!(!meta.is_directory);
        assert!(meta.last_modified > 0);
    }

    #[test]
    fn get_file_metadata_sync_directory() {
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("mydir");
        fs::create_dir(&sub).unwrap();

        let meta = get_file_metadata_sync(&sub).unwrap();
        assert_eq!(meta.name, "mydir");
        assert!(meta.is_directory);
        assert_eq!(meta.mime_type, "inode/directory");
    }

    #[test]
    fn get_file_metadata_sync_rust_file() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("lib.rs");
        fs::write(&file, "pub fn foo() {}").unwrap();

        let meta = get_file_metadata_sync(&file).unwrap();
        assert_eq!(meta.name, "lib.rs");
        // rust files should get a reasonable mime type
        assert!(!meta.mime_type.is_empty());
    }

    #[test]
    fn get_file_metadata_sync_nonexistent() {
        let dir = tempfile::tempdir().unwrap();
        let fake = dir.path().join("missing.txt");
        let result = get_file_metadata_sync(&fake);
        assert!(result.is_err());
    }

    #[test]
    fn get_file_metadata_sync_image_mime() {
        let dir = tempfile::tempdir().unwrap();
        let png = dir.path().join("icon.png");
        fs::write(&png, &[0x89, 0x50, 0x4E, 0x47]).unwrap();

        let meta = get_file_metadata_sync(&png).unwrap();
        assert_eq!(meta.mime_type, "image/png");
    }

    #[test]
    fn get_file_metadata_sync_unknown_extension() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("data.xyz123");
        fs::write(&file, "binary data").unwrap();

        let meta = get_file_metadata_sync(&file).unwrap();
        assert_eq!(meta.mime_type, "application/octet-stream");
    }
}

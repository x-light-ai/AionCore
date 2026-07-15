// FORK-CUSTOM: resolve the XAIWork OpenAPI host from an external JSON file,
// falling back to the value compiled into the binary.
//
// 此文件为 XAIWork fork 新增文件，不存在于上游仓库，rebase 时无冲突风险。

use std::path::{Path, PathBuf};

use serde::Deserialize;
use tracing::{info, warn};

pub const DEFAULT_XAIWORK_BASE_URL: &str = "http://localhost:5330";

/// External config file looked up next to the executable and under `data_dir`.
const XAIWORK_HOST_FILE: &str = "xaiwork_host.json";

/// Shape of `xaiwork_host.json`, e.g. `{"baseUrl":"https://api.example.com"}`.
#[derive(Debug, Deserialize)]
struct XaiworkHostConfig {
    #[serde(rename = "baseUrl")]
    base_url: String,
}

/// Resolve the XAIWork OpenAPI base URL.
///
/// Lookup order:
/// 1. `xaiwork_host.json` next to the executable (deployment override).
/// 2. `xaiwork_host.json` under `data_dir` (per-instance override).
/// 3. `DEFAULT_XAIWORK_BASE_URL` compiled into the binary.
///
/// A missing file is normal and silently falls through to the next candidate.
/// A present but malformed file is logged at `warn` and treated as absent.
pub fn resolve_xaiwork_base_url(data_dir: &Path) -> String {
    for path in candidate_paths(data_dir) {
        if !path.exists() {
            continue;
        }
        if let Some(base_url) = load_from_file(&path) {
            info!(
                path = %path.display(),
                base_url = %base_url,
                "xaiwork host: loaded from external file"
            );
            return base_url;
        }
    }
    DEFAULT_XAIWORK_BASE_URL.to_string()
}

/// Candidate file locations in priority order.
fn candidate_paths(data_dir: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        paths.push(dir.join(XAIWORK_HOST_FILE));
    }
    paths.push(data_dir.join(XAIWORK_HOST_FILE));
    paths
}

/// Read + parse one candidate file. Returns `None` (with a warning) on any
/// read/parse error or when `baseUrl` is empty.
fn load_from_file(path: &Path) -> Option<String> {
    let raw = match std::fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(e) => {
            warn!(path = %path.display(), error = %e, "xaiwork host: failed to read external file, ignoring");
            return None;
        }
    };
    match serde_json::from_str::<XaiworkHostConfig>(&raw) {
        Ok(cfg) => {
            let base_url = cfg.base_url.trim().to_string();
            if base_url.is_empty() {
                warn!(path = %path.display(), "xaiwork host: baseUrl is empty, ignoring");
                return None;
            }
            Some(base_url)
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "xaiwork host: failed to parse external file, ignoring");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn falls_back_to_default_when_absent() {
        let dir = tempdir().unwrap();
        assert_eq!(resolve_xaiwork_base_url(dir.path()), DEFAULT_XAIWORK_BASE_URL);
    }

    #[test]
    fn reads_base_url_from_data_dir_file() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path().join(XAIWORK_HOST_FILE),
            r#"{"baseUrl":"https://api.example.com"}"#,
        )
        .unwrap();
        assert_eq!(resolve_xaiwork_base_url(dir.path()), "https://api.example.com");
    }

    #[test]
    fn falls_back_on_malformed_file() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(XAIWORK_HOST_FILE), "not json").unwrap();
        assert_eq!(resolve_xaiwork_base_url(dir.path()), DEFAULT_XAIWORK_BASE_URL);
    }

    #[test]
    fn falls_back_on_empty_base_url() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join(XAIWORK_HOST_FILE), r#"{"baseUrl":"  "}"#).unwrap();
        assert_eq!(resolve_xaiwork_base_url(dir.path()), DEFAULT_XAIWORK_BASE_URL);
    }
}

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::error::GremlinError;

pub fn now_rfc3339() -> String {
    Utc::now().to_rfc3339()
}

pub fn new_id(prefix: &str) -> String {
    format!("{prefix}_{}", Uuid::new_v4().simple())
}

pub fn system_time_rfc3339(value: SystemTime) -> String {
    let dt: DateTime<Utc> = value.into();
    dt.to_rfc3339()
}

pub fn lossy(path: &Path) -> String {
    // TODO: v0 stores display paths as UTF-8 lossy strings. Preserve raw bytes on Unix later.
    path.to_string_lossy().to_string()
}

pub fn absolute_path(path: &Path) -> std::io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

pub fn relative_path(root: &Path, path: &Path) -> Result<String, GremlinError> {
    let rel = path
        .strip_prefix(root)
        .map_err(|_| GremlinError::PathOutsideRoot {
            path: lossy(path),
            root: lossy(root),
        })?;
    if rel.as_os_str().is_empty() {
        return Ok(".".to_string());
    }
    Ok(lossy(rel))
}

pub fn basename(path: &Path) -> Result<String, GremlinError> {
    path.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .ok_or_else(|| GremlinError::MissingFileName(lossy(path)))
}

pub fn parent_path(relative_path: &str) -> String {
    Path::new(relative_path)
        .parent()
        .map(lossy)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| ".".to_string())
}

pub fn local_machine_id() -> String {
    let host = std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "unknown-host".to_string());
    let platform = std::env::consts::OS;
    let digest = blake3::hash(format!("{host}:{platform}").as_bytes());
    format!("machine_{}", &digest.to_hex()[..16])
}

pub fn local_hostname() -> Option<String> {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn calculates_relative_path() {
        let root = Path::new("/tmp/root");
        let file = Path::new("/tmp/root/a/b.txt");
        assert_eq!(relative_path(root, file).unwrap(), "a/b.txt");
        assert_eq!(parent_path("a/b.txt"), "a");
    }
}

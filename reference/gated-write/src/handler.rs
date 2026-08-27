//! File-write executor for the confirmation-gated `gated-write` plugin.
//!
//! The dangerous half of the split: this code only ever runs after an
//! allowlisted caller confirms a pending `request_write`. Paths are kept
//! inside the configured data dir (no absolute paths, no `..`, no symlink
//! escape).

use std::fs;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// Upper bound for one confirmed write, so a single `confirm_write` stays
/// bounded (params arrive inside an `ActionRequest`).
pub const MAX_CONTENT_BYTES: usize = 1024 * 1024;

/// Validated params for one confirmed write.
pub struct WriteParams {
    /// Path relative to the data dir.
    pub path: String,
    pub content: String,
    /// `true` appends, `false` truncates and overwrites.
    pub append: bool,
}

/// Parse and validate the `request_write` params JSON.
pub fn parse_write_params(params_json: &[u8]) -> Result<WriteParams, String> {
    let value: serde_json::Value =
        serde_json::from_slice(params_json).map_err(|e| format!("invalid params: {e}"))?;
    let path = value
        .get("path")
        .and_then(|p| p.as_str())
        .ok_or_else(|| "params must include a string path".to_string())?;
    if path.is_empty() {
        return Err("path must not be empty".into());
    }
    if path.len() > 4096 {
        return Err("path is too long".into());
    }
    let content = value
        .get("content")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_string();
    if content.len() > MAX_CONTENT_BYTES {
        return Err(format!("content exceeds {MAX_CONTENT_BYTES} bytes"));
    }
    let append = match value.get("mode").and_then(|m| m.as_str()) {
        Some("append") => true,
        Some("overwrite") | None => false,
        Some(other) => {
            return Err(format!(
                "mode must be \"append\" or \"overwrite\", got {other:?}"
            ))
        }
    };
    Ok(WriteParams {
        path: path.to_string(),
        content,
        append,
    })
}

/// Reject paths that could escape `data_dir`: absolute paths and `..`
/// components. The write target is always `<data_dir>/<path>`.
pub fn safe_target(data_dir: &Path, rel_path: &str) -> Result<PathBuf, String> {
    let rel = Path::new(rel_path);
    if rel.is_absolute() {
        return Err("path must be relative".into());
    }
    for component in rel.components() {
        match component {
            Component::ParentDir => return Err("path must not contain '..'".into()),
            Component::RootDir | Component::Prefix(_) => return Err("path must be relative".into()),
            _ => {}
        }
    }
    Ok(data_dir.join(rel))
}

/// Execute one confirmed write under `data_dir`, creating parent dirs.
/// Returns the absolute written path and the byte count. Refuses targets
/// whose canonical parent escapes `data_dir` (symlink-escape defense) and
/// targets that are themselves symlinks.
pub fn execute_write(data_dir: &Path, params: &WriteParams) -> Result<(PathBuf, u64), String> {
    let target = safe_target(data_dir, &params.path)?;

    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("failed to create {}: {e}", parent.display()))?;
    }
    let canonical_data = data_dir
        .canonicalize()
        .map_err(|e| format!("failed to resolve {}: {e}", data_dir.display()))?;
    let canonical_parent = target
        .parent()
        .unwrap()
        .canonicalize()
        .map_err(|e| format!("failed to resolve target parent: {e}"))?;
    if !canonical_parent.starts_with(&canonical_data) {
        return Err("path resolves outside the data dir".into());
    }
    if fs::symlink_metadata(&target)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
    {
        return Err(format!(
            "refusing to write through symlink: {}",
            target.display()
        ));
    }

    let bytes = if params.append {
        let mut f = fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&target)
            .map_err(|e| format!("failed to open {}: {e}", target.display()))?;
        f.write_all(params.content.as_bytes())
            .map_err(|e| format!("failed to write: {e}"))?;
        f.flush().map_err(|e| format!("failed to flush: {e}"))?;
        params.content.len()
    } else {
        fs::write(&target, params.content.as_bytes())
            .map_err(|e| format!("failed to write {}: {e}", target.display()))?;
        params.content.len()
    };
    Ok((target, bytes as u64))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_data_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "gated-write-handler-{}-{name}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn safe_target_rejects_escape_paths() {
        let dir = Path::new("/data");
        assert_eq!(
            safe_target(dir, "../etc/passwd").unwrap_err(),
            "path must not contain '..'"
        );
        assert_eq!(
            safe_target(dir, "a/../../b").unwrap_err(),
            "path must not contain '..'"
        );
        assert_eq!(
            safe_target(dir, "/etc/passwd").unwrap_err(),
            "path must be relative"
        );
        assert!(safe_target(dir, "notes/hello.txt").is_ok());
    }

    #[test]
    fn parse_rejects_bad_params() {
        assert!(parse_write_params(b"not json").is_err());
        assert!(
            parse_write_params(br#"{"content": "x"}"#).is_err(),
            "missing path"
        );
        assert!(parse_write_params(br#"{"path": "", "content": "x"}"#).is_err());
        assert!(parse_write_params(br#"{"path": "a", "mode": "truncate"}"#).is_err());
        let huge = serde_json::json!({"path": "a", "content": "x".repeat(MAX_CONTENT_BYTES + 1)});
        assert!(parse_write_params(huge.to_string().as_bytes()).is_err());
        let ok =
            parse_write_params(br#"{"path": "a", "content": "hi", "mode": "append"}"#).unwrap();
        assert!(ok.append);
        let ok = parse_write_params(br#"{"path": "a"}"#).unwrap();
        assert!(!ok.append, "missing mode defaults to overwrite");
    }

    #[test]
    fn execute_writes_under_data_dir() {
        let dir = temp_data_dir("writes");
        let (target, bytes) = execute_write(
            &dir,
            &WriteParams {
                path: "sub/notes.txt".into(),
                content: "hello".into(),
                append: false,
            },
        )
        .unwrap();
        assert_eq!(bytes, 5);
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello");
        assert!(target.starts_with(&dir));

        // append adds, overwrite replaces
        execute_write(
            &dir,
            &WriteParams {
                path: "sub/notes.txt".into(),
                content: " world".into(),
                append: true,
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "hello world");
        execute_write(
            &dir,
            &WriteParams {
                path: "sub/notes.txt".into(),
                content: "fresh".into(),
                append: false,
            },
        )
        .unwrap();
        assert_eq!(fs::read_to_string(&target).unwrap(), "fresh");

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn symlink_target_is_refused() {
        let dir = temp_data_dir("symlink");
        let outside = temp_data_dir("outside");
        let outside_file = outside.join("victim.txt");
        fs::write(&outside_file, "keep").unwrap();
        // A data-dir symlink pointing outside — writing through it would
        // escape the sandbox.
        std::os::unix::fs::symlink(&outside_file, dir.join("link")).unwrap();

        let err = execute_write(
            &dir,
            &WriteParams {
                path: "link".into(),
                content: "pwned".into(),
                append: false,
            },
        )
        .unwrap_err();
        assert!(err.contains("symlink"), "error was: {err}");
        assert_eq!(fs::read_to_string(&outside_file).unwrap(), "keep");

        let _ = fs::remove_dir_all(&dir);
        let _ = fs::remove_dir_all(&outside);
    }
}

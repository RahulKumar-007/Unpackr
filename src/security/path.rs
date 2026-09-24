use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum PathSecurityError {
    #[error("Path attempts directory traversal (Zip Slip): {0}")]
    DirectoryTraversal(String),
    #[error("Path is absolute, which is forbidden in archives: {0}")]
    AbsolutePath(String),
    #[error("Path contains invalid characters or null bytes: {0}")]
    InvalidCharacters(String),
    #[error("Path resolves outside the target destination directory: {0}")]
    OutsideDestination(String),
    #[error("Empty entry path")]
    EmptyPath,
}

/// Validates and sanitizes a raw path extracted from a ZIP entry.
///
/// Prevents Zip Slip attacks (e.g. `../../etc/passwd` or `C:\Windows\...`)
/// and ensures that extracting this path will strictly remain within `dest_dir`.
pub fn sanitize_entry_path(raw_path: &str) -> Result<PathBuf, PathSecurityError> {
    if raw_path.is_empty() {
        return Err(PathSecurityError::EmptyPath);
    }

    if raw_path.contains('\0') {
        return Err(PathSecurityError::InvalidCharacters(raw_path.to_string()));
    }

    // Normalize forward slashes and backslashes
    let normalized = raw_path.replace('\\', "/");
    let path = Path::new(&normalized);

    // Reject absolute paths
    if path.is_absolute() || normalized.starts_with('/') {
        return Err(PathSecurityError::AbsolutePath(raw_path.to_string()));
    }

    // Check Windows-style drive letters (e.g. "C:file")
    if let Some(first_char) = normalized.chars().next() {
        if first_char.is_ascii_alphabetic() && normalized.chars().nth(1) == Some(':') {
            return Err(PathSecurityError::AbsolutePath(raw_path.to_string()));
        }
    }

    let mut clean_buf = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => {
                clean_buf.push(seg);
            }
            Component::CurDir => {
                // Ignore '.'
            }
            Component::ParentDir => {
                // Reject '..'
                return Err(PathSecurityError::DirectoryTraversal(raw_path.to_string()));
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathSecurityError::AbsolutePath(raw_path.to_string()));
            }
        }
    }

    if clean_buf.as_os_str().is_empty() {
        return Err(PathSecurityError::EmptyPath);
    }

    Ok(clean_buf)
}

/// Resolves a sanitized entry path inside the destination root directory.
/// Ensures the resolved target path is lexically within `dest_root`.
pub fn resolve_safe_dest(dest_root: &Path, sanitized_path: &Path) -> Result<PathBuf, PathSecurityError> {
    let resolved = dest_root.join(sanitized_path);
    // Double check that the resolved path starts with dest_root
    if !resolved.starts_with(dest_root) {
        return Err(PathSecurityError::OutsideDestination(
            sanitized_path.to_string_lossy().to_string(),
        ));
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_paths() {
        assert_eq!(
            sanitize_entry_path("foo/bar/baz.txt").unwrap(),
            PathBuf::from("foo/bar/baz.txt")
        );
        assert_eq!(
            sanitize_entry_path("./foo/bar.txt").unwrap(),
            PathBuf::from("foo/bar.txt")
        );
        assert_eq!(
            sanitize_entry_path("foo\\bar\\baz.txt").unwrap(),
            PathBuf::from("foo/bar/baz.txt")
        );
    }

    #[test]
    fn test_zip_slip_rejection() {
        assert!(matches!(
            sanitize_entry_path("../evil.txt"),
            Err(PathSecurityError::DirectoryTraversal(_))
        ));
        assert!(matches!(
            sanitize_entry_path("foo/../../evil.txt"),
            Err(PathSecurityError::DirectoryTraversal(_))
        ));
        assert!(matches!(
            sanitize_entry_path("foo/..\\..\\evil.txt"),
            Err(PathSecurityError::DirectoryTraversal(_))
        ));
    }

    #[test]
    fn test_absolute_path_rejection() {
        assert!(matches!(
            sanitize_entry_path("/etc/passwd"),
            Err(PathSecurityError::AbsolutePath(_))
        ));
        assert!(matches!(
            sanitize_entry_path("C:/Windows/System32"),
            Err(PathSecurityError::AbsolutePath(_))
        ));
    }

    #[test]
    fn test_null_byte_rejection() {
        assert!(matches!(
            sanitize_entry_path("foo\0bar.txt"),
            Err(PathSecurityError::InvalidCharacters(_))
        ));
    }
}

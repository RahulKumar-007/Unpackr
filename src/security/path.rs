use std::path::{Component, Path, PathBuf};
use thiserror::Error;

#[derive(Error, Debug, PartialEq, Eq)]
pub enum PathSecurityError {
    #[error("Path attempts directory traversal (Zip Slip): {0}")]
    DirectoryTraversal(String),
    #[error("Path is absolute, which is forbidden in archives: {0}")]
    AbsolutePath(String),
    #[error("Path contains invalid characters, colons, or null bytes: {0}")]
    InvalidCharacters(String),
    #[error("Path contains reserved Windows device name: {0}")]
    ReservedDeviceName(String),
    #[error("Path attempts to access reserved internal directory: {0}")]
    ReservedPath(String),
    #[error("Path resolves outside the target destination directory: {0}")]
    OutsideDestination(String),
    #[error("Path traverses through an existing symbolic link on disk: {0}")]
    SymlinkTraversal(String),
    #[error("Symbolic link target escapes the destination directory: {0}")]
    SymlinkTargetOutside(String),
    #[error("Empty entry path")]
    EmptyPath,
}

/// Checks if a path component is a reserved Windows device name (e.g. CON, PRN, AUX, NUL, COM1-9, LPT1-9).
pub fn is_windows_reserved_device_name(segment: &str) -> bool {
    let base = match segment.split_once('.') {
        Some((stem, _)) => stem,
        None => segment,
    };
    let upper = base.to_ascii_uppercase();
    matches!(
        upper.as_str(),
        "CON"
            | "PRN"
            | "AUX"
            | "NUL"
            | "CONIN$"
            | "CONOUT$"
            | "COM0"
            | "COM1"
            | "COM2"
            | "COM3"
            | "COM4"
            | "COM5"
            | "COM6"
            | "COM7"
            | "COM8"
            | "COM9"
            | "LPT0"
            | "LPT1"
            | "LPT2"
            | "LPT3"
            | "LPT4"
            | "LPT5"
            | "LPT6"
            | "LPT7"
            | "LPT8"
            | "LPT9"
    )
}

/// Validates and sanitizes a raw path extracted from a ZIP entry.
///
/// Prevents Zip Slip attacks (e.g. `../../etc/passwd` or `C:\Windows\...`),
/// reserved device names, internal state collisions (`.unpackr`),
/// Alternate Data Streams (colons), and ensures lexical isolation.
pub fn sanitize_entry_path(raw_path: &str) -> Result<PathBuf, PathSecurityError> {
    if raw_path.is_empty() {
        return Err(PathSecurityError::EmptyPath);
    }

    if raw_path.contains('\0') || raw_path.contains('\u{FFFD}') {
        return Err(PathSecurityError::InvalidCharacters(raw_path.to_string()));
    }

    // Check for colons (Alternate Data Streams or Windows device access e.g. "foo:stream")
    if raw_path.contains(':') {
        return Err(PathSecurityError::InvalidCharacters(raw_path.to_string()));
    }

    // Normalize forward slashes and backslashes
    let normalized = raw_path.replace('\\', "/");
    let path = Path::new(&normalized);

    // Reject absolute paths
    if path.is_absolute() || normalized.starts_with('/') {
        return Err(PathSecurityError::AbsolutePath(raw_path.to_string()));
    }

    let mut clean_buf = PathBuf::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => {
                let seg_str = seg.to_string_lossy();

                // Prevent access to internal state directory
                if seg_str == ".unpackr" {
                    return Err(PathSecurityError::ReservedPath(raw_path.to_string()));
                }

                // Prevent Windows reserved device names
                if is_windows_reserved_device_name(&seg_str) {
                    return Err(PathSecurityError::ReservedDeviceName(raw_path.to_string()));
                }

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
pub fn resolve_safe_dest(
    dest_root: &Path,
    sanitized_path: &Path,
) -> Result<PathBuf, PathSecurityError> {
    let resolved = dest_root.join(sanitized_path);
    // Double check that the resolved path starts with dest_root
    if !resolved.starts_with(dest_root) {
        return Err(PathSecurityError::OutsideDestination(
            sanitized_path.to_string_lossy().to_string(),
        ));
    }
    Ok(resolved)
}

/// Verifies that no ancestor directory under `dest_root` along `sanitized_path`
/// is an existing symbolic link on the filesystem.
///
/// This prevents Symlink Poisoning / Traversal attacks where an attacker pre-creates
/// a symlink (e.g. `dest/link -> /etc`) to fool subsequent file writes into escaping
/// the extraction directory.
pub fn check_symlink_traversal(
    dest_root: &Path,
    sanitized_path: &Path,
) -> Result<(), PathSecurityError> {
    let mut current = dest_root.to_path_buf();
    let components: Vec<_> = sanitized_path.components().collect();

    for comp in components {
        if let Component::Normal(seg) = comp {
            current.push(seg);
            if let Ok(meta) = std::fs::symlink_metadata(&current) {
                if meta.file_type().is_symlink() {
                    return Err(PathSecurityError::SymlinkTraversal(
                        current.to_string_lossy().to_string(),
                    ));
                }
            }
        }
    }

    Ok(())
}

/// Validates that a symbolic link target path (if symlinks are encountered)
/// stays strictly within `dest_root` and does not escape via parent traversal or absolute paths.
pub fn validate_symlink_target(
    dest_root: &Path,
    symlink_dir: &Path,
    target_str: &str,
) -> Result<PathBuf, PathSecurityError> {
    if target_str.is_empty() {
        return Err(PathSecurityError::EmptyPath);
    }

    if target_str.contains('\0') || target_str.contains(':') {
        return Err(PathSecurityError::InvalidCharacters(target_str.to_string()));
    }

    let normalized = target_str.replace('\\', "/");
    let target_path = Path::new(&normalized);

    if target_path.is_absolute() || normalized.starts_with('/') {
        return Err(PathSecurityError::AbsolutePath(target_str.to_string()));
    }

    // Compute resolved target path relative to the symlink's directory
    let mut current = symlink_dir.to_path_buf();
    for comp in target_path.components() {
        match comp {
            Component::Normal(seg) => {
                current.push(seg);
            }
            Component::CurDir => {}
            Component::ParentDir => {
                if !current.pop() || !current.starts_with(dest_root) {
                    return Err(PathSecurityError::SymlinkTargetOutside(
                        target_str.to_string(),
                    ));
                }
            }
            Component::RootDir | Component::Prefix(_) => {
                return Err(PathSecurityError::AbsolutePath(target_str.to_string()));
            }
        }
    }

    if !current.starts_with(dest_root) {
        return Err(PathSecurityError::SymlinkTargetOutside(
            target_str.to_string(),
        ));
    }

    Ok(current)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

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
            Err(PathSecurityError::InvalidCharacters(_))
        ));
    }

    #[test]
    fn test_null_byte_rejection() {
        assert!(matches!(
            sanitize_entry_path("foo\0bar.txt"),
            Err(PathSecurityError::InvalidCharacters(_))
        ));
    }

    #[test]
    fn test_reserved_unpackr_directory_rejection() {
        assert!(matches!(
            sanitize_entry_path(".unpackr/manifest.json"),
            Err(PathSecurityError::ReservedPath(_))
        ));
        assert!(matches!(
            sanitize_entry_path("subdir/.unpackr/evil.sh"),
            Err(PathSecurityError::ReservedPath(_))
        ));
    }

    #[test]
    fn test_windows_reserved_device_names() {
        assert!(matches!(
            sanitize_entry_path("aux.txt"),
            Err(PathSecurityError::ReservedDeviceName(_))
        ));
        assert!(matches!(
            sanitize_entry_path("CON"),
            Err(PathSecurityError::ReservedDeviceName(_))
        ));
        assert!(matches!(
            sanitize_entry_path("dir/NUL.dat"),
            Err(PathSecurityError::ReservedDeviceName(_))
        ));
        assert!(matches!(
            sanitize_entry_path("lpt1"),
            Err(PathSecurityError::ReservedDeviceName(_))
        ));
        assert!(matches!(
            sanitize_entry_path("COM0"),
            Err(PathSecurityError::ReservedDeviceName(_))
        ));
        assert!(matches!(
            sanitize_entry_path("dir/LPT0.txt"),
            Err(PathSecurityError::ReservedDeviceName(_))
        ));
        assert!(matches!(
            sanitize_entry_path("CONIN$"),
            Err(PathSecurityError::ReservedDeviceName(_))
        ));
        assert!(matches!(
            sanitize_entry_path("CONOUT$"),
            Err(PathSecurityError::ReservedDeviceName(_))
        ));
    }

    #[test]
    fn test_alternate_data_stream_colon_rejection() {
        assert!(matches!(
            sanitize_entry_path("file.txt:hidden"),
            Err(PathSecurityError::InvalidCharacters(_))
        ));
    }

    #[test]
    fn test_symlink_traversal_detection() {
        let temp = tempdir().unwrap();
        let dest = temp.path().join("dest");
        std::fs::create_dir(&dest).unwrap();

        let outside = temp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        // Create a symlink: dest/evil_link -> outside
        let symlink_path = dest.join("evil_link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &symlink_path).unwrap();

        #[cfg(unix)]
        {
            let entry_path = PathBuf::from("evil_link/secret.txt");
            let res = check_symlink_traversal(&dest, &entry_path);
            assert!(matches!(res, Err(PathSecurityError::SymlinkTraversal(_))));
        }
    }

    #[test]
    fn test_symlink_target_validation() {
        let temp = tempfile::tempdir().unwrap();
        let dest = temp.path();
        let link_dir = dest.join("subdir");

        // Safe relative link
        assert!(validate_symlink_target(dest, &link_dir, "foo.txt").is_ok());
        assert!(validate_symlink_target(dest, &link_dir, "../other.txt").is_ok());

        // Escapes dest via traversal
        assert!(matches!(
            validate_symlink_target(dest, &link_dir, "../../escaped.txt"),
            Err(PathSecurityError::SymlinkTargetOutside(_))
        ));

        // Absolute link target
        assert!(matches!(
            validate_symlink_target(dest, &link_dir, "/etc/passwd"),
            Err(PathSecurityError::AbsolutePath(_))
        ));
    }
}

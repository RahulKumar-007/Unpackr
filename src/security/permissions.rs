use std::path::Path;

/// Checks if an entry's Unix mode in external attributes attempts to create
/// a special device node (character/block device, named pipe/FIFO, or socket).
///
/// Unpackr strictly refuses to create device nodes or sockets from archives.
pub fn check_forbidden_device_type(external_attributes: u32) -> Result<(), &'static str> {
    let mode = (external_attributes >> 16) & 0o170000;
    match mode {
        0o060000 => Err("Archive entry is a block device (S_IFBLK), which is forbidden"),
        0o020000 => Err("Archive entry is a character device (S_IFCHR), which is forbidden"),
        0o010000 => Err("Archive entry is a named pipe/FIFO (S_IFIFO), which is forbidden"),
        0o140000 => Err("Archive entry is a socket (S_IFSOCK), which is forbidden"),
        _ => Ok(()),
    }
}

/// Sanitizes Unix file mode permissions extracted from external attributes.
///
/// Security rules:
/// 1. SUID (0o4000) and SGID (0o2000) bits are ALWAYS stripped to prevent privilege escalation.
/// 2. Sticky bit (0o1000) is stripped.
/// 3. World-writable bit (0o002) is stripped to avoid insecure public write access.
/// 4. Owner read and write permissions (0o600) are always granted.
/// 5. For directories, execute bits (0o755) are ensured so directories remain traversable.
pub fn sanitize_unix_mode(external_attributes: u32, is_dir: bool) -> u32 {
    let raw_mode = (external_attributes >> 16) & 0o7777;

    if is_dir {
        // Safe directory default: 0o755
        let base = if raw_mode == 0 { 0o755 } else { raw_mode };
        // Strip SUID/SGID/Sticky and world-writable, ensure owner rwx (0o700)
        (base & !0o7002) | 0o700
    } else {
        // Safe file default: 0o644 (or 0o755 if executable)
        let base = if raw_mode == 0 {
            0o644
        } else {
            raw_mode
        };
        // Strip SUID/SGID/Sticky and world-writable, ensure owner rw (0o600)
        (base & !0o7002) | 0o600
    }
}

/// Safely applies sanitized Unix permissions to an extracted file or directory.
#[cfg(unix)]
pub fn apply_safe_permissions(path: &Path, external_attributes: u32, is_dir: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let safe_mode = sanitize_unix_mode(external_attributes, is_dir);
    let perms = std::fs::Permissions::from_mode(safe_mode);
    std::fs::set_permissions(path, perms)
}

#[cfg(not(unix))]
pub fn apply_safe_permissions(_path: &Path, _external_attributes: u32, _is_dir: bool) -> std::io::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_forbidden_device_rejection() {
        assert!(check_forbidden_device_type(0o060666 << 16).is_err()); // Block device
        assert!(check_forbidden_device_type(0o020666 << 16).is_err()); // Char device
        assert!(check_forbidden_device_type(0o010666 << 16).is_err()); // FIFO
        assert!(check_forbidden_device_type(0o140666 << 16).is_err()); // Socket

        assert!(check_forbidden_device_type(0o100644 << 16).is_ok()); // Regular file
        assert!(check_forbidden_device_type(0o040755 << 16).is_ok()); // Directory
        assert!(check_forbidden_device_type(0o120777 << 16).is_ok()); // Symlink
    }

    #[test]
    fn test_suid_sgid_stripped() {
        // Attempt setuid + setgid root file: 0o6755
        let mode = sanitize_unix_mode(0o6755 << 16, false);
        assert_eq!(mode & 0o4000, 0, "SUID bit must be stripped");
        assert_eq!(mode & 0o2000, 0, "SGID bit must be stripped");
        assert_eq!(mode & 0o1000, 0, "Sticky bit must be stripped");
        assert_eq!(mode & 0o0002, 0, "World writable bit must be stripped");
        assert_eq!(mode, 0o755);
    }

    #[test]
    fn test_world_writable_stripped() {
        // Attempt 0o777
        let mode = sanitize_unix_mode(0o777 << 16, false);
        assert_eq!(mode, 0o775); // 0o777 & !0o002
    }

    #[test]
    fn test_default_permissions() {
        assert_eq!(sanitize_unix_mode(0, false), 0o644);
        assert_eq!(sanitize_unix_mode(0, true), 0o755);
    }
}

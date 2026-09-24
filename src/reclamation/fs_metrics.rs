use std::path::Path;
use std::io::Result;

/// Returns the actual physical disk space allocated to a file in bytes,
/// querying filesystem allocation blocks (`st_blocks * 512`) to accurately
/// reflect sparse holes.
#[cfg(unix)]
pub fn get_physical_allocated_bytes(path: &Path) -> Result<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidInput, e))?;

    let mut stat_buf: libc::stat = unsafe { std::mem::zeroed() };
    let ret = unsafe { libc::stat(c_path.as_ptr(), &mut stat_buf) };

    if ret != 0 {
        return Err(std::io::Error::last_os_error());
    }

    // On Linux/POSIX systems, st_blocks is explicitly defined in units of 512-byte blocks.
    Ok((stat_buf.st_blocks as u64) * 512)
}

#[cfg(not(unix))]
pub fn get_physical_allocated_bytes(path: &Path) -> Result<u64> {
    let metadata = std::fs::metadata(path)?;
    Ok(metadata.len())
}

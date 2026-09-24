use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::time::SystemTime;
use anyhow::{Context, Result};
use blake3::Hasher;

/// Computes a fast, collision-resistant identity hash for a ZIP archive without
/// hashing the entire multi-gigabyte payload.
///
/// Hashing combines:
/// 1. Logical file size (8 bytes)
/// 2. Filesystem modification time (seconds + nanos)
/// 3. First 64 KB of the file (covers file signature and first local file header)
/// 4. Tail of the file (up to 128 KB, covering EOCD and parts of Central Directory)
pub fn compute_archive_identity(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("Failed to open archive: {:?}", path))?;
    let metadata = file.metadata().with_context(|| "Failed to read archive metadata")?;
    let file_size = metadata.len();

    let mut hasher = Hasher::new();

    // 1. File size
    hasher.update(&file_size.to_le_bytes());

    // 2. Modified time
    if let Ok(mtime) = metadata.modified() {
        if let Ok(duration) = mtime.duration_since(SystemTime::UNIX_EPOCH) {
            hasher.update(&duration.as_secs().to_le_bytes());
            hasher.update(&duration.subsec_nanos().to_le_bytes());
        }
    }

    // 3. First 64 KB
    let head_chunk_size = (file_size.min(65536)) as usize;
    let mut head_buf = vec![0u8; head_chunk_size];
    file.seek(SeekFrom::Start(0))?;
    file.read_exact(&mut head_buf)?;
    hasher.update(&head_buf);

    // 4. Tail up to 128 KB (or remaining bytes)
    if file_size > head_chunk_size as u64 {
        let tail_chunk_size = (file_size.saturating_sub(head_chunk_size as u64).min(131072)) as usize;
        let mut tail_buf = vec![0u8; tail_chunk_size];
        let tail_start = file_size - tail_chunk_size as u64;
        file.seek(SeekFrom::Start(tail_start))?;
        file.read_exact(&mut tail_buf)?;
        hasher.update(&tail_buf);
    }

    let hash_bytes = hasher.finalize();
    Ok(hash_bytes.to_hex().to_string())
}

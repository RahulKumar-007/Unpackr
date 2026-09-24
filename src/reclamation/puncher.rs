use std::fs::File;
use crate::archive::ZipEntryMetadata;
use crate::reclamation::error::ReclamationError;

pub const DEFAULT_BLOCK_SIZE: u64 = 4096;

/// Calculates the safe, inward-aligned block range for hole punching.
/// 
/// Given an entry's compressed data interval [D_start, D_end), the range is rounded:
/// R_start = ceil(D_start / block_size) * block_size
/// R_end   = floor(D_end / block_size) * block_size
/// 
/// If R_end > R_start, returns Some((R_start, R_end - R_start)).
/// Otherwise returns None (no whole blocks to punch).
pub fn compute_inward_reclaim_range(
    data_offset: u64,
    compressed_size: u64,
    block_size: u64,
) -> Option<(u64, u64)> {
    if compressed_size == 0 || block_size == 0 {
        return None;
    }

    let d_start = data_offset;
    let d_end = match data_offset.checked_add(compressed_size) {
        Some(val) => val,
        None => return None,
    };

    // Inward ceiling: (d_start + block_size - 1) / block_size * block_size
    let r_start = match d_start.checked_add(block_size - 1) {
        Some(val) => (val / block_size) * block_size,
        None => return None,
    };

    // Inward floor: (d_end / block_size) * block_size
    let r_end = (d_end / block_size) * block_size;

    if r_end > r_start {
        Some((r_start, r_end - r_start))
    } else {
        None
    }
}

pub struct ArchiveHolePuncher {
    file: File,
    archive_size: u64,
    central_directory_offset: u64,
    block_size: u64,
    total_reclaimed_bytes: u64,
}

impl ArchiveHolePuncher {
    pub fn new(file: File, archive_size: u64, central_directory_offset: u64) -> Self {
        Self {
            file,
            archive_size,
            central_directory_offset,
            block_size: DEFAULT_BLOCK_SIZE,
            total_reclaimed_bytes: 0,
        }
    }

    /// Punches an inward block-aligned hole in the archive for the given entry's compressed data.
    /// Returns the number of bytes reclaimed (0 if entry fits within a sub-block boundary).
    pub fn punch_entry(&mut self, entry: &ZipEntryMetadata) -> Result<u64, ReclamationError> {
        if entry.is_dir || entry.compressed_size == 0 {
            return Ok(0);
        }

        let range = match compute_inward_reclaim_range(
            entry.data_offset,
            entry.compressed_size,
            self.block_size,
        ) {
            Some(r) => r,
            None => return Ok(0),
        };

        let (offset, length) = range;

        // Safety Barrier 1: Must never punch before entry's data offset
        if offset < entry.data_offset {
            return Err(ReclamationError::SafetyBoundaryViolation {
                start: offset,
                end: offset + length,
            });
        }

        // Safety Barrier 2: Must never punch past entry's compressed data
        let entry_end = entry.data_offset + entry.compressed_size;
        if offset + length > entry_end {
            return Err(ReclamationError::SafetyBoundaryViolation {
                start: offset,
                end: offset + length,
            });
        }

        // Safety Barrier 3: Must never punch into Central Directory or EOCD
        if offset + length > self.central_directory_offset {
            return Err(ReclamationError::CentralDirectoryViolation {
                offset: offset + length,
            });
        }

        // Execute fallocate punch hole syscall on Linux
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::io::AsRawFd;
            let fd = self.file.as_raw_fd();
            let ret = unsafe {
                libc::fallocate(
                    fd,
                    libc::FALLOC_FL_PUNCH_HOLE | libc::FALLOC_FL_KEEP_SIZE,
                    offset as libc::off_t,
                    length as libc::off_t,
                )
            };

            if ret != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EOPNOTSUPP) {
                    return Err(ReclamationError::UnsupportedFilesystem);
                }
                return Err(ReclamationError::Io(err));
            }
        }

        #[cfg(not(target_os = "linux"))]
        {
            return Err(ReclamationError::UnsupportedPlatform);
        }

        self.total_reclaimed_bytes += length;
        Ok(length)
    }

    pub fn total_reclaimed_bytes(&self) -> u64 {
        self.total_reclaimed_bytes
    }

    pub fn archive_size(&self) -> u64 {
        self.archive_size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compute_inward_reclaim_range() {
        // Case 1: Sub-block entries (less than 4096 bytes) -> None
        assert_eq!(compute_inward_reclaim_range(0, 100, 4096), None);
        assert_eq!(compute_inward_reclaim_range(100, 3900, 4096), None);
        assert_eq!(compute_inward_reclaim_range(4000, 200, 4096), None); // crosses boundary but spans no full block

        // Case 2: Exactly block-aligned
        assert_eq!(
            compute_inward_reclaim_range(4096, 4096, 4096),
            Some((4096, 4096))
        );
        assert_eq!(
            compute_inward_reclaim_range(0, 8192, 4096),
            Some((0, 8192))
        );

        // Case 3: Inward alignment from unaligned edges
        // Data range: [100, 9000).
        // R_start = ceil(100/4096)*4096 = 4096
        // R_end   = floor(9000/4096)*4096 = 8192
        // Reclaimed: [4096, 8192), length 4096
        assert_eq!(
            compute_inward_reclaim_range(100, 8900, 4096),
            Some((4096, 4096))
        );

        // Data range: [5000, 15000).
        // R_start = ceil(5000/4096)*4096 = 8192
        // R_end   = floor(15000/4096)*4096 = 12288
        // Reclaimed: [8192, 12288), length 4096
        assert_eq!(
            compute_inward_reclaim_range(5000, 10000, 4096),
            Some((8192, 4096))
        );

        // Large 1 MB entry at offset 100
        // Data range: [100, 1048676).
        // R_start = 4096
        // R_end = floor(1048676/4096)*4096 = 1048576 (256 * 4096)
        // Length = 1044480 (255 * 4096)
        assert_eq!(
            compute_inward_reclaim_range(100, 1048576, 4096),
            Some((4096, 1044480))
        );
    }
}

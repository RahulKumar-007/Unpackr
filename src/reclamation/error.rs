use thiserror::Error;

#[derive(Error, Debug)]
pub enum ReclamationError {
    #[error("Filesystem does not support hole punching (FALLOC_FL_PUNCH_HOLE)")]
    UnsupportedFilesystem,

    #[error("Storage reclamation is not supported on this platform")]
    UnsupportedPlatform,

    #[error(
        "Safety violation: attempted to punch outside entry data boundary [0x{start:X}, 0x{end:X})"
    )]
    SafetyBoundaryViolation { start: u64, end: u64 },

    #[error("Safety violation: attempted to punch into Central Directory at offset 0x{offset:X}")]
    CentralDirectoryViolation { offset: u64 },

    #[error("I/O error during reclamation: {0}")]
    Io(#[from] std::io::Error),
}

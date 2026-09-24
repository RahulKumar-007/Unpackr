use std::path::PathBuf;
use thiserror::Error;
use crate::security::path::PathSecurityError;

#[derive(Error, Debug)]
pub enum ExtractionError {
    #[error("Security violation for entry '{entry}': {source}")]
    Security {
        entry: String,
        source: PathSecurityError,
    },

    #[error("CRC-32 checksum mismatch for entry '{entry}': expected 0x{expected:08X}, computed 0x{actual:08X}")]
    CrcMismatch {
        entry: String,
        expected: u32,
        actual: u32,
    },

    #[error("Uncompressed size mismatch for entry '{entry}': expected {expected} bytes, got {actual} bytes")]
    SizeMismatch {
        entry: String,
        expected: u64,
        actual: u64,
    },

    #[error("Suspicious compression ratio exceeded limit for entry '{entry}' (ratio: {ratio:.1}x, limit: {limit:.1}x)")]
    SuspiciousCompressionRatio {
        entry: String,
        ratio: f64,
        limit: f64,
    },

    #[error("Destination collision for entry '{entry}': target file '{path}' already exists")]
    DestinationCollision {
        entry: String,
        path: PathBuf,
    },

    #[error("Unsupported compression method: {0:?}")]
    UnsupportedCompression(crate::archive::CompressionMethod),

    #[error("I/O error during extraction of '{entry}': {source}")]
    Io {
        entry: String,
        #[source]
        source: std::io::Error,
    },

    #[error("Archive error: {0}")]
    Archive(String),
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompressionMethod {
    Stored,
    Deflated,
    Unsupported(u16),
}

impl CompressionMethod {
    pub fn from_u16(val: u16) -> Self {
        match val {
            0 => CompressionMethod::Stored,
            8 => CompressionMethod::Deflated,
            other => CompressionMethod::Unsupported(other),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            CompressionMethod::Stored => "Stored (no compression)",
            CompressionMethod::Deflated => "Deflate",
            CompressionMethod::Unsupported(_) => "Unsupported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryState {
    Pending,
    Extracting,
    Extracted,
    Verified,
    Reclaimed,
    Skipped,
    Failed(String),
}

impl EntryState {
    pub fn as_str(&self) -> &str {
        match self {
            EntryState::Pending => "PENDING",
            EntryState::Extracting => "EXTRACTING",
            EntryState::Extracted => "EXTRACTED",
            EntryState::Verified => "VERIFIED",
            EntryState::Reclaimed => "RECLAIMED",
            EntryState::Skipped => "SKIPPED",
            EntryState::Failed(_) => "FAILED",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZipEntryMetadata {
    pub index: usize,
    pub name: String,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub compression_method: CompressionMethod,
    pub crc32: u32,
    pub local_header_offset: u64,
    pub data_offset: u64,
    pub is_dir: bool,
    pub is_symlink: bool,
    pub external_attributes: u32,
    pub state: EntryState,
}

impl ZipEntryMetadata {
    /// Return the end byte offset of the compressed data payload within the archive
    pub fn data_end_offset(&self) -> u64 {
        self.data_offset.saturating_add(self.compressed_size)
    }

    /// Return the compression ratio as percentage (e.g. 70.0% space savings)
    pub fn savings_ratio(&self) -> f64 {
        if self.uncompressed_size == 0 {
            0.0
        } else {
            let saved = (self.uncompressed_size as f64 - self.compressed_size as f64).max(0.0);
            (saved / self.uncompressed_size as f64) * 100.0
        }
    }
}

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::archive::{EntryState, ZipArchiveInspection};

pub const MANIFEST_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveManifestInfo {
    pub path: PathBuf,
    pub size: u64,
    pub modified_time_unix: u64,
    pub identity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EntryManifestRecord {
    pub index: usize,
    pub state: EntryState,
    pub compressed_size: u64,
    pub uncompressed_size: u64,
    pub crc32: u32,
    pub local_header_offset: u64,
    pub data_offset: u64,
    pub is_dir: bool,
    pub output_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionManifest {
    pub version: u32,
    pub job_id: String,
    pub created_at_unix: u64,
    pub updated_at_unix: u64,
    pub archive: ArchiveManifestInfo,
    pub destination: PathBuf,
    pub entries: BTreeMap<String, EntryManifestRecord>,
}

impl ExtractionManifest {
    /// Creates a fresh manifest initialized with all entries in PENDING state.
    pub fn create_new(
        job_id: &str,
        inspection: &ZipArchiveInspection,
        destination: &Path,
    ) -> Result<Self> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let archive_mtime = fs::metadata(&inspection.path)
            .and_then(|m| m.modified())
            .and_then(|t| t.duration_since(UNIX_EPOCH).map_err(std::io::Error::other))
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let mut entries = BTreeMap::new();
        for entry in &inspection.entries {
            entries.insert(
                entry.name.clone(),
                EntryManifestRecord {
                    index: entry.index,
                    state: EntryState::Pending,
                    compressed_size: entry.compressed_size,
                    uncompressed_size: entry.uncompressed_size,
                    crc32: entry.crc32,
                    local_header_offset: entry.local_header_offset,
                    data_offset: entry.data_offset,
                    is_dir: entry.is_dir,
                    output_path: None,
                },
            );
        }

        Ok(Self {
            version: MANIFEST_VERSION,
            job_id: job_id.to_string(),
            created_at_unix: now,
            updated_at_unix: now,
            archive: ArchiveManifestInfo {
                path: inspection.path.clone(),
                size: inspection.file_size,
                modified_time_unix: archive_mtime,
                identity: inspection.identity.clone(),
            },
            destination: destination.to_path_buf(),
            entries,
        })
    }

    /// Loads and parses a manifest file from disk.
    pub fn load(path: &Path) -> Result<Self> {
        let file =
            File::open(path).with_context(|| format!("Failed to open manifest at {:?}", path))?;
        let reader = BufReader::new(file);
        let manifest: ExtractionManifest = serde_json::from_reader(reader)
            .with_context(|| format!("Failed to parse manifest JSON at {:?}", path))?;
        if manifest.version != MANIFEST_VERSION {
            bail!(
                "Manifest version {} is not supported (expected {})",
                manifest.version,
                MANIFEST_VERSION
            );
        }
        Ok(manifest)
    }

    /// Atomically persists the manifest file to disk using a write-to-temp-then-rename strategy.
    pub fn save_atomic(&self, path: &Path) -> Result<()> {
        let parent_dir = path.parent();
        if let Some(parent) = parent_dir {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create manifest directory {:?}", parent))?;
        }

        let temp_filename = format!(".manifest_{}_{}.tmp", self.job_id, std::process::id());
        let temp_path = path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(temp_filename);

        {
            let temp_file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .open(&temp_path)
                .with_context(|| {
                    format!("Failed to create temporary manifest at {:?}", temp_path)
                })?;
            let mut writer = BufWriter::new(temp_file);
            serde_json::to_writer_pretty(&mut writer, self)
                .with_context(|| "Failed to serialize manifest JSON")?;
            writer.flush()?;
            let inner_file = writer.into_inner()?;
            inner_file.sync_all()?;
        }

        fs::rename(&temp_path, path).with_context(|| {
            format!(
                "Failed to atomically rename manifest from {:?} to {:?}",
                temp_path, path
            )
        })?;

        if let Some(parent) = parent_dir {
            if let Ok(dir_file) = File::open(parent) {
                let _ = dir_file.sync_all();
            }
        }

        Ok(())
    }

    pub fn verified_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, EntryState::Verified | EntryState::Reclaimed))
            .count()
    }

    pub fn failed_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, EntryState::Failed(_)))
            .count()
    }

    pub fn pending_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, EntryState::Pending))
            .count()
    }

    pub fn extracting_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, EntryState::Extracting))
            .count()
    }

    pub fn extracted_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, EntryState::Extracted))
            .count()
    }

    pub fn reclaimed_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, EntryState::Reclaimed))
            .count()
    }

    pub fn skipped_count(&self) -> usize {
        self.entries
            .values()
            .filter(|e| matches!(e.state, EntryState::Skipped))
            .count()
    }

    pub fn total_uncompressed_bytes(&self) -> u64 {
        self.entries.values().map(|e| e.uncompressed_size).sum()
    }

    pub fn verified_uncompressed_bytes(&self) -> u64 {
        self.entries
            .values()
            .filter(|e| matches!(e.state, EntryState::Verified | EntryState::Reclaimed))
            .map(|e| e.uncompressed_size)
            .sum()
    }

    pub fn progress_percent(&self) -> f64 {
        if self.entries.is_empty() {
            100.0
        } else {
            let completed = self
                .entries
                .values()
                .filter(|e| {
                    matches!(
                        e.state,
                        EntryState::Verified | EntryState::Reclaimed | EntryState::Skipped
                    )
                })
                .count();
            (completed as f64 / self.entries.len() as f64) * 100.0
        }
    }
}

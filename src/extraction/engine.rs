use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use crate::archive::ZipInspector;
use crate::extraction::collision::CollisionPolicy;
use crate::extraction::error::ExtractionError;
use crate::extraction::worker::{EntryWorker, WorkerResult};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionOptions {
    pub destination: PathBuf,
    pub collision_policy: CollisionPolicy,
    pub enable_sparse: bool,
    pub max_compression_ratio: f64,
    pub reclaim_archive: bool,
    pub verbose: bool,
}

impl Default for ExtractionOptions {
    fn default() -> Self {
        Self {
            destination: PathBuf::from("."),
            collision_policy: CollisionPolicy::Fail,
            enable_sparse: true,
            max_compression_ratio: 100.0,
            reclaim_archive: false,
            verbose: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionSummary {
    pub archive_path: PathBuf,
    pub destination: PathBuf,
    pub total_entries: usize,
    pub extracted_files: usize,
    pub skipped_files: usize,
    pub created_directories: usize,
    pub total_uncompressed_bytes: u64,
    pub sparse_bytes_saved: u64,
    pub duration: Duration,
}

pub struct ExtractionEngine;

impl ExtractionEngine {
    /// Extracts an entire archive to the destination directory with streaming verification.
    pub fn extract(
        archive_path: &Path,
        options: &ExtractionOptions,
    ) -> Result<ExtractionSummary, ExtractionError> {
        let start_time = Instant::now();

        // 1. Inspect archive headers and resolve data offsets
        let inspection = ZipInspector::inspect(archive_path).map_err(|e| {
            ExtractionError::Archive(format!("Failed to inspect archive: {}", e))
        })?;

        // 2. Open archive for streaming
        let mut archive_file = File::open(archive_path).map_err(|e| {
            ExtractionError::Archive(format!("Failed to open archive for extraction: {}", e))
        })?;

        // 3. Ensure destination directory exists
        std::fs::create_dir_all(&options.destination).map_err(|e| {
            ExtractionError::Io {
                entry: options.destination.to_string_lossy().to_string(),
                source: e,
            }
        })?;

        let mut extracted_files = 0;
        let mut skipped_files = 0;
        let mut created_directories = 0;
        let mut total_uncompressed_bytes = 0u64;
        let mut sparse_bytes_saved = 0u64;

        // 4. Sequential streaming extraction of entries
        for (i, entry) in inspection.entries.iter().enumerate() {
            if options.verbose {
                println!(
                    "[{}/{}] Extracting: {}",
                    i + 1,
                    inspection.entries.len(),
                    entry.name
                );
            }

            let result = EntryWorker::extract_entry(
                &mut archive_file,
                entry,
                &options.destination,
                options.collision_policy,
                options.enable_sparse,
                options.max_compression_ratio,
            )?;

            match result {
                WorkerResult::Extracted {
                    uncompressed_bytes,
                    sparse_bytes_saved: sparse_saved,
                    ..
                } => {
                    extracted_files += 1;
                    total_uncompressed_bytes += uncompressed_bytes;
                    sparse_bytes_saved += sparse_saved;
                }
                WorkerResult::Directory { .. } => {
                    created_directories += 1;
                }
                WorkerResult::Skipped { .. } => {
                    skipped_files += 1;
                }
            }
        }

        let duration = start_time.elapsed();

        Ok(ExtractionSummary {
            archive_path: archive_path.to_path_buf(),
            destination: options.destination.clone(),
            total_entries: inspection.entries.len(),
            extracted_files,
            skipped_files,
            created_directories,
            total_uncompressed_bytes,
            sparse_bytes_saved,
            duration,
        })
    }
}

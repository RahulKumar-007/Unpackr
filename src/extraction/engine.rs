use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use serde::{Deserialize, Serialize};

use crate::archive::{EntryState, ZipInspector};
use crate::extraction::collision::CollisionPolicy;
use crate::extraction::error::ExtractionError;
use crate::extraction::worker::{EntryWorker, WorkerResult};
use crate::state::job::{global_jobs_dir, JobId};
use crate::state::manifest::ExtractionManifest;
use crate::state::tracker::StateTracker;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionOptions {
    pub destination: PathBuf,
    pub collision_policy: CollisionPolicy,
    pub enable_sparse: bool,
    pub max_compression_ratio: f64,
    pub reclaim_archive: bool,
    pub state_dir: Option<PathBuf>,
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
            state_dir: None,
            verbose: false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExtractionSummary {
    pub job_id: String,
    pub archive_path: PathBuf,
    pub destination: PathBuf,
    pub manifest_path: PathBuf,
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
    /// Extracts an archive to the destination directory with persistent manifest state tracking.
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

        // 4. Setup or load Job Manifest State
        let job_id = JobId::generate(archive_path, &inspection.identity);
        let manifest_path = match &options.state_dir {
            Some(dir) => dir.join(job_id.as_str()).join("manifest.json"),
            None => options.destination.join(".unpackr").join("manifest.json"),
        };

        let mut tracker = if manifest_path.exists() {
            let tr = StateTracker::load_from_file(&manifest_path).map_err(|e| {
                ExtractionError::Archive(format!("Failed to load existing manifest: {}", e))
            })?;
            // Verify archive identity
            tr.verify_archive_identity(archive_path).map_err(|e| {
                ExtractionError::Archive(format!("Existing manifest archive validation failed: {}", e))
            })?;
            tr
        } else {
            let manifest = ExtractionManifest::create_new(
                job_id.as_str(),
                &inspection,
                &options.destination,
            )
            .map_err(|e| ExtractionError::Archive(format!("Failed to initialize manifest: {}", e)))?;
            manifest.save_atomic(&manifest_path).map_err(|e| {
                ExtractionError::Archive(format!("Failed to save initial manifest: {}", e))
            })?;

            // Also mirror manifest in global jobs directory if available
            if let Some(global_dir) = global_jobs_dir() {
                let global_manifest_path = global_dir.join(job_id.as_str()).join("manifest.json");
                let _ = manifest.save_atomic(&global_manifest_path);
            }

            StateTracker::new(manifest, manifest_path.clone())
        };

        let mut extracted_files = 0;
        let mut skipped_files = 0;
        let mut created_directories = 0;
        let mut total_uncompressed_bytes = 0u64;
        let mut sparse_bytes_saved = 0u64;

        // 5. Sequential streaming extraction with live state tracking
        for (i, entry) in inspection.entries.iter().enumerate() {
            // Check if already verified in manifest (only if not forcing Overwrite or Rename)
            if options.collision_policy != CollisionPolicy::Overwrite
                && options.collision_policy != CollisionPolicy::Rename
            {
                if let Some(record) = tracker.manifest.entries.get(&entry.name) {
                    if matches!(record.state, EntryState::Verified | EntryState::Reclaimed) {
                        if options.verbose {
                            println!(
                                "[{}/{}] (Verified) Skipping: {}",
                                i + 1,
                                inspection.entries.len(),
                                entry.name
                            );
                        }
                        if !entry.is_dir {
                            extracted_files += 1;
                            total_uncompressed_bytes += entry.uncompressed_size;
                        } else {
                            created_directories += 1;
                        }
                        continue;
                    }
                }
            }

            if options.verbose {
                println!(
                    "[{}/{}] Extracting: {}",
                    i + 1,
                    inspection.entries.len(),
                    entry.name
                );
            }

            // Transition: PENDING -> EXTRACTING
            tracker.set_entry_extracting(&entry.name);

            let result = EntryWorker::extract_entry(
                &mut archive_file,
                entry,
                &options.destination,
                options.collision_policy,
                options.enable_sparse,
                options.max_compression_ratio,
            );

            match result {
                Ok(WorkerResult::Extracted {
                    path,
                    uncompressed_bytes,
                    sparse_bytes_saved: sparse_saved,
                }) => {
                    // Transition: EXTRACTING -> EXTRACTED -> VERIFIED
                    tracker.set_entry_extracted(&entry.name);
                    tracker
                        .set_entry_verified(&entry.name, path)
                        .map_err(|e| ExtractionError::Archive(e.to_string()))?;

                    extracted_files += 1;
                    total_uncompressed_bytes += uncompressed_bytes;
                    sparse_bytes_saved += sparse_saved;
                }
                Ok(WorkerResult::Directory { path }) => {
                    tracker.set_entry_extracted(&entry.name);
                    tracker
                        .set_entry_verified(&entry.name, path)
                        .map_err(|e| ExtractionError::Archive(e.to_string()))?;

                    created_directories += 1;
                }
                Ok(WorkerResult::Skipped { path }) => {
                    tracker
                        .set_entry_skipped(&entry.name, path)
                        .map_err(|e| ExtractionError::Archive(e.to_string()))?;

                    skipped_files += 1;
                }
                Err(err) => {
                    // Transition: EXTRACTING -> FAILED
                    let _ = tracker.set_entry_failed(&entry.name, err.to_string());
                    return Err(err);
                }
            }
        }

        let duration = start_time.elapsed();

        Ok(ExtractionSummary {
            job_id: tracker.manifest.job_id.clone(),
            archive_path: archive_path.to_path_buf(),
            destination: options.destination.clone(),
            manifest_path,
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

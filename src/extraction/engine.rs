use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::archive::{EntryState, ZipInspector};
use crate::extraction::collision::CollisionPolicy;
use crate::extraction::error::ExtractionError;
use crate::extraction::worker::{EntryWorker, WorkerResult};
use crate::reclamation::{get_physical_allocated_bytes, ArchiveHolePuncher};
use crate::state::job::{find_manifest_for_job, global_jobs_dir, JobId};
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
    pub quiet: bool,
    #[serde(default)]
    pub max_total_size: Option<u64>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
    #[serde(default)]
    pub max_entries: Option<usize>,
}

pub const DEFAULT_MAX_ENTRIES: usize = 100_000;

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
            quiet: false,
            max_total_size: None,
            max_file_size: None,
            max_entries: Some(DEFAULT_MAX_ENTRIES),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResumeOptions {
    pub destination_override: Option<PathBuf>,
    pub archive_override: Option<PathBuf>,
    pub retry_failed: bool,
    pub verify_existing: bool,
    pub collision_policy: Option<CollisionPolicy>,
    pub enable_sparse: bool,
    pub max_compression_ratio: f64,
    pub reclaim_archive: bool,
    pub verbose: bool,
    pub quiet: bool,
    #[serde(default)]
    pub max_total_size: Option<u64>,
    #[serde(default)]
    pub max_file_size: Option<u64>,
    #[serde(default)]
    pub max_entries: Option<usize>,
}

impl Default for ResumeOptions {
    fn default() -> Self {
        Self {
            destination_override: None,
            archive_override: None,
            retry_failed: false,
            verify_existing: false,
            collision_policy: None,
            enable_sparse: true,
            max_compression_ratio: 100.0,
            reclaim_archive: false,
            verbose: false,
            quiet: false,
            max_total_size: None,
            max_file_size: None,
            max_entries: None,
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
    pub reclaimed_archive_bytes: u64,
    pub peak_disk_footprint_bytes: u64,
    pub throughput_mb_per_sec: f64,
    pub duration: Duration,
}

struct DestinationLock {
    _file: File,
}

impl DestinationLock {
    fn acquire(dir: &Path) -> Result<Self, ExtractionError> {
        let _ = std::fs::create_dir_all(dir);
        let lock_path = dir.join(".lock");
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|e| ExtractionError::Io {
                entry: lock_path.to_string_lossy().to_string(),
                source: e,
            })?;

        #[cfg(unix)]
        {
            use std::os::unix::io::AsRawFd;
            let ret = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() == Some(libc::EWOULDBLOCK)
                    || err.raw_os_error() == Some(libc::EAGAIN)
                {
                    return Err(ExtractionError::Archive(format!(
                        "Another unpackr process is currently extracting to destination {:?}",
                        dir
                    )));
                }
            }
        }

        Ok(Self { _file: file })
    }
}

struct ProcessEntriesParams<'a> {
    archive_path: &'a Path,
    destination: &'a Path,
    manifest_path: &'a Path,
    entries: &'a [crate::archive::ZipEntryMetadata],
    total_uncompressed_archive_size: u64,
    collision_policy: CollisionPolicy,
    enable_sparse: bool,
    max_compression_ratio: f64,
    max_file_size: Option<u64>,
    verbose: bool,
    quiet: bool,
    start_time: Instant,
    initial_archive_phys: u64,
    already_extracted_bytes: u64,
    already_reclaimed_bytes: u64,
}

fn process_entries(
    mut archive_file: File,
    mut puncher: Option<ArchiveHolePuncher>,
    tracker: &mut StateTracker,
    params: ProcessEntriesParams,
) -> Result<ExtractionSummary, ExtractionError> {
    let mut current_extracted_bytes = params.already_extracted_bytes;
    let mut current_sparse_saved = 0u64;
    let mut current_reclaimed_bytes = 0u64;
    let mut peak_disk_footprint = params.initial_archive_phys + params.already_extracted_bytes;

    let mut extracted_files = 0;
    let mut skipped_files = 0;
    let mut created_directories = 0;
    let mut total_uncompressed_bytes = 0u64;
    let mut sparse_bytes_saved = 0u64;
    let mut warned_reclamation_unsupported = false;

    for (i, entry) in params.entries.iter().enumerate() {
        if params.collision_policy != CollisionPolicy::Overwrite
            && params.collision_policy != CollisionPolicy::Rename
        {
            if let Some(record) = tracker.manifest.entries.get(&entry.name) {
                if matches!(record.state, EntryState::Verified | EntryState::Reclaimed) {
                    if params.verbose {
                        println!(
                            "[{}/{}] (Verified) Skipping: {}",
                            i + 1,
                            params.entries.len(),
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
                } else if matches!(record.state, EntryState::Skipped) {
                    skipped_files += 1;
                    continue;
                }
            }
        }

        if params.verbose {
            println!(
                "[{}/{}] Extracting: {}",
                i + 1,
                params.entries.len(),
                entry.name
            );
        }

        // Security check: Single file resource limit
        if let Some(max_file) = params.max_file_size {
            if entry.uncompressed_size > max_file {
                return Err(ExtractionError::ResourceLimitExceeded(format!(
                    "Entry '{}' uncompressed size is {} bytes, exceeding configured limit of {} bytes",
                    entry.name, entry.uncompressed_size, max_file
                )));
            }
        }

        // Transition: PENDING -> EXTRACTING
        tracker.set_entry_extracting(&entry.name);

        let result = EntryWorker::extract_entry(
            &mut archive_file,
            entry,
            params.destination,
            params.collision_policy,
            params.enable_sparse,
            params.max_compression_ratio,
        );

        match result {
            Ok(WorkerResult::Extracted {
                path,
                uncompressed_bytes,
                sparse_bytes_saved: sparse_saved,
            }) => {
                tracker.set_entry_extracted(&entry.name);

                // If storage reclamation enabled: punch hole in source archive BEFORE committing verified status (P1-02)
                let mut punched_bytes = 0u64;
                let mut is_reclaimed = false;
                if let Some(p) = &mut puncher {
                    match p.punch_entry(entry) {
                        Ok(punched) if punched > 0 => {
                            punched_bytes = punched;
                            is_reclaimed = true;
                        }
                        Ok(_) => {}
                        Err(e) => {
                            if matches!(
                                e,
                                crate::reclamation::ReclamationError::UnsupportedFilesystem
                            ) {
                                if !warned_reclamation_unsupported {
                                    warned_reclamation_unsupported = true;
                                    if !params.quiet {
                                        eprintln!("Warning: filesystem does not support hole punching (FALLOC_FL_PUNCH_HOLE). Archive reclamation is disabled.");
                                    }
                                }
                            } else if params.verbose {
                                eprintln!(
                                    "Warning: storage reclamation skipped for '{}': {}",
                                    entry.name, e
                                );
                            }
                        }
                    }
                }

                // Atomically update state in tracker (P1-02, P1-01)
                tracker
                    .set_entry_verified_and_reclaimed(&entry.name, path, is_reclaimed)
                    .map_err(|e| ExtractionError::Archive(e.to_string()))?;

                extracted_files += 1;
                total_uncompressed_bytes += uncompressed_bytes;
                sparse_bytes_saved += sparse_saved;

                let footprint_before_punch = params
                    .initial_archive_phys
                    .saturating_sub(current_reclaimed_bytes)
                    + (current_extracted_bytes + uncompressed_bytes)
                        .saturating_sub(current_sparse_saved + sparse_saved);
                if footprint_before_punch > peak_disk_footprint {
                    peak_disk_footprint = footprint_before_punch;
                }

                current_extracted_bytes += uncompressed_bytes;
                current_sparse_saved += sparse_saved;
                current_reclaimed_bytes += punched_bytes;

                if std::io::stderr().is_terminal() && !params.verbose && !params.quiet {
                    let pct = if params.total_uncompressed_archive_size > 0 {
                        (current_extracted_bytes as f64
                            / params.total_uncompressed_archive_size as f64
                            * 100.0)
                            .min(100.0)
                    } else {
                        (i + 1) as f64 / params.entries.len() as f64 * 100.0
                    };
                    let elapsed_secs = params.start_time.elapsed().as_secs_f64();
                    let mb_s = if elapsed_secs > 0.0 {
                        ((current_extracted_bytes.saturating_sub(params.already_extracted_bytes))
                            as f64
                            / 1_048_576.0)
                            / elapsed_secs
                    } else {
                        0.0
                    };
                    let eta_secs = if mb_s > 0.0
                        && params.total_uncompressed_archive_size > current_extracted_bytes
                    {
                        ((params.total_uncompressed_archive_size - current_extracted_bytes) as f64
                            / 1_048_576.0)
                            / mb_s
                    } else {
                        0.0
                    };
                    eprint!(
                        "\rExtracting: [{}/{}] ({:>5.1}%) - {:.1} MB/s - ETA: {:.0}s - Peak: {}",
                        i + 1,
                        params.entries.len(),
                        pct,
                        mb_s,
                        eta_secs,
                        crate::cli::inspect::format_bytes(peak_disk_footprint)
                    );
                    let _ = std::io::stderr().flush();
                }
            }
            Ok(WorkerResult::Directory { path }) => {
                tracker.set_entry_extracted(&entry.name);
                tracker
                    .set_entry_verified_and_reclaimed(&entry.name, path, false)
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
                // Transition: EXTRACTING -> FAILED (P0-04)
                if let Err(persist_err) = tracker.set_entry_failed(&entry.name, err.to_string()) {
                    eprintln!(
                        "Warning: failed to persist FAILED state for '{}': {}",
                        entry.name, persist_err
                    );
                }
                let _ = tracker.flush();
                return Err(err);
            }
        }
    }

    if std::io::stderr().is_terminal()
        && !params.verbose
        && !params.quiet
        && !params.entries.is_empty()
    {
        eprintln!();
    }

    // Flush any pending checkpointed writes to disk (P1-01)
    tracker
        .flush()
        .map_err(|e| ExtractionError::Archive(e.to_string()))?;

    let duration = params.start_time.elapsed();
    let secs = duration.as_secs_f64();
    let newly_extracted = current_extracted_bytes.saturating_sub(params.already_extracted_bytes);
    let throughput_mb_per_sec = if secs > 0.0 {
        (newly_extracted as f64 / 1_048_576.0) / secs
    } else {
        0.0
    };
    let reclaimed_archive_bytes = params.already_reclaimed_bytes
        + puncher
            .as_ref()
            .map(|p| p.total_reclaimed_bytes())
            .unwrap_or(0);

    Ok(ExtractionSummary {
        job_id: tracker.manifest.job_id.clone(),
        archive_path: params.archive_path.to_path_buf(),
        destination: params.destination.to_path_buf(),
        manifest_path: params.manifest_path.to_path_buf(),
        total_entries: params.entries.len(),
        extracted_files,
        skipped_files,
        created_directories,
        total_uncompressed_bytes,
        sparse_bytes_saved,
        reclaimed_archive_bytes,
        peak_disk_footprint_bytes: peak_disk_footprint,
        throughput_mb_per_sec,
        duration,
    })
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
        let inspection = ZipInspector::inspect(archive_path)
            .map_err(|e| ExtractionError::Archive(format!("Failed to inspect archive: {}", e)))?;

        // Security check: Overlapping compressed data streams (e.g. Fifield non-linear zip bomb)
        if options.reclaim_archive && inspection.has_overlapping_entries {
            return Err(ExtractionError::Archive(
                "Archive contains overlapping compressed data streams (e.g. Fifield non-linear zip bomb). In-place reclamation cannot be safely performed on overlapping entries.".to_string(),
            ));
        }

        // Security check: Resource limits
        if let Some(max_entries) = options.max_entries {
            if inspection.total_entries > max_entries {
                return Err(ExtractionError::ResourceLimitExceeded(format!(
                    "Archive contains {} entries, exceeding configured limit of {}",
                    inspection.total_entries, max_entries
                )));
            }
        }

        if let Some(max_total) = options.max_total_size {
            if inspection.total_uncompressed_size > max_total {
                return Err(ExtractionError::ResourceLimitExceeded(format!(
                    "Total uncompressed archive size is {} bytes, exceeding configured limit of {} bytes",
                    inspection.total_uncompressed_size, max_total
                )));
            }
        }

        // 2. Open archive for streaming and optional in-place reclamation
        let (archive_file, puncher) = if options.reclaim_archive {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(archive_path)
                .map_err(|e| {
                    ExtractionError::Archive(format!(
                        "Failed to open archive for read/write reclamation: {}",
                        e
                    ))
                })?;
            let puncher_file = file.try_clone().map_err(|e| {
                ExtractionError::Archive(format!(
                    "Failed to clone archive handle for reclamation: {}",
                    e
                ))
            })?;
            let puncher = ArchiveHolePuncher::new(
                puncher_file,
                inspection.file_size,
                inspection.central_directory_offset,
            );
            (file, Some(puncher))
        } else {
            let file = File::open(archive_path).map_err(|e| {
                ExtractionError::Archive(format!("Failed to open archive for extraction: {}", e))
            })?;
            (file, None)
        };

        // 3. Ensure destination directory exists and canonicalize (P1-04)
        std::fs::create_dir_all(&options.destination).map_err(|e| ExtractionError::Io {
            entry: options.destination.to_string_lossy().to_string(),
            source: e,
        })?;
        let destination = options
            .destination
            .canonicalize()
            .unwrap_or_else(|_| options.destination.clone());

        // File locking for destination directory (P2-13)
        let state_dir = options
            .state_dir
            .clone()
            .unwrap_or_else(|| destination.join(".unpackr"));
        let _dest_lock = DestinationLock::acquire(&state_dir)?;

        // 4. Setup or load Job Manifest State
        let job_id = JobId::generate(archive_path, &inspection.identity);
        let manifest_path = match &options.state_dir {
            Some(dir) => dir.join(job_id.as_str()).join("manifest.json"),
            None => destination.join(".unpackr").join("manifest.json"),
        };

        let mut tracker = if manifest_path.exists() {
            let mut tr = StateTracker::load_from_file(&manifest_path).map_err(|e| {
                ExtractionError::Archive(format!("Failed to load existing manifest: {}", e))
            })?;
            // Verify archive identity
            tr.verify_archive_identity(archive_path).map_err(|e| {
                ExtractionError::Archive(format!(
                    "Existing manifest archive validation failed: {}",
                    e
                ))
            })?;
            // Reconcile and clean any in-flight state from interrupted previous run
            tr.reconcile_and_clean(false, false).map_err(|e| {
                ExtractionError::Archive(format!("Failed to reconcile state: {}", e))
            })?;
            tr
        } else {
            let manifest =
                ExtractionManifest::create_new(job_id.as_str(), &inspection, &destination)
                    .map_err(|e| {
                        ExtractionError::Archive(format!("Failed to initialize manifest: {}", e))
                    })?;
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

        let initial_archive_phys =
            get_physical_allocated_bytes(archive_path).unwrap_or(inspection.file_size);

        let params = ProcessEntriesParams {
            archive_path,
            destination: &destination,
            manifest_path: &manifest_path,
            entries: &inspection.entries,
            total_uncompressed_archive_size: inspection.total_uncompressed_size,
            collision_policy: options.collision_policy,
            enable_sparse: options.enable_sparse,
            max_compression_ratio: options.max_compression_ratio,
            max_file_size: options.max_file_size,
            verbose: options.verbose,
            quiet: options.quiet,
            start_time,
            initial_archive_phys,
            already_extracted_bytes: 0,
            already_reclaimed_bytes: 0,
        };

        process_entries(archive_file, puncher, &mut tracker, params)
    }

    /// Resumes an interrupted extraction job, reconciling orphaned temporary files and incomplete entries.
    pub fn resume(
        job_id_or_path: &str,
        options: &ResumeOptions,
    ) -> Result<ExtractionSummary, ExtractionError> {
        let start_time = Instant::now();

        // 1. Locate existing manifest
        let manifest_path =
            find_manifest_for_job(job_id_or_path, options.destination_override.as_deref())
                .ok_or_else(|| {
                    ExtractionError::Archive(format!(
                        "Could not locate extraction manifest for '{}'",
                        job_id_or_path
                    ))
                })?;

        // 2. Load tracker
        let mut tracker = StateTracker::load_from_file(&manifest_path)
            .map_err(|e| ExtractionError::Archive(format!("Failed to load manifest: {}", e)))?;

        // 3. Resolve archive path
        let archive_path = options
            .archive_override
            .clone()
            .unwrap_or_else(|| tracker.manifest.archive.path.clone());

        if !archive_path.exists() {
            return Err(ExtractionError::Archive(format!(
                "Source archive not found at {:?}. If moved, specify --archive <path>",
                archive_path
            )));
        }

        // 4. Verify archive identity against initial inspection (using cached inspection if matched, P2-10)
        let maybe_inspection = tracker
            .verify_archive_identity_with_inspection(&archive_path)
            .map_err(|e| {
                ExtractionError::Archive(format!("Archive validation failed on resume: {}", e))
            })?;

        // 5. Resolve and canonicalize destination (P1-04)
        let raw_dest = options
            .destination_override
            .clone()
            .unwrap_or_else(|| tracker.manifest.destination.clone());

        std::fs::create_dir_all(&raw_dest).map_err(|e| ExtractionError::Io {
            entry: raw_dest.to_string_lossy().to_string(),
            source: e,
        })?;
        let destination = raw_dest.canonicalize().unwrap_or_else(|_| raw_dest.clone());

        // File locking for destination directory (P2-13)
        let state_dir = manifest_path.parent().unwrap_or(&destination);
        let _dest_lock = DestinationLock::acquire(state_dir)?;

        // 6. Reconcile state and clean orphaned temporary files
        let rec_summary = tracker
            .reconcile_and_clean(options.retry_failed, options.verify_existing)
            .map_err(|e| ExtractionError::Archive(format!("State reconciliation failed: {}", e)))?;

        if options.verbose {
            println!(
                "Reconciliation complete: {} orphaned files removed, {} interrupted entries reset, {} recovered as verified, {} pending",
                rec_summary.orphaned_tmp_files_removed,
                rec_summary.interrupted_entries_reset,
                rec_summary.recovered_verified_entries,
                rec_summary.pending_entries
            );
        }

        // 7. Inspect archive for entry metadata (reuse cached inspection if available)
        let inspection = match maybe_inspection {
            Some(insp) => insp,
            None => ZipInspector::inspect(&archive_path).map_err(|e| {
                ExtractionError::Archive(format!("Failed to inspect archive: {}", e))
            })?,
        };

        // Security check: Overlapping compressed data streams (e.g. Fifield non-linear zip bomb)
        if options.reclaim_archive && inspection.has_overlapping_entries {
            return Err(ExtractionError::Archive(
                "Archive contains overlapping compressed data streams (e.g. Fifield non-linear zip bomb). In-place reclamation cannot be safely performed on overlapping entries.".to_string(),
            ));
        }

        // Security check: Resource limits
        if let Some(max_entries) = options.max_entries {
            if inspection.total_entries > max_entries {
                return Err(ExtractionError::ResourceLimitExceeded(format!(
                    "Archive contains {} entries, exceeding configured limit of {}",
                    inspection.total_entries, max_entries
                )));
            }
        }

        if let Some(max_total) = options.max_total_size {
            if inspection.total_uncompressed_size > max_total {
                return Err(ExtractionError::ResourceLimitExceeded(format!(
                    "Total uncompressed archive size is {} bytes, exceeding configured limit of {} bytes",
                    inspection.total_uncompressed_size, max_total
                )));
            }
        }

        // 8. Open archive for streaming decompression and optional in-place reclamation
        let (archive_file, puncher) = if options.reclaim_archive {
            let file = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(&archive_path)
                .map_err(|e| {
                    ExtractionError::Archive(format!(
                        "Failed to open archive for read/write reclamation on resume: {}",
                        e
                    ))
                })?;
            let puncher_file = file.try_clone().map_err(|e| {
                ExtractionError::Archive(format!(
                    "Failed to clone archive handle for reclamation: {}",
                    e
                ))
            })?;
            let puncher = ArchiveHolePuncher::new(
                puncher_file,
                inspection.file_size,
                inspection.central_directory_offset,
            );
            (file, Some(puncher))
        } else {
            let file = File::open(&archive_path).map_err(|e| {
                ExtractionError::Archive(format!("Failed to open archive for resume: {}", e))
            })?;
            (file, None)
        };

        let collision_policy = options.collision_policy.unwrap_or(CollisionPolicy::Fail);
        let initial_archive_phys =
            get_physical_allocated_bytes(&archive_path).unwrap_or(inspection.file_size);

        let mut already_extracted_bytes = 0u64;
        let mut already_reclaimed_bytes = 0u64;
        for entry in &inspection.entries {
            if let Some(record) = tracker.manifest.entries.get(&entry.name) {
                if matches!(record.state, EntryState::Verified | EntryState::Reclaimed)
                    && !entry.is_dir
                {
                    already_extracted_bytes += entry.uncompressed_size;
                }
                if matches!(record.state, EntryState::Reclaimed) && !entry.is_dir {
                    if let Some((_, len)) =
                        crate::reclamation::puncher::compute_inward_reclaim_range(
                            entry.data_offset,
                            entry.compressed_size,
                            crate::reclamation::puncher::DEFAULT_BLOCK_SIZE,
                        )
                    {
                        already_reclaimed_bytes += len;
                    }
                }
            }
        }

        let params = ProcessEntriesParams {
            archive_path: &archive_path,
            destination: &destination,
            manifest_path: &manifest_path,
            entries: &inspection.entries,
            total_uncompressed_archive_size: inspection.total_uncompressed_size,
            collision_policy,
            enable_sparse: options.enable_sparse,
            max_compression_ratio: options.max_compression_ratio,
            max_file_size: options.max_file_size,
            verbose: options.verbose,
            quiet: options.quiet,
            start_time,
            initial_archive_phys,
            already_extracted_bytes,
            already_reclaimed_bytes,
        };

        process_entries(archive_file, puncher, &mut tracker, params)
    }
}

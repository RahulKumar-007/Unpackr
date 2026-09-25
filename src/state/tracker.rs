use anyhow::{bail, Context, Result};
use crc32fast::Hasher;
use serde::{Deserialize, Serialize};
use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::archive::{compute_archive_identity, EntryState, ZipArchiveInspection, ZipInspector};
use crate::state::manifest::ExtractionManifest;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReconciliationSummary {
    pub orphaned_tmp_files_removed: usize,
    pub interrupted_entries_reset: usize,
    pub recovered_verified_entries: usize,
    pub missing_verified_reset: usize,
    pub failed_retried: usize,
    pub pending_entries: usize,
}

pub struct StateTracker {
    pub manifest: ExtractionManifest,
    pub manifest_path: PathBuf,
    pub manifest_needs_save: bool,
    dirty_count: usize,
    last_checkpoint: std::time::Instant,
}

#[derive(Debug, Clone)]
pub struct VerificationFailure {
    pub entry_name: String,
    pub path: PathBuf,
    pub reason: String,
}

impl StateTracker {
    pub fn new(manifest: ExtractionManifest, manifest_path: PathBuf) -> Self {
        Self {
            manifest,
            manifest_path,
            manifest_needs_save: false,
            dirty_count: 0,
            last_checkpoint: std::time::Instant::now(),
        }
    }

    /// Loads an existing tracker from a manifest file.
    pub fn load_from_file(manifest_path: &Path) -> Result<Self> {
        let manifest = ExtractionManifest::load(manifest_path)?;
        Ok(Self::new(manifest, manifest_path.to_path_buf()))
    }

    /// Verifies that the source archive on disk still matches the cryptographic
    /// identity stored in the manifest.
    /// Verifies that the source archive on disk still matches the cryptographic
    /// identity stored in the manifest. Returns cached inspection if central directory was read.
    pub fn verify_archive_identity_with_inspection(
        &self,
        current_archive_path: &Path,
    ) -> Result<Option<ZipArchiveInspection>> {
        let metadata = std::fs::metadata(current_archive_path)
            .with_context(|| format!("Failed to read metadata for {:?}", current_archive_path))?;

        if metadata.len() != self.manifest.archive.size {
            bail!(
                "Archive size mismatch! Expected: {} bytes, Current: {} bytes",
                self.manifest.archive.size,
                metadata.len()
            );
        }

        // If no entries have been reclaimed yet, the archive must match the exact initial hash.
        if self.manifest.reclaimed_count() == 0 {
            let current_identity =
                compute_archive_identity(current_archive_path).with_context(|| {
                    format!("Failed to compute identity for {:?}", current_archive_path)
                })?;

            if current_identity != self.manifest.archive.identity {
                bail!(
                    "Archive identity mismatch!\nExpected: {}\nCurrent:  {}\nThe archive file appears to have been modified or replaced.",
                    self.manifest.archive.identity,
                    current_identity
                );
            }
            return Ok(None);
        }

        // If entries have already been reclaimed, in-place hole punching has legally altered
        // the payload blocks and filesystem mtime. We verify structural archive identity:
        // 1. Central directory entries must match all manifest records
        let inspection = ZipInspector::inspect(current_archive_path)
            .with_context(|| "Failed to parse Central Directory of reclaimed archive")?;

        if inspection.entries.len() != self.manifest.entries.len() {
            bail!(
                "Archive entry count mismatch! Manifest has {}, archive has {}",
                self.manifest.entries.len(),
                inspection.entries.len()
            );
        }

        for entry in &inspection.entries {
            let record = match self.manifest.entries.get(&entry.name) {
                Some(r) => r,
                None => bail!("Archive contains unexpected entry: {}", entry.name),
            };
            if record.crc32 != entry.crc32 || record.uncompressed_size != entry.uncompressed_size {
                bail!("Entry metadata mismatch for: {}", entry.name);
            }
        }

        // 2. Local file headers must have valid signatures
        use std::io::{Seek, SeekFrom};
        let mut file = File::open(current_archive_path)?;
        let mut sig = [0u8; 4];
        for (name, record) in &self.manifest.entries {
            file.seek(SeekFrom::Start(record.local_header_offset))?;
            file.read_exact(&mut sig)?;
            if u32::from_le_bytes(sig) != 0x04034b50 {
                bail!("Corrupted local file header for entry: {}", name);
            }
        }

        Ok(Some(inspection))
    }

    /// Verifies that the source archive on disk still matches the cryptographic
    /// identity stored in the manifest.
    pub fn verify_archive_identity(&self, current_archive_path: &Path) -> Result<()> {
        self.verify_archive_identity_with_inspection(current_archive_path)
            .map(|_| ())
    }

    pub fn set_entry_extracting(&mut self, name: &str) {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Extracting;
            self.touch();
        }
    }

    pub fn set_entry_extracted(&mut self, name: &str) {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Extracted;
            self.touch();
        }
    }

    /// Atomically records both verified and reclaimed status for an entry (P1-02).
    /// Batches manifest persistence using checkpoint intervals (P1-01).
    pub fn set_entry_verified_and_reclaimed(
        &mut self,
        name: &str,
        output_path: PathBuf,
        reclaimed: bool,
    ) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = if reclaimed {
                EntryState::Reclaimed
            } else {
                EntryState::Verified
            };
            record.output_path = Some(output_path);
            self.touch();
            self.manifest_needs_save = true;
            self.dirty_count += 1;
        }
        self.save_checkpoint_if_needed(false)?;
        Ok(())
    }

    fn persist_manifest(&self) -> Result<()> {
        self.manifest.save_atomic(&self.manifest_path)?;
        if let Some(global_dir) = crate::state::job::global_jobs_dir() {
            let global_manifest_path = global_dir.join(&self.manifest.job_id).join("manifest.json");
            if global_manifest_path != self.manifest_path && global_manifest_path.exists() {
                let _ = self.manifest.save_atomic(&global_manifest_path);
            }
        }
        Ok(())
    }

    /// Checkpoints the manifest to disk if threshold of entries (100) or time (5s) is reached,
    /// or if force is true.
    pub fn save_checkpoint_if_needed(&mut self, force: bool) -> Result<()> {
        if !self.manifest_needs_save {
            return Ok(());
        }

        let should_save = force
            || self.dirty_count >= 100
            || self.last_checkpoint.elapsed() >= std::time::Duration::from_secs(5);

        if should_save {
            self.persist_manifest()?;
            self.manifest_needs_save = false;
            self.dirty_count = 0;
            self.last_checkpoint = std::time::Instant::now();
        }
        Ok(())
    }

    /// Explicitly flushes any pending manifest changes to disk.
    pub fn flush(&mut self) -> Result<()> {
        self.save_checkpoint_if_needed(true)
    }

    pub fn set_entry_verified(&mut self, name: &str, output_path: PathBuf) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Verified;
            record.output_path = Some(output_path);
            self.touch();
            self.manifest_needs_save = true;
            self.persist_manifest()?;
            self.manifest_needs_save = false;
            self.dirty_count = 0;
        }
        Ok(())
    }

    pub fn set_entry_reclaimed(&mut self, name: &str) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Reclaimed;
            self.touch();
            self.manifest_needs_save = true;
            self.persist_manifest()?;
            self.manifest_needs_save = false;
            self.dirty_count = 0;
        }
        Ok(())
    }

    pub fn set_entry_skipped(&mut self, name: &str, output_path: PathBuf) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Skipped;
            record.output_path = Some(output_path);
            self.touch();
            self.manifest_needs_save = true;
            self.dirty_count += 1;
        }
        self.save_checkpoint_if_needed(false)?;
        Ok(())
    }

    pub fn set_entry_failed(&mut self, name: &str, reason: String) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Failed(reason);
            self.touch();
            self.manifest_needs_save = true;
            self.persist_manifest()?;
            self.manifest_needs_save = false;
            self.dirty_count = 0;
        }
        Ok(())
    }

    fn touch(&mut self) {
        self.manifest.updated_at_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
    }

    /// Verifies all previously extracted files on disk against their manifest CRC-32 and sizes.
    pub fn verify_extracted_output(&self) -> Vec<VerificationFailure> {
        let mut failures = Vec::new();

        for (name, record) in &self.manifest.entries {
            if !matches!(record.state, EntryState::Verified | EntryState::Reclaimed) {
                continue;
            }

            let output_path = match &record.output_path {
                Some(p) => p,
                None => {
                    failures.push(VerificationFailure {
                        entry_name: name.clone(),
                        path: PathBuf::new(),
                        reason: "Manifest marked entry as verified but output_path is missing"
                            .to_string(),
                    });
                    continue;
                }
            };

            if record.is_dir {
                if !output_path.is_dir() {
                    failures.push(VerificationFailure {
                        entry_name: name.clone(),
                        path: output_path.clone(),
                        reason: "Directory does not exist on disk".to_string(),
                    });
                }
                continue;
            }

            if !output_path.is_file() {
                failures.push(VerificationFailure {
                    entry_name: name.clone(),
                    path: output_path.clone(),
                    reason: "File does not exist on disk".to_string(),
                });
                continue;
            }

            // Stream file to verify size and CRC
            match File::open(output_path) {
                Err(e) => {
                    failures.push(VerificationFailure {
                        entry_name: name.clone(),
                        path: output_path.clone(),
                        reason: format!("Failed to open file: {}", e),
                    });
                }
                Ok(file) => {
                    let mut reader = BufReader::with_capacity(65536, file);
                    let mut hasher = Hasher::new();
                    let mut total_bytes = 0u64;
                    let mut buf = [0u8; 65536];

                    let mut io_error = false;
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) => break,
                            Ok(n) => {
                                hasher.update(&buf[..n]);
                                total_bytes += n as u64;
                            }
                            Err(e) => {
                                failures.push(VerificationFailure {
                                    entry_name: name.clone(),
                                    path: output_path.clone(),
                                    reason: format!("I/O read error during verification: {}", e),
                                });
                                io_error = true;
                                break;
                            }
                        }
                    }

                    if io_error {
                        continue;
                    }

                    if total_bytes != record.uncompressed_size {
                        failures.push(VerificationFailure {
                            entry_name: name.clone(),
                            path: output_path.clone(),
                            reason: format!(
                                "File size mismatch (disk: {} B, expected: {} B)",
                                total_bytes, record.uncompressed_size
                            ),
                        });
                        continue;
                    }

                    let computed_crc = hasher.finalize();
                    if computed_crc != record.crc32 {
                        failures.push(VerificationFailure {
                            entry_name: name.clone(),
                            path: output_path.clone(),
                            reason: format!(
                                "CRC-32 mismatch (disk: 0x{:08X}, expected: 0x{:08X})",
                                computed_crc, record.crc32
                            ),
                        });
                    }
                }
            }
        }

        failures
    }

    /// Reconciles state after a crash or interruption, cleaning up orphaned temporary files
    /// and reconciling in-flight entries against actual disk contents.
    pub fn reconcile_and_clean(
        &mut self,
        retry_failed: bool,
        verify_existing: bool,
    ) -> Result<ReconciliationSummary> {
        let mut summary = ReconciliationSummary {
            orphaned_tmp_files_removed: 0,
            interrupted_entries_reset: 0,
            recovered_verified_entries: 0,
            missing_verified_reset: 0,
            failed_retried: 0,
            pending_entries: 0,
        };

        // 1. Scan and remove orphaned temporary files in destination directory
        let orphaned = find_orphaned_temp_files(&self.manifest.destination);
        for tmp_path in orphaned {
            if std::fs::remove_file(&tmp_path).is_ok() {
                summary.orphaned_tmp_files_removed += 1;
            }
        }

        // 2. Reconcile entries against actual filesystem state
        let dest = self.manifest.destination.clone();
        for (name, record) in &mut self.manifest.entries {
            let expected_path = record
                .output_path
                .clone()
                .unwrap_or_else(|| dest.join(name));

            match &record.state {
                EntryState::Extracting | EntryState::Extracted => {
                    // Crashed during extraction or before rename
                    if record.is_dir {
                        if expected_path.is_dir() {
                            record.state = EntryState::Verified;
                            record.output_path = Some(expected_path);
                            summary.recovered_verified_entries += 1;
                        } else {
                            record.state = EntryState::Pending;
                            record.output_path = None;
                            summary.interrupted_entries_reset += 1;
                        }
                    } else if expected_path.is_file() {
                        match verify_single_file(
                            &expected_path,
                            record.uncompressed_size,
                            record.crc32,
                        ) {
                            Ok(true) => {
                                record.state = EntryState::Verified;
                                record.output_path = Some(expected_path);
                                summary.recovered_verified_entries += 1;
                            }
                            _ => {
                                let _ = std::fs::remove_file(&expected_path);
                                record.state = EntryState::Pending;
                                record.output_path = None;
                                summary.interrupted_entries_reset += 1;
                            }
                        }
                    } else {
                        record.state = EntryState::Pending;
                        record.output_path = None;
                        summary.interrupted_entries_reset += 1;
                    }
                }
                EntryState::Verified => {
                    let exists = if record.is_dir {
                        expected_path.is_dir()
                    } else {
                        expected_path.is_file()
                    };

                    if !exists {
                        record.state = EntryState::Pending;
                        record.output_path = None;
                        summary.missing_verified_reset += 1;
                    } else if verify_existing && !record.is_dir {
                        match verify_single_file(
                            &expected_path,
                            record.uncompressed_size,
                            record.crc32,
                        ) {
                            Ok(true) => {}
                            _ => {
                                let _ = std::fs::remove_file(&expected_path);
                                record.state = EntryState::Pending;
                                record.output_path = None;
                                summary.missing_verified_reset += 1;
                            }
                        }
                    }
                }
                EntryState::Failed(_) => {
                    if retry_failed {
                        record.state = EntryState::Pending;
                        record.output_path = None;
                        summary.failed_retried += 1;
                    }
                }
                EntryState::Pending => {
                    // Check if file already exists and matches expected CRC/size
                    // (e.g. process was killed right after rename but before manifest was flushed)
                    if record.is_dir {
                        if expected_path.is_dir() {
                            record.state = EntryState::Verified;
                            record.output_path = Some(expected_path);
                            summary.recovered_verified_entries += 1;
                        }
                    } else if expected_path.is_file() {
                        if let Ok(true) = verify_single_file(
                            &expected_path,
                            record.uncompressed_size,
                            record.crc32,
                        ) {
                            record.state = EntryState::Verified;
                            record.output_path = Some(expected_path);
                            summary.recovered_verified_entries += 1;
                        }
                    }
                }
                EntryState::Skipped | EntryState::Reclaimed => {}
            }
        }

        summary.pending_entries = self.manifest.pending_count();
        self.touch();
        self.persist_manifest()?;

        Ok(summary)
    }

    /// Cancels the extraction job, cleaning up all orphaned temp files and marking incomplete entries as failed.
    pub fn cancel(&mut self) -> Result<usize> {
        let orphaned = find_orphaned_temp_files(&self.manifest.destination);
        for tmp_path in orphaned {
            let _ = std::fs::remove_file(&tmp_path);
        }

        let mut cancelled_count = 0;
        for record in self.manifest.entries.values_mut() {
            if matches!(
                record.state,
                EntryState::Extracting | EntryState::Extracted | EntryState::Pending
            ) {
                record.state = EntryState::Failed("Extraction cancelled by user".to_string());
                cancelled_count += 1;
            }
        }

        self.touch();
        self.persist_manifest()?;
        Ok(cancelled_count)
    }
}

/// Recursively discovers any orphaned temporary files created by Unpackr (`.*.unpackr_tmp_*` and `.manifest_*.tmp`).
pub fn find_orphaned_temp_files(dir: &Path) -> Vec<PathBuf> {
    let mut temp_files = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                let file_name = entry.file_name();
                let name_str = file_name.to_string_lossy();
                if name_str == ".unpackr" {
                    if let Ok(sub_entries) = std::fs::read_dir(&path) {
                        for sub_entry in sub_entries.flatten() {
                            let sub_path = sub_entry.path();
                            let s = sub_entry.file_name();
                            let s_str = s.to_string_lossy();
                            if s_str.starts_with(".manifest_") && s_str.ends_with(".tmp") {
                                temp_files.push(sub_path);
                            }
                        }
                    }
                } else {
                    temp_files.extend(find_orphaned_temp_files(&path));
                }
            } else if path.is_file() {
                let file_name = entry.file_name();
                let name_str = file_name.to_string_lossy();
                if name_str.starts_with('.') && name_str.contains(".unpackr_tmp_") {
                    temp_files.push(path);
                }
            }
        }
    }
    temp_files
}

/// Helper to verify a single file's size and CRC-32 in streaming chunks.
pub fn verify_single_file(path: &Path, expected_size: u64, expected_crc: u32) -> Result<bool> {
    let metadata = std::fs::metadata(path)?;
    if metadata.len() != expected_size {
        return Ok(false);
    }
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(65536, file);
    let mut hasher = Hasher::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize() == expected_crc)
}

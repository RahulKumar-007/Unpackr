use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{bail, Context, Result};
use crc32fast::Hasher;
use serde::{Deserialize, Serialize};

use crate::archive::{compute_archive_identity, EntryState};
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
}

#[derive(Debug)]
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
        }
    }

    /// Loads an existing tracker from a manifest file.
    pub fn load_from_file(manifest_path: &Path) -> Result<Self> {
        let manifest = ExtractionManifest::load(manifest_path)?;
        Ok(Self {
            manifest,
            manifest_path: manifest_path.to_path_buf(),
        })
    }

    /// Verifies that the source archive on disk still matches the cryptographic
    /// identity stored in the manifest.
    pub fn verify_archive_identity(&self, current_archive_path: &Path) -> Result<()> {
        let current_identity = compute_archive_identity(current_archive_path)
            .with_context(|| format!("Failed to compute identity for {:?}", current_archive_path))?;

        if current_identity != self.manifest.archive.identity {
            bail!(
                "Archive identity mismatch!\nExpected: {}\nCurrent:  {}\nThe archive file appears to have been modified or replaced.",
                self.manifest.archive.identity,
                current_identity
            );
        }
        Ok(())
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

    pub fn set_entry_verified(&mut self, name: &str, output_path: PathBuf) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Verified;
            record.output_path = Some(output_path);
            self.touch();
            self.manifest.save_atomic(&self.manifest_path)?;
        }
        Ok(())
    }

    pub fn set_entry_reclaimed(&mut self, name: &str) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Reclaimed;
            self.touch();
            self.manifest.save_atomic(&self.manifest_path)?;
        }
        Ok(())
    }

    pub fn set_entry_skipped(&mut self, name: &str, output_path: PathBuf) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Skipped;
            record.output_path = Some(output_path);
            self.touch();
            self.manifest.save_atomic(&self.manifest_path)?;
        }
        Ok(())
    }

    pub fn set_entry_failed(&mut self, name: &str, reason: String) -> Result<()> {
        if let Some(record) = self.manifest.entries.get_mut(name) {
            record.state = EntryState::Failed(reason);
            self.touch();
            self.manifest.save_atomic(&self.manifest_path)?;
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

        for record in self.manifest.entries.values() {
            if !matches!(record.state, EntryState::Verified | EntryState::Reclaimed) {
                continue;
            }

            let output_path = match &record.output_path {
                Some(p) => p,
                None => {
                    failures.push(VerificationFailure {
                        entry_name: record.name.clone(),
                        path: PathBuf::new(),
                        reason: "Manifest marked entry as verified but output_path is missing".to_string(),
                    });
                    continue;
                }
            };

            if record.is_dir {
                if !output_path.is_dir() {
                    failures.push(VerificationFailure {
                        entry_name: record.name.clone(),
                        path: output_path.clone(),
                        reason: "Directory does not exist on disk".to_string(),
                    });
                }
                continue;
            }

            if !output_path.is_file() {
                failures.push(VerificationFailure {
                    entry_name: record.name.clone(),
                    path: output_path.clone(),
                    reason: "File does not exist on disk".to_string(),
                });
                continue;
            }

            // Stream file to verify size and CRC
            match File::open(output_path) {
                Err(e) => {
                    failures.push(VerificationFailure {
                        entry_name: record.name.clone(),
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
                                    entry_name: record.name.clone(),
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
                            entry_name: record.name.clone(),
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
                            entry_name: record.name.clone(),
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
        for record in self.manifest.entries.values_mut() {
            let expected_path = record
                .output_path
                .clone()
                .unwrap_or_else(|| dest.join(&record.name));

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
                        match verify_single_file(&expected_path, record.uncompressed_size, record.crc32) {
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
                        match verify_single_file(&expected_path, record.uncompressed_size, record.crc32) {
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
                        if let Ok(true) = verify_single_file(&expected_path, record.uncompressed_size, record.crc32) {
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
        self.manifest.save_atomic(&self.manifest_path)?;

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
        self.manifest.save_atomic(&self.manifest_path)?;
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

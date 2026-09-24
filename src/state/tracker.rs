use std::fs::File;
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use anyhow::{bail, Context, Result};
use crc32fast::Hasher;

use crate::archive::{compute_archive_identity, EntryState};
use crate::state::manifest::ExtractionManifest;

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
}

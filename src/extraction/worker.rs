use crc32fast::Hasher;
use flate2::bufread::DeflateDecoder;
use std::fs::{self, File, OpenOptions};
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use crate::archive::{CompressionMethod, ZipEntryMetadata};
use crate::extraction::collision::CollisionPolicy;
use crate::extraction::error::ExtractionError;
use crate::extraction::sparse_writer::SparseWriter;
use crate::security::{
    apply_safe_permissions, check_forbidden_device_type, check_symlink_traversal,
    resolve_safe_dest, sanitize_entry_path, validate_symlink_target,
};

#[derive(Debug)]
pub enum WorkerResult {
    Directory {
        path: PathBuf,
    },
    Skipped {
        path: PathBuf,
    },
    Extracted {
        path: PathBuf,
        uncompressed_bytes: u64,
        sparse_bytes_saved: u64,
    },
}

pub struct EntryWorker;

impl EntryWorker {
    /// Streams, decompresses, verifies, and atomically writes a single archive entry.
    pub fn extract_entry(
        archive_file: &mut File,
        entry: &ZipEntryMetadata,
        dest_root: &Path,
        collision_policy: CollisionPolicy,
        enable_sparse: bool,
        max_compression_ratio: f64,
    ) -> Result<WorkerResult, ExtractionError> {
        // 0. Forbidden special device files (block, character, FIFO, socket)
        if let Err(reason) = check_forbidden_device_type(entry.external_attributes) {
            return Err(ExtractionError::ForbiddenDeviceType {
                entry: entry.name.clone(),
                reason,
            });
        }

        // 1. Path sanitization (Zip Slip defense, Windows devices, .unpackr protection)
        let sanitized =
            sanitize_entry_path(&entry.name).map_err(|e| ExtractionError::Security {
                entry: entry.name.clone(),
                source: e,
            })?;

        let target_path =
            resolve_safe_dest(dest_root, &sanitized).map_err(|e| ExtractionError::Security {
                entry: entry.name.clone(),
                source: e,
            })?;

        // 2. Symlink traversal & poisoning defense on filesystem
        check_symlink_traversal(dest_root, &sanitized).map_err(|e| ExtractionError::Security {
            entry: entry.name.clone(),
            source: e,
        })?;

        // 3. Handle directory entries
        if entry.is_dir {
            fs::create_dir_all(&target_path).map_err(|e| ExtractionError::Io {
                entry: entry.name.clone(),
                source: e,
            })?;
            let _ = apply_safe_permissions(&target_path, entry.external_attributes, true);
            return Ok(WorkerResult::Directory { path: target_path });
        }

        // 4. Handle symlink entries
        if entry.is_symlink {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent).map_err(|e| ExtractionError::Io {
                    entry: entry.name.clone(),
                    source: e,
                })?;
            }

            let resolved_target = match collision_policy.resolve_collision(&target_path) {
                Ok(Some(path)) => path,
                Ok(None) => return Ok(WorkerResult::Skipped { path: target_path }),
                Err(_) => {
                    return Err(ExtractionError::DestinationCollision {
                        entry: entry.name.clone(),
                        path: target_path,
                    });
                }
            };

            // Read the symlink target path bytes (limit to safe path length, e.g. 4096 bytes)
            if entry.uncompressed_size > 4096 {
                return Err(ExtractionError::Archive(format!(
                    "Symlink target path too long ({} bytes) for entry '{}'",
                    entry.uncompressed_size, entry.name
                )));
            }

            let target_bytes = Self::read_entry_bytes(archive_file, entry)?;
            let target_str = String::from_utf8(target_bytes).map_err(|_| {
                ExtractionError::Archive(format!(
                    "Symlink target for entry '{}' is not valid UTF-8",
                    entry.name
                ))
            })?;

            // Validate symlink target safety (prevent escaping dest_root)
            let symlink_dir = resolved_target.parent().unwrap_or(dest_root);
            validate_symlink_target(dest_root, symlink_dir, &target_str).map_err(|e| {
                ExtractionError::Security {
                    entry: entry.name.clone(),
                    source: e,
                }
            })?;

            #[cfg(unix)]
            {
                let temp_filename = format!(
                    ".{}.unpackr_tmp_symlink_{}",
                    resolved_target
                        .file_name()
                        .map(|s| s.to_string_lossy().to_string())
                        .unwrap_or_else(|| "link".to_string()),
                    entry.index
                );
                let temp_path = symlink_dir.join(temp_filename);
                let _ = fs::remove_file(&temp_path);
                std::os::unix::fs::symlink(&target_str, &temp_path).map_err(|e| {
                    ExtractionError::Io {
                        entry: entry.name.clone(),
                        source: e,
                    }
                })?;
                fs::rename(&temp_path, &resolved_target).map_err(|e| {
                    let _ = fs::remove_file(&temp_path);
                    ExtractionError::Io {
                        entry: entry.name.clone(),
                        source: e,
                    }
                })?;
            }

            #[cfg(not(unix))]
            {
                return Err(ExtractionError::Archive(
                    "Symlink creation is not supported on this platform".to_string(),
                ));
            }

            return Ok(WorkerResult::Extracted {
                path: resolved_target,
                uncompressed_bytes: entry.uncompressed_size,
                sparse_bytes_saved: 0,
            });
        }

        // 5. Compression bomb defense: tiered ratio check across all sizes (P2-08)
        if entry.compressed_size > 0 && max_compression_ratio > 0.0 {
            let ratio = entry.uncompressed_size as f64 / entry.compressed_size as f64;
            let effective_max_ratio = if entry.uncompressed_size <= 10 * 1024 * 1024 {
                max_compression_ratio.max(1500.0)
            } else {
                max_compression_ratio
            };
            if ratio > effective_max_ratio {
                return Err(ExtractionError::SuspiciousCompressionRatio {
                    entry: entry.name.clone(),
                    ratio,
                    limit: effective_max_ratio,
                });
            }
        }

        // 6. Ensure parent directory exists
        if let Some(parent) = target_path.parent() {
            fs::create_dir_all(parent).map_err(|e| ExtractionError::Io {
                entry: entry.name.clone(),
                source: e,
            })?;
        }

        // 7. Collision policy check
        let resolved_target = match collision_policy.resolve_collision(&target_path) {
            Ok(Some(path)) => path,
            Ok(None) => return Ok(WorkerResult::Skipped { path: target_path }),
            Err(_) => {
                return Err(ExtractionError::DestinationCollision {
                    entry: entry.name.clone(),
                    path: target_path,
                })
            }
        };

        // 8. Temporary file path for atomic swap
        let temp_filename = format!(
            ".{}.unpackr_tmp_{}",
            resolved_target
                .file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "file".to_string()),
            entry.index
        );
        let temp_path = resolved_target
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(temp_filename);

        let temp_file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&temp_path)
            .map_err(|e| ExtractionError::Io {
                entry: entry.name.clone(),
                source: e,
            })?;

        // 9. Streaming Decompression & In-flight CRC-32 Calculation
        let extraction_res = Self::stream_decompress(archive_file, entry, temp_file, enable_sparse);

        match extraction_res {
            Ok((bytes_written, sparse_saved)) => {
                // Apply safe sanitized permissions before atomic rename
                let _ = apply_safe_permissions(&temp_path, entry.external_attributes, false);

                // 10. Atomic Rename on verified output
                fs::rename(&temp_path, &resolved_target).map_err(|e| {
                    let _ = fs::remove_file(&temp_path);
                    ExtractionError::Io {
                        entry: entry.name.clone(),
                        source: e,
                    }
                })?;

                Ok(WorkerResult::Extracted {
                    path: resolved_target,
                    uncompressed_bytes: bytes_written,
                    sparse_bytes_saved: sparse_saved,
                })
            }
            Err(err) => {
                // Cleanup temp file on failure
                let _ = fs::remove_file(&temp_path);
                Err(err)
            }
        }
    }

    fn stream_decompress(
        archive_file: &mut File,
        entry: &ZipEntryMetadata,
        temp_file: File,
        enable_sparse: bool,
    ) -> Result<(u64, u64), ExtractionError> {
        let mut sparse_writer = SparseWriter::new(temp_file, enable_sparse);
        let mut hasher = Hasher::new();
        let mut total_uncompressed_bytes = 0u64;

        // Seek to exact data offset
        archive_file
            .seek(SeekFrom::Start(entry.data_offset))
            .map_err(|e| ExtractionError::Io {
                entry: entry.name.clone(),
                source: e,
            })?;

        // Bounded read chunk size: 64 KB
        let mut chunk_buf = [0u8; 65536];

        match entry.compression_method {
            CompressionMethod::Stored => {
                let mut reader = archive_file.take(entry.compressed_size);
                loop {
                    let n = reader
                        .read(&mut chunk_buf)
                        .map_err(|e| ExtractionError::Io {
                            entry: entry.name.clone(),
                            source: e,
                        })?;
                    if n == 0 {
                        break;
                    }
                    hasher.update(&chunk_buf[..n]);
                    sparse_writer.write_chunk(&chunk_buf[..n]).map_err(|e| {
                        ExtractionError::Io {
                            entry: entry.name.clone(),
                            source: e,
                        }
                    })?;
                    total_uncompressed_bytes += n as u64;
                }
            }
            CompressionMethod::Deflated => {
                let limited = archive_file.take(entry.compressed_size);
                let buf_reader = BufReader::with_capacity(65536, limited);
                let mut decoder = DeflateDecoder::new(buf_reader);

                loop {
                    let n = decoder
                        .read(&mut chunk_buf)
                        .map_err(|e| ExtractionError::Io {
                            entry: entry.name.clone(),
                            source: e,
                        })?;
                    if n == 0 {
                        break;
                    }

                    // Zip bomb check: abort if decompressed stream exceeds metadata uncompressed size
                    if total_uncompressed_bytes + n as u64 > entry.uncompressed_size {
                        return Err(ExtractionError::SizeMismatch {
                            entry: entry.name.clone(),
                            expected: entry.uncompressed_size,
                            actual: total_uncompressed_bytes + n as u64,
                        });
                    }

                    hasher.update(&chunk_buf[..n]);
                    sparse_writer.write_chunk(&chunk_buf[..n]).map_err(|e| {
                        ExtractionError::Io {
                            entry: entry.name.clone(),
                            source: e,
                        }
                    })?;
                    total_uncompressed_bytes += n as u64;
                }
            }
            CompressionMethod::Unsupported(m) => {
                return Err(ExtractionError::UnsupportedCompression(
                    CompressionMethod::Unsupported(m),
                ));
            }
        }

        // Verify uncompressed size
        if total_uncompressed_bytes != entry.uncompressed_size {
            return Err(ExtractionError::SizeMismatch {
                entry: entry.name.clone(),
                expected: entry.uncompressed_size,
                actual: total_uncompressed_bytes,
            });
        }

        // Verify CRC-32
        let computed_crc = hasher.finalize();
        if computed_crc != entry.crc32 {
            return Err(ExtractionError::CrcMismatch {
                entry: entry.name.clone(),
                expected: entry.crc32,
                actual: computed_crc,
            });
        }

        // Finalize sparse writer
        let sparse_saved =
            sparse_writer
                .finish(entry.uncompressed_size)
                .map_err(|e| ExtractionError::Io {
                    entry: entry.name.clone(),
                    source: e,
                })?;

        Ok((total_uncompressed_bytes, sparse_saved))
    }

    fn read_entry_bytes(
        archive_file: &mut File,
        entry: &ZipEntryMetadata,
    ) -> Result<Vec<u8>, ExtractionError> {
        archive_file
            .seek(SeekFrom::Start(entry.data_offset))
            .map_err(|e| ExtractionError::Io {
                entry: entry.name.clone(),
                source: e,
            })?;

        let mut data = Vec::with_capacity(entry.uncompressed_size as usize);
        match entry.compression_method {
            CompressionMethod::Stored => {
                let mut reader = archive_file.take(entry.compressed_size);
                reader
                    .read_to_end(&mut data)
                    .map_err(|e| ExtractionError::Io {
                        entry: entry.name.clone(),
                        source: e,
                    })?;
            }
            CompressionMethod::Deflated => {
                let limited = archive_file.take(entry.compressed_size);
                let buf_reader = BufReader::with_capacity(4096, limited);
                let mut decoder = DeflateDecoder::new(buf_reader);
                decoder
                    .read_to_end(&mut data)
                    .map_err(|e| ExtractionError::Io {
                        entry: entry.name.clone(),
                        source: e,
                    })?;
            }
            CompressionMethod::Unsupported(m) => {
                return Err(ExtractionError::UnsupportedCompression(
                    CompressionMethod::Unsupported(m),
                ));
            }
        }

        if data.len() as u64 != entry.uncompressed_size {
            return Err(ExtractionError::SizeMismatch {
                entry: entry.name.clone(),
                expected: entry.uncompressed_size,
                actual: data.len() as u64,
            });
        }

        let mut hasher = Hasher::new();
        hasher.update(&data);
        let computed_crc = hasher.finalize();
        if computed_crc != entry.crc32 {
            return Err(ExtractionError::CrcMismatch {
                entry: entry.name.clone(),
                expected: entry.crc32,
                actual: computed_crc,
            });
        }

        Ok(data)
    }
}

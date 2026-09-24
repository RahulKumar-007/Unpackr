use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use super::entry::{CompressionMethod, EntryState, ZipEntryMetadata};
use super::identity::compute_archive_identity;

pub const SIGNATURE_LOCAL_FILE_HEADER: u32 = 0x04034b50;
pub const SIGNATURE_CENTRAL_DIRECTORY: u32 = 0x02014b50;
pub const SIGNATURE_ZIP64_EOCD_RECORD: u32 = 0x06064b50;
pub const SIGNATURE_ZIP64_EOCD_LOCATOR: u32 = 0x07064b50;
pub const SIGNATURE_EOCD: u32 = 0x06054b50;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ZipArchiveInspection {
    pub path: PathBuf,
    pub file_size: u64,
    pub identity: String,
    pub total_entries: usize,
    pub total_uncompressed_size: u64,
    pub total_compressed_size: u64,
    pub central_directory_offset: u64,
    pub central_directory_size: u64,
    pub is_zip64: bool,
    pub has_overlapping_entries: bool,
    pub entries: Vec<ZipEntryMetadata>,
}

pub struct ZipInspector;

impl ZipInspector {
    /// Inspects an archive on disk completely without loading entry data into memory.
    /// Streams headers and computes exact offsets, sizes, and metadata.
    pub fn inspect(path: &Path) -> Result<ZipArchiveInspection> {
        let mut file = File::open(path)
            .with_context(|| format!("Failed to open archive at {:?}", path))?;
        let file_size = file.metadata()?.len();

        if file_size < 22 {
            bail!("File is too small to be a valid ZIP archive (size: {} bytes)", file_size);
        }

        let identity = compute_archive_identity(path)?;

        // 1. Locate and parse EOCD
        let eocd_info = Self::find_and_parse_eocd(&mut file, file_size)?;

        // 2. Check for Zip64
        let (cd_offset, cd_size, total_entries, is_zip64) =
            Self::check_zip64(&mut file, file_size, &eocd_info)?;

        // 3. Parse Central Directory headers
        let mut entries = Self::parse_central_directory(
            &mut file,
            cd_offset,
            total_entries,
        )?;

        // 4. Inspect Local File Headers to find exact data offsets and validate boundaries
        for entry in &mut entries {
            if entry.local_header_offset >= cd_offset {
                bail!(
                    "Corrupt archive: entry '{}' local header offset ({}) is at or past central directory offset ({})",
                    entry.name,
                    entry.local_header_offset,
                    cd_offset
                );
            }

            let data_offset = Self::resolve_data_offset(&mut file, entry.local_header_offset)?;
            entry.data_offset = data_offset;

            if entry.data_end_offset() > cd_offset {
                bail!(
                    "Corrupt archive: entry '{}' compressed data range ends at {}, exceeding central directory offset ({})",
                    entry.name,
                    entry.data_end_offset(),
                    cd_offset
                );
            }
        }

        // 5. Detect overlapping payload ranges (e.g. Fifield non-linear zip bomb)
        let has_overlapping_entries = Self::detect_overlapping_entries(&entries);

        let total_uncompressed_size = entries.iter().map(|e| e.uncompressed_size).sum();
        let total_compressed_size = entries.iter().map(|e| e.compressed_size).sum();

        Ok(ZipArchiveInspection {
            path: path.to_path_buf(),
            file_size,
            identity,
            total_entries: entries.len(),
            total_uncompressed_size,
            total_compressed_size,
            central_directory_offset: cd_offset,
            central_directory_size: cd_size,
            is_zip64,
            has_overlapping_entries,
            entries,
        })
    }

    /// Detects if any distinct entries in the archive share overlapping compressed data intervals.
    /// Overlapping intervals indicate non-standard archives or Fifield-style zip bombs.
    pub fn detect_overlapping_entries(entries: &[ZipEntryMetadata]) -> bool {
        let mut intervals: Vec<(u64, u64)> = entries
            .iter()
            .filter(|e| e.compressed_size > 0)
            .map(|e| (e.data_offset, e.data_end_offset()))
            .collect();

        if intervals.len() <= 1 {
            return false;
        }

        intervals.sort_unstable_by_key(|&(start, _)| start);

        for i in 0..intervals.len() - 1 {
            let (_, end_prev) = intervals[i];
            let (start_curr, _) = intervals[i + 1];
            if start_curr < end_prev {
                return true;
            }
        }

        false
    }
}

struct EocdRecord {
    offset_in_file: u64,
    total_entries: u16,
    cd_size: u32,
    cd_offset: u32,
}

impl ZipInspector {
    fn find_and_parse_eocd(file: &mut File, file_size: u64) -> Result<EocdRecord> {
        // EOCD is at least 22 bytes, comment can be up to 65535 bytes
        let max_search = (file_size.min(65535 + 22)) as usize;
        let mut buf = vec![0u8; max_search];
        let search_start = file_size - max_search as u64;
        file.seek(SeekFrom::Start(search_start))?;
        file.read_exact(&mut buf)?;

        // Search backwards for signature 0x06054b50 (PK\x05\x06)
        let sig = [0x50, 0x4b, 0x05, 0x06];
        let mut eocd_pos = None;

        for i in (0..=max_search.saturating_sub(22)).rev() {
            if buf[i..i + 4] == sig {
                let comment_len = u16::from_le_bytes([buf[i + 20], buf[i + 21]]) as usize;
                if i + 22 + comment_len <= max_search {
                    eocd_pos = Some(i);
                    break;
                }
            }
        }

        let pos = match eocd_pos {
            Some(p) => p,
            None => bail!("End of Central Directory (EOCD) signature not found in archive"),
        };

        let offset_in_file = search_start + pos as u64;
        let total_entries = u16::from_le_bytes([buf[pos + 10], buf[pos + 11]]);
        let cd_size = u32::from_le_bytes([
            buf[pos + 12],
            buf[pos + 13],
            buf[pos + 14],
            buf[pos + 15],
        ]);
        let cd_offset = u32::from_le_bytes([
            buf[pos + 16],
            buf[pos + 17],
            buf[pos + 18],
            buf[pos + 19],
        ]);

        Ok(EocdRecord {
            offset_in_file,
            total_entries,
            cd_size,
            cd_offset,
        })
    }

    fn check_zip64(
        file: &mut File,
        file_size: u64,
        eocd: &EocdRecord,
    ) -> Result<(u64, u64, u64, bool)> {
        // Zip64 EOCD Locator is 20 bytes preceding the standard EOCD
        if eocd.offset_in_file >= 20 {
            let locator_offset = eocd.offset_in_file - 20;
            file.seek(SeekFrom::Start(locator_offset))?;
            let mut loc_buf = [0u8; 20];
            if file.read_exact(&mut loc_buf).is_ok() {
                let sig = u32::from_le_bytes([loc_buf[0], loc_buf[1], loc_buf[2], loc_buf[3]]);
                if sig == SIGNATURE_ZIP64_EOCD_LOCATOR {
                    let zip64_eocd_offset = u64::from_le_bytes([
                        loc_buf[8], loc_buf[9], loc_buf[10], loc_buf[11],
                        loc_buf[12], loc_buf[13], loc_buf[14], loc_buf[15],
                    ]);

                    if zip64_eocd_offset < file_size {
                        file.seek(SeekFrom::Start(zip64_eocd_offset))?;
                        let mut rec_buf = [0u8; 56];
                        if file.read_exact(&mut rec_buf).is_ok() {
                            let rec_sig = u32::from_le_bytes([
                                rec_buf[0], rec_buf[1], rec_buf[2], rec_buf[3],
                            ]);
                            if rec_sig == SIGNATURE_ZIP64_EOCD_RECORD {
                                let total_entries = u64::from_le_bytes([
                                    rec_buf[32], rec_buf[33], rec_buf[34], rec_buf[35],
                                    rec_buf[36], rec_buf[37], rec_buf[38], rec_buf[39],
                                ]);
                                let cd_size = u64::from_le_bytes([
                                    rec_buf[40], rec_buf[41], rec_buf[42], rec_buf[43],
                                    rec_buf[44], rec_buf[45], rec_buf[46], rec_buf[47],
                                ]);
                                let cd_offset = u64::from_le_bytes([
                                    rec_buf[48], rec_buf[49], rec_buf[50], rec_buf[51],
                                    rec_buf[52], rec_buf[53], rec_buf[54], rec_buf[55],
                                ]);
                                return Ok((cd_offset, cd_size, total_entries, true));
                            }
                        }
                    }
                }
            }
        }

        // Standard 32-bit ZIP
        Ok((
            eocd.cd_offset as u64,
            eocd.cd_size as u64,
            eocd.total_entries as u64,
            false,
        ))
    }

    fn parse_central_directory(
        file: &mut File,
        cd_offset: u64,
        total_entries: u64,
    ) -> Result<Vec<ZipEntryMetadata>> {
        file.seek(SeekFrom::Start(cd_offset))?;
        let mut reader = BufReader::with_capacity(65536, file);
        let mut entries = Vec::with_capacity(total_entries.min(100_000) as usize);

        for idx in 0..total_entries {
            let mut header = [0u8; 46];
            if let Err(e) = reader.read_exact(&mut header) {
                bail!("Failed to read central directory entry {}: {}", idx, e);
            }

            let sig = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
            if sig != SIGNATURE_CENTRAL_DIRECTORY {
                bail!(
                    "Invalid central directory signature at entry {}: 0x{:08X}",
                    idx,
                    sig
                );
            }

            let method_raw = u16::from_le_bytes([header[10], header[11]]);
            let compression_method = CompressionMethod::from_u16(method_raw);
            let crc32 = u32::from_le_bytes([header[16], header[17], header[18], header[19]]);
            let mut comp_size = u32::from_le_bytes([
                header[20], header[21], header[22], header[23],
            ]) as u64;
            let mut uncomp_size = u32::from_le_bytes([
                header[24], header[25], header[26], header[27],
            ]) as u64;
            let name_len = u16::from_le_bytes([header[28], header[29]]) as usize;
            let extra_len = u16::from_le_bytes([header[30], header[31]]) as usize;
            let comment_len = u16::from_le_bytes([header[32], header[33]]) as usize;
            let external_attrs = u32::from_le_bytes([
                header[38], header[39], header[40], header[41],
            ]);
            let mut local_header_offset = u32::from_le_bytes([
                header[42], header[43], header[44], header[45],
            ]) as u64;

            // Read variable length data
            let mut name_buf = vec![0u8; name_len];
            reader.read_exact(&mut name_buf)?;
            let name = String::from_utf8_lossy(&name_buf).to_string();

            let mut extra_buf = vec![0u8; extra_len];
            reader.read_exact(&mut extra_buf)?;

            // Skip comment
            if comment_len > 0 {
                let mut comment_buf = vec![0u8; comment_len];
                reader.read_exact(&mut comment_buf)?;
            }

            // Parse Zip64 Extra Field (Tag 0x0001) if present
            Self::parse_zip64_extra(
                &extra_buf,
                &mut uncomp_size,
                &mut comp_size,
                &mut local_header_offset,
            );

            let is_dir = name.ends_with('/') || (external_attrs & 0x10 != 0);
            // Unix file mode symlink test: S_IFLNK is 0o120000
            let is_symlink = ((external_attrs >> 16) & 0o170000) == 0o120000;

            entries.push(ZipEntryMetadata {
                index: idx as usize,
                name,
                compressed_size: comp_size,
                uncompressed_size: uncomp_size,
                compression_method,
                crc32,
                local_header_offset,
                data_offset: 0, // Resolved in step 4
                is_dir,
                is_symlink,
                external_attributes: external_attrs,
                state: EntryState::Pending,
            });
        }

        Ok(entries)
    }

    fn parse_zip64_extra(
        extra_buf: &[u8],
        uncomp_size: &mut u64,
        comp_size: &mut u64,
        local_header_offset: &mut u64,
    ) {
        let mut pos = 0;
        while pos + 4 <= extra_buf.len() {
            let tag = u16::from_le_bytes([extra_buf[pos], extra_buf[pos + 1]]);
            let block_size = u16::from_le_bytes([extra_buf[pos + 2], extra_buf[pos + 3]]) as usize;
            pos += 4;

            if pos + block_size > extra_buf.len() {
                break;
            }

            if tag == 0x0001 {
                let mut data_pos = pos;
                let end = pos + block_size;

                if *uncomp_size == 0xFFFFFFFF && data_pos + 8 <= end {
                    *uncomp_size = u64::from_le_bytes([
                        extra_buf[data_pos], extra_buf[data_pos + 1],
                        extra_buf[data_pos + 2], extra_buf[data_pos + 3],
                        extra_buf[data_pos + 4], extra_buf[data_pos + 5],
                        extra_buf[data_pos + 6], extra_buf[data_pos + 7],
                    ]);
                    data_pos += 8;
                }

                if *comp_size == 0xFFFFFFFF && data_pos + 8 <= end {
                    *comp_size = u64::from_le_bytes([
                        extra_buf[data_pos], extra_buf[data_pos + 1],
                        extra_buf[data_pos + 2], extra_buf[data_pos + 3],
                        extra_buf[data_pos + 4], extra_buf[data_pos + 5],
                        extra_buf[data_pos + 6], extra_buf[data_pos + 7],
                    ]);
                    data_pos += 8;
                }

                if *local_header_offset == 0xFFFFFFFF && data_pos + 8 <= end {
                    *local_header_offset = u64::from_le_bytes([
                        extra_buf[data_pos], extra_buf[data_pos + 1],
                        extra_buf[data_pos + 2], extra_buf[data_pos + 3],
                        extra_buf[data_pos + 4], extra_buf[data_pos + 5],
                        extra_buf[data_pos + 6], extra_buf[data_pos + 7],
                    ]);
                }
            }

            pos += block_size;
        }
    }

    /// Resolves the exact data offset by inspecting the Local File Header at `local_header_offset`.
    /// Data begins immediately after: 30 bytes + local_file_name_len + local_extra_field_len.
    pub fn resolve_data_offset(file: &mut File, local_header_offset: u64) -> Result<u64> {
        file.seek(SeekFrom::Start(local_header_offset))?;
        let mut header = [0u8; 30];
        file.read_exact(&mut header)
            .with_context(|| format!("Failed to read Local File Header at offset {}", local_header_offset))?;

        let sig = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
        if sig != SIGNATURE_LOCAL_FILE_HEADER {
            bail!(
                "Invalid Local File Header signature at offset {}: 0x{:08X}",
                local_header_offset,
                sig
            );
        }

        let name_len = u16::from_le_bytes([header[26], header[27]]) as u64;
        let extra_len = u16::from_le_bytes([header[28], header[29]]) as u64;

        Ok(local_header_offset + 30 + name_len + extra_len)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_empty_or_tiny_file_fails_cleanly() {
        let mut tmp = NamedTempFile::new().unwrap();
        tmp.write_all(b"not a zip").unwrap();
        tmp.flush().unwrap();

        let result = ZipInspector::inspect(tmp.path());
        assert!(result.is_err());
    }
}

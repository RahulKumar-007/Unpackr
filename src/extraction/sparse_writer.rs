use std::fs::File;
use std::io::{Result, Seek, SeekFrom, Write};

pub const BLOCK_SIZE: usize = 4096;

/// A bounded streaming writer that detects contiguous 4096-byte blocks of zeroes
/// and produces sparse files by seeking instead of writing zeroed blocks to disk.
pub struct SparseWriter {
    file: File,
    enable_sparse: bool,
    buffer: Vec<u8>,
    sparse_bytes_saved: u64,
}

impl SparseWriter {
    pub fn new(file: File, enable_sparse: bool) -> Self {
        Self {
            file,
            enable_sparse,
            buffer: Vec::with_capacity(BLOCK_SIZE),
            sparse_bytes_saved: 0,
        }
    }

    /// Fast safe SWAR check if a 4096-byte slice is completely zeroes
    #[inline]
    fn is_all_zero(slice: &[u8]) -> bool {
        let (chunks, remainder) = slice.as_chunks::<8>();
        chunks.iter().all(|c| u64::from_ne_bytes(*c) == 0) && remainder.iter().all(|&b| b == 0)
    }

    pub fn write_chunk(&mut self, data: &[u8]) -> Result<()> {
        let mut offset = 0;
        let len = data.len();

        while offset < len {
            // Fill current block buffer
            let needed = BLOCK_SIZE - self.buffer.len();
            let available = len - offset;
            let to_copy = needed.min(available);

            self.buffer
                .extend_from_slice(&data[offset..offset + to_copy]);
            offset += to_copy;

            if self.buffer.len() == BLOCK_SIZE {
                self.flush_block()?;
            }
        }

        Ok(())
    }

    fn flush_block(&mut self) -> Result<()> {
        if self.buffer.is_empty() {
            return Ok(());
        }

        if self.enable_sparse && self.buffer.len() == BLOCK_SIZE && Self::is_all_zero(&self.buffer)
        {
            // Advance seek position without writing physical data blocks
            self.file.seek(SeekFrom::Current(BLOCK_SIZE as i64))?;
            self.sparse_bytes_saved += BLOCK_SIZE as u64;
        } else {
            self.file.write_all(&self.buffer)?;
        }

        self.buffer.clear();
        Ok(())
    }

    /// Flushes any remaining bytes, sets exact file length, and syncs to disk.
    /// Returns total sparse bytes saved (bytes skipped from physical allocation).
    pub fn finish(mut self, expected_len: u64) -> Result<u64> {
        if !self.buffer.is_empty() {
            self.file.write_all(&self.buffer)?;
            self.buffer.clear();
        }

        // Ensure logical length matches expected length even if trailing blocks were sparse
        self.file.set_len(expected_len)?;
        self.file.flush()?;
        let _ = self.file.sync_data();

        Ok(self.sparse_bytes_saved)
    }

    pub fn sparse_bytes_saved(&self) -> u64 {
        self.sparse_bytes_saved
    }
}

#[cfg(debug_assertions)]
impl Drop for SparseWriter {
    fn drop(&mut self) {
        if !self.buffer.is_empty() && !std::thread::panicking() {
            eprintln!(
                "Warning: SparseWriter dropped with {} unflushed bytes in buffer",
                self.buffer.len()
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;
    use tempfile::NamedTempFile;

    #[test]
    fn test_sparse_writer_data_integrity() {
        let tmp = NamedTempFile::new().unwrap();
        let file = tmp.reopen().unwrap();

        let mut writer = SparseWriter::new(file, true);

        // 1 block of data, 2 blocks of zeroes, 1 block of data
        let mut data = Vec::new();
        data.extend_from_slice(&[0x41; 4096]);
        data.extend_from_slice(&[0x00; 8192]);
        data.extend_from_slice(&[0x42; 4096]);

        writer.write_chunk(&data).unwrap();
        assert_eq!(writer.sparse_bytes_saved(), 8192);
        let saved = writer.finish(data.len() as u64).unwrap();

        assert_eq!(saved, 8192);

        // Verify logical contents match exactly
        let mut read_file = File::open(tmp.path()).unwrap();
        let mut read_buf = Vec::new();
        read_file.read_to_end(&mut read_buf).unwrap();
        assert_eq!(read_buf, data);
    }
}

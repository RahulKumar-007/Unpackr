use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use tempfile::NamedTempFile;
use unpackr::archive::{CompressionMethod, ZipInspector};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn test_inspect_stored_and_deflated_entries() {
    let tmp_file = NamedTempFile::new().unwrap();
    let path = tmp_file.path().to_path_buf();

    // Create a test zip file
    {
        let file = File::create(&path).unwrap();
        let mut zip = ZipWriter::new(file);

        // Entry 1: Stored
        let stored_opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file("stored.txt", stored_opts).unwrap();
        zip.write_all(b"Hello Stored World!").unwrap();

        // Entry 2: Deflated
        let deflated_opts =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("nested/deflated.txt", deflated_opts)
            .unwrap();
        zip.write_all(b"A quick brown fox jumps over the lazy dog. A quick brown fox jumps over the lazy dog.").unwrap();

        // Entry 3: Directory
        zip.add_directory("nested/subdir/", SimpleFileOptions::default())
            .unwrap();

        zip.finish().unwrap();
    }

    let inspection = ZipInspector::inspect(&path).expect("Failed to inspect zip");

    assert_eq!(inspection.total_entries, 3);
    assert!(!inspection.identity.is_empty());
    assert!(inspection.file_size > 0);
    assert!(!inspection.is_zip64);

    // Verify Entry 0: stored.txt
    let entry0 = &inspection.entries[0];
    assert_eq!(entry0.name, "stored.txt");
    assert_eq!(entry0.compression_method, CompressionMethod::Stored);
    assert_eq!(entry0.uncompressed_size, 19);
    assert_eq!(entry0.compressed_size, 19);
    assert!(!entry0.is_dir);
    assert!(entry0.data_offset > entry0.local_header_offset);

    // Read directly from data_offset to verify data_offset accuracy
    let mut file = File::open(&path).unwrap();
    file.seek(SeekFrom::Start(entry0.data_offset)).unwrap();
    let mut buf = vec![0u8; entry0.compressed_size as usize];
    file.read_exact(&mut buf).unwrap();
    assert_eq!(&buf, b"Hello Stored World!");

    // Verify Entry 1: nested/deflated.txt
    let entry1 = &inspection.entries[1];
    assert_eq!(entry1.name, "nested/deflated.txt");
    assert_eq!(entry1.compression_method, CompressionMethod::Deflated);
    assert_eq!(entry1.uncompressed_size, 85);
    assert!(entry1.compressed_size < entry1.uncompressed_size);
    assert!(entry1.data_offset > entry1.local_header_offset);

    // Verify Entry 2: nested/subdir/
    let entry2 = &inspection.entries[2];
    assert_eq!(entry2.name, "nested/subdir/");
    assert!(entry2.is_dir);
    assert_eq!(entry2.uncompressed_size, 0);
}

#[test]
fn test_inspect_corrupt_file() {
    let mut tmp_file = NamedTempFile::new().unwrap();
    tmp_file
        .write_all(b"corrupt non-zip data of arbitrary bytes")
        .unwrap();
    tmp_file.flush().unwrap();

    let result = ZipInspector::inspect(tmp_file.path());
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("signature not found") || err_msg.contains("too small"));
}

#[test]
fn test_archive_identity_consistency() {
    let tmp_file = NamedTempFile::new().unwrap();
    let path = tmp_file.path().to_path_buf();

    {
        let file = File::create(&path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("sample.txt", SimpleFileOptions::default())
            .unwrap();
        zip.write_all(b"constant content").unwrap();
        zip.finish().unwrap();
    }

    let id1 = unpackr::archive::compute_archive_identity(&path).unwrap();
    let id2 = unpackr::archive::compute_archive_identity(&path).unwrap();
    assert_eq!(id1, id2);
}

#[test]
fn test_inspect_zip_slip_sample() {
    let path = std::path::Path::new("tests/test_data/zip_slip.zip");
    if path.exists() {
        let inspection = ZipInspector::inspect(path).expect("Failed to inspect zip slip archive");
        assert_eq!(inspection.total_entries, 2);
        let entry0 = &inspection.entries[0];
        assert_eq!(entry0.name, "../../etc/passwd");

        // Verify that path sanitizer catches the slip
        let sanitize_result = unpackr::security::path::sanitize_entry_path(&entry0.name);
        assert!(sanitize_result.is_err());
    }
}

#[test]
fn test_inspect_zip64_archive() {
    let tmp_file = NamedTempFile::new().unwrap();
    let path = tmp_file.path().to_path_buf();

    let mut data = Vec::new();
    // 1. Local File Header
    let lfh_offset = data.len() as u64;
    data.extend_from_slice(&0x04034b50u32.to_le_bytes());
    data.extend_from_slice(&45u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    let payload = b"ZIP64 payload test content";
    let crc = crc32fast::hash(payload);
    data.extend_from_slice(&crc.to_le_bytes());
    data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    let filename = b"zip64_test.txt";
    data.extend_from_slice(&(filename.len() as u16).to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(filename);
    data.extend_from_slice(payload);

    // 2. Central Directory Header
    let cd_offset = data.len() as u64;
    data.extend_from_slice(&0x02014b50u32.to_le_bytes());
    data.extend_from_slice(&45u16.to_le_bytes());
    data.extend_from_slice(&45u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&crc.to_le_bytes());
    data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    data.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    data.extend_from_slice(&(filename.len() as u16).to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0o100644u32.to_le_bytes());
    data.extend_from_slice(&(lfh_offset as u32).to_le_bytes());
    data.extend_from_slice(filename);
    let cd_size = (data.len() as u64) - cd_offset;

    // 3. ZIP64 End of Central Directory Record (56 bytes)
    let zip64_eocd_offset = data.len() as u64;
    data.extend_from_slice(&0x06064b50u32.to_le_bytes());
    data.extend_from_slice(&44u64.to_le_bytes());
    data.extend_from_slice(&45u16.to_le_bytes());
    data.extend_from_slice(&45u16.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());
    data.extend_from_slice(&1u64.to_le_bytes());
    data.extend_from_slice(&cd_size.to_le_bytes());
    data.extend_from_slice(&cd_offset.to_le_bytes());

    // 4. ZIP64 End of Central Directory Locator (20 bytes)
    data.extend_from_slice(&0x07064b50u32.to_le_bytes());
    data.extend_from_slice(&0u32.to_le_bytes());
    data.extend_from_slice(&zip64_eocd_offset.to_le_bytes());
    data.extend_from_slice(&1u32.to_le_bytes());

    // 5. Standard EOCD (22 bytes)
    data.extend_from_slice(&0x06054b50u32.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());
    data.extend_from_slice(&0xFFFFu16.to_le_bytes());
    data.extend_from_slice(&0xFFFFu16.to_le_bytes());
    data.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes());
    data.extend_from_slice(&0xFFFFFFFFu32.to_le_bytes());
    data.extend_from_slice(&0u16.to_le_bytes());

    std::fs::write(&path, &data).unwrap();

    let inspection = ZipInspector::inspect(&path).expect("Failed to inspect zip64 archive");
    assert!(inspection.is_zip64);
    assert_eq!(inspection.total_entries, 1);
    assert_eq!(inspection.entries[0].name, "zip64_test.txt");
    assert_eq!(
        inspection.entries[0].uncompressed_size,
        payload.len() as u64
    );
}

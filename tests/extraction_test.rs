use std::fs::{self, File};
use std::io::{Read, Write};
use tempfile::{tempdir, NamedTempFile};
use unpackr::extraction::{CollisionPolicy, ExtractionEngine, ExtractionError, ExtractionOptions};
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn test_streaming_extraction_accuracy() {
    let dest_dir = tempdir().unwrap();
    let archive_path = std::path::Path::new("tests/test_data/valid_sample.zip");

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 200.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: true,
    };

    let summary = ExtractionEngine::extract(archive_path, &options).expect("Extraction failed");

    assert_eq!(summary.total_entries, 5);
    assert_eq!(summary.extracted_files, 4);
    assert_eq!(summary.created_directories, 1);

    // Verify plain.txt
    let plain_path = dest_dir.path().join("plain.txt");
    assert!(plain_path.exists());
    let mut plain_content = String::new();
    File::open(&plain_path).unwrap().read_to_string(&mut plain_content).unwrap();
    assert_eq!(plain_content, "This is uncompressed text stored as is.");

    // Verify nested/data.txt
    let nested_path = dest_dir.path().join("nested/data.txt");
    assert!(nested_path.exists());
    let mut nested_content = Vec::new();
    File::open(&nested_path).unwrap().read_to_end(&mut nested_content).unwrap();
    assert_eq!(nested_content.len(), 14000);
    assert_eq!(&nested_content[..28], b"Decompression testing data! ");

    // Verify empty.dat
    let empty_path = dest_dir.path().join("empty.dat");
    assert!(empty_path.exists());
    assert_eq!(fs::metadata(&empty_path).unwrap().len(), 0);

    // Verify logs/ directory
    let logs_path = dest_dir.path().join("logs");
    assert!(logs_path.is_dir());
}

#[test]
fn test_sparse_hole_extraction() {
    let dest_dir = tempdir().unwrap();
    let zip_file = NamedTempFile::new().unwrap();

    // Create a zip with 1 MB file containing 512 KB of zeroes
    {
        let file = File::create(zip_file.path()).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("sparse_test.bin", SimpleFileOptions::default()).unwrap();

        let mut payload = Vec::with_capacity(1024 * 1024);
        payload.extend_from_slice(&[0xAA; 256 * 1024]);
        payload.extend_from_slice(&[0x00; 512 * 1024]); // 512 KB zeroes
        payload.extend_from_slice(&[0xBB; 256 * 1024]);

        zip.write_all(&payload).unwrap();
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 2000.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
    };

    let summary = ExtractionEngine::extract(zip_file.path(), &options).unwrap();
    assert!(summary.sparse_bytes_saved >= 512 * 1024);

    // Verify logical integrity of the sparse file
    let extracted_path = dest_dir.path().join("sparse_test.bin");
    assert_eq!(fs::metadata(&extracted_path).unwrap().len(), 1024 * 1024);

    let mut read_data = Vec::new();
    File::open(&extracted_path).unwrap().read_to_end(&mut read_data).unwrap();
    assert_eq!(&read_data[..256 * 1024], &[0xAA; 256 * 1024]);
    assert_eq!(&read_data[256 * 1024..768 * 1024], &[0x00; 512 * 1024]);
    assert_eq!(&read_data[768 * 1024..], &[0xBB; 256 * 1024]);
}

#[test]
fn test_zip_slip_rejection_at_extraction() {
    let dest_dir = tempdir().unwrap();
    let archive_path = std::path::Path::new("tests/test_data/zip_slip.zip");

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
    };

    let res = ExtractionEngine::extract(archive_path, &options);
    assert!(res.is_err());
    let err = res.unwrap_err();
    assert!(matches!(err, ExtractionError::Security { .. }));

    // Verify no file was created outside destination
    assert!(!dest_dir.path().parent().unwrap().join("etc/passwd").exists());
}

#[test]
fn test_compression_bomb_rejection() {
    let dest_dir = tempdir().unwrap();
    let zip_file = NamedTempFile::new().unwrap();

    // Create a 1 MB file of zeroes that compresses to ~1 KB (>500:1 ratio)
    {
        let file = File::create(zip_file.path()).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file(
            "bomb.txt",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&vec![0x00; 15 * 1024 * 1024]).unwrap();
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 10.0, // Low threshold
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
    };

    let res = ExtractionEngine::extract(zip_file.path(), &options);
    assert!(res.is_err());
    assert!(matches!(
        res.unwrap_err(),
        ExtractionError::SuspiciousCompressionRatio { .. }
    ));
}

#[test]
fn test_collision_policies_at_engine_level() {
    let dest_dir = tempdir().unwrap();
    let zip_file = NamedTempFile::new().unwrap();

    {
        let file = File::create(zip_file.path()).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("test.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"new content").unwrap();
        zip.finish().unwrap();
    }

    let target_file = dest_dir.path().join("test.txt");
    fs::write(&target_file, b"existing content").unwrap();

    // Policy: Fail
    let mut options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
    };
    assert!(ExtractionEngine::extract(zip_file.path(), &options).is_err());

    // Policy: Skip
    options.collision_policy = CollisionPolicy::Skip;
    let summary = ExtractionEngine::extract(zip_file.path(), &options).unwrap();
    assert_eq!(summary.skipped_files, 1);
    assert_eq!(fs::read(&target_file).unwrap(), b"existing content");

    // Policy: Overwrite
    options.collision_policy = CollisionPolicy::Overwrite;
    let summary = ExtractionEngine::extract(zip_file.path(), &options).unwrap();
    assert_eq!(summary.extracted_files, 1);
    assert_eq!(fs::read(&target_file).unwrap(), b"new content");

    // Policy: Rename
    options.collision_policy = CollisionPolicy::Rename;
    let summary = ExtractionEngine::extract(zip_file.path(), &options).unwrap();
    assert_eq!(summary.extracted_files, 1);
    let renamed = dest_dir.path().join("test.1.txt");
    assert!(renamed.exists());
    assert_eq!(fs::read(&renamed).unwrap(), b"new content");
}

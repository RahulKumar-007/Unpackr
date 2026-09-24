use std::fs::{self, File};
use std::io::Write;
use tempfile::tempdir;
use unpackr::archive::ZipInspector;
use unpackr::extraction::{CollisionPolicy, ExtractionEngine, ExtractionError, ExtractionOptions};
use unpackr::security::PathSecurityError;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn test_symlink_traversal_poisoning_attack() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    fs::create_dir_all(&dest_dir).unwrap();

    let outside_dir = base_dir.path().join("outside");
    fs::create_dir_all(&outside_dir).unwrap();

    // Create symlink: dest/evil_dir -> outside_dir
    let symlink_path = dest_dir.join("evil_dir");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside_dir, &symlink_path).unwrap();

    // Create archive containing entry: evil_dir/target.txt
    let archive_path = base_dir.path().join("poison.zip");
    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("evil_dir/target.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"malicious payload intended to escape dest").unwrap();
        zip.finish().unwrap();
    }

    #[cfg(unix)]
    {
        let options = ExtractionOptions {
            destination: dest_dir.clone(),
            collision_policy: CollisionPolicy::Fail,
            ..Default::default()
        };

        let result = ExtractionEngine::extract(&archive_path, &options);
        assert!(result.is_err(), "Expected symlink traversal attack to be rejected");
        let err = result.unwrap_err();
        assert!(
            matches!(err, ExtractionError::Security { source: PathSecurityError::SymlinkTraversal(_), .. }),
            "Expected SymlinkTraversal error, got: {:?}",
            err
        );

        // Assert nothing was written to outside_dir
        assert!(!outside_dir.join("target.txt").exists(), "File escaped into outside directory!");
    }
}

#[test]
fn test_reserved_unpackr_path_attack() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    let archive_path = base_dir.path().join("reserved.zip");

    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file(".unpackr/manifest.json", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"forged manifest content").unwrap();
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir,
        ..Default::default()
    };

    let result = ExtractionEngine::extract(&archive_path, &options);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExtractionError::Security { source: PathSecurityError::ReservedPath(_), .. }),
        "Expected ReservedPath error, got: {:?}",
        err
    );
}

#[test]
fn test_windows_reserved_device_names() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    let archive_path = base_dir.path().join("win_reserved.zip");

    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("CON.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"con device data").unwrap();
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir,
        ..Default::default()
    };

    let result = ExtractionEngine::extract(&archive_path, &options);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExtractionError::Security { source: PathSecurityError::ReservedDeviceName(_), .. }),
        "Expected ReservedDeviceName error, got: {:?}",
        err
    );
}

#[test]
fn test_alternate_data_stream_colon_attack() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    let archive_path = base_dir.path().join("ads_colon.zip");

    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("file.txt:hidden", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"hidden stream payload").unwrap();
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir,
        ..Default::default()
    };

    let result = ExtractionEngine::extract(&archive_path, &options);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExtractionError::Security { source: PathSecurityError::InvalidCharacters(_), .. }),
        "Expected InvalidCharacters for colon, got: {:?}",
        err
    );
}

#[test]
fn test_fifield_overlapping_entries_reclaim_rejection() {
    let base_dir = tempdir().unwrap();
    let archive_path = base_dir.path().join("overlapping.zip");
    let dest_dir = base_dir.path().join("dest");

    // Construct a ZIP archive where entry 0 and entry 1 point to identical compressed payload bytes
    // (David Fifield non-linear zip bomb pattern)
    {
        let mut file = File::create(&archive_path).unwrap();
        // Entry 1 Local Header + Data
        let lh_offset = 0u64;
        let mut lh = vec![
            0x50, 0x4B, 0x03, 0x04, // Local header signature
            20, 0,                   // Version needed (2.0)
            0, 0,                    // Flags
            0, 0,                    // Stored (no compression)
            0, 0, 0, 0,              // Time/Date
            0x12, 0x34, 0x56, 0x78,  // CRC-32 (dummy)
            10, 0, 0, 0,             // Compressed size (10)
            10, 0, 0, 0,             // Uncompressed size (10)
            6, 0,                    // Name length (6)
            0, 0,                    // Extra field length (0)
        ];
        lh.extend_from_slice(b"f1.dat");
        lh.extend_from_slice(b"0123456789"); // 10 bytes data
        file.write_all(&lh).unwrap();

        // Central Directory with TWO entries pointing to the EXACT SAME lh_offset
        let cd_offset = file.metadata().unwrap().len();

        for name in &[b"f1.dat", b"f2.dat"] {
            let mut cd = vec![
                0x50, 0x4B, 0x01, 0x02, // Central directory signature
                20, 0,                   // Version made by
                20, 0,                   // Version needed
                0, 0,                    // Flags
                0, 0,                    // Stored
                0, 0, 0, 0,              // Time/Date
                0x12, 0x34, 0x56, 0x78,  // CRC-32
                10, 0, 0, 0,             // Compressed size
                10, 0, 0, 0,             // Uncompressed size
                name.len() as u8, 0,     // Name length
                0, 0,                    // Extra length
                0, 0,                    // Comment length
                0, 0,                    // Disk number
                0, 0,                    // Internal attrs
                0, 0, 0, 0,              // External attrs
            ];
            cd.extend_from_slice(&(lh_offset as u32).to_le_bytes()); // Local header offset!
            cd.extend_from_slice(*name);
            file.write_all(&cd).unwrap();
        }

        let cd_end = file.metadata().unwrap().len();
        let cd_size = cd_end - cd_offset;

        // EOCD
        let mut eocd = vec![
            0x50, 0x4B, 0x05, 0x06, // EOCD signature
            0, 0,                    // Disk number
            0, 0,                    // CD disk
            2, 0,                    // Total entries on disk
            2, 0,                    // Total entries in CD
        ];
        eocd.extend_from_slice(&(cd_size as u32).to_le_bytes());
        eocd.extend_from_slice(&(cd_offset as u32).to_le_bytes());
        eocd.extend_from_slice(&[0, 0]); // Comment length (0)
        file.write_all(&eocd).unwrap();
    }

    // Inspect archive
    let inspection = ZipInspector::inspect(&archive_path).expect("Inspection failed");
    assert!(inspection.has_overlapping_entries, "Inspection must detect overlapping entries");

    // Attempt extraction with reclaim_archive: true
    let options = ExtractionOptions {
        destination: dest_dir,
        reclaim_archive: true,
        ..Default::default()
    };

    let result = ExtractionEngine::extract(&archive_path, &options);
    assert!(result.is_err(), "Reclamation must be rejected on overlapping entries!");
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("overlapping compressed data streams") || err_msg.contains("zip bomb"));
}

#[test]
fn test_special_device_file_rejection() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    let archive_path = base_dir.path().join("fifo_device.zip");

    // Construct raw ZIP with external attributes = 0o010666 << 16 (FIFO)
    {
        let mut file = File::create(&archive_path).unwrap();
        let lh_offset = 0u64;
        let mut lh = vec![
            0x50, 0x4B, 0x03, 0x04, // Local header signature
            20, 0,                   // Version needed (2.0)
            0, 0,                    // Flags
            0, 0,                    // Stored
            0, 0, 0, 0,              // Time/Date
            0x12, 0x34, 0x56, 0x78,  // CRC-32
            4, 0, 0, 0,              // Compressed size
            4, 0, 0, 0,              // Uncompressed size
            10, 0,                   // Name length (10)
            0, 0,                    // Extra length
        ];
        lh.extend_from_slice(b"named_pipe");
        lh.extend_from_slice(b"fifo");
        file.write_all(&lh).unwrap();

        let cd_offset = file.metadata().unwrap().len();
        let mut cd = vec![
            0x50, 0x4B, 0x01, 0x02, // Central directory signature
            0x03, 0x1E,              // Version made by: Unix (3) + version 30
            20, 0,                   // Version needed
            0, 0,                    // Flags
            0, 0,                    // Stored
            0, 0, 0, 0,              // Time/Date
            0x12, 0x34, 0x56, 0x78,  // CRC-32
            4, 0, 0, 0,              // Compressed size
            4, 0, 0, 0,              // Uncompressed size
            10, 0,                   // Name length
            0, 0,                    // Extra length
            0, 0,                    // Comment length
            0, 0,                    // Disk number
            0, 0,                    // Internal attrs
        ];
        // External attrs: S_IFIFO (0o010000 | 0o666) << 16
        cd.extend_from_slice(&(0o010666u32 << 16).to_le_bytes());
        cd.extend_from_slice(&(lh_offset as u32).to_le_bytes());
        cd.extend_from_slice(b"named_pipe");
        file.write_all(&cd).unwrap();

        let cd_end = file.metadata().unwrap().len();
        let cd_size = cd_end - cd_offset;

        let mut eocd = vec![
            0x50, 0x4B, 0x05, 0x06,
            0, 0,
            0, 0,
            1, 0,
            1, 0,
        ];
        eocd.extend_from_slice(&(cd_size as u32).to_le_bytes());
        eocd.extend_from_slice(&(cd_offset as u32).to_le_bytes());
        eocd.extend_from_slice(&[0, 0]);
        file.write_all(&eocd).unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir,
        ..Default::default()
    };

    let result = ExtractionEngine::extract(&archive_path, &options);
    assert!(result.is_err(), "Expected special device to be rejected");
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExtractionError::ForbiddenDeviceType { .. }),
        "Expected ForbiddenDeviceType error for FIFO/device, got: {:?}",
        err
    );
}

#[test]
fn test_suid_sgid_and_world_writable_bit_stripping() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    let archive_path = base_dir.path().join("suid_exploit.zip");

    // Attempt to set SUID + SGID + world-writable: 0o6777
    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        let opts = SimpleFileOptions::default().unix_permissions(0o6777);
        zip.start_file("exploit_bin", opts).unwrap();
        zip.write_all(b"binary content").unwrap();
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir.clone(),
        ..Default::default()
    };

    ExtractionEngine::extract(&archive_path, &options).expect("Extraction failed");

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(dest_dir.join("exploit_bin")).unwrap();
        let mode = meta.permissions().mode();
        assert_eq!(mode & 0o4000, 0, "SUID bit must be stripped!");
        assert_eq!(mode & 0o2000, 0, "SGID bit must be stripped!");
        assert_eq!(mode & 0o0002, 0, "World-writable bit must be stripped!");
    }
}

#[test]
fn test_resource_limits_max_entries() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    let archive_path = base_dir.path().join("ten_files.zip");

    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        for i in 0..10 {
            zip.start_file(format!("file_{}.txt", i), SimpleFileOptions::default()).unwrap();
            zip.write_all(b"abc").unwrap();
        }
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir,
        max_entries: Some(5), // Limit to 5 entries max
        ..Default::default()
    };

    let result = ExtractionEngine::extract(&archive_path, &options);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExtractionError::ResourceLimitExceeded(_)),
        "Expected ResourceLimitExceeded error, got: {:?}",
        err
    );
}

#[test]
fn test_resource_limits_max_total_size() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    let archive_path = base_dir.path().join("large_total.zip");

    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("big.dat", SimpleFileOptions::default()).unwrap();
        zip.write_all(&vec![0x42; 2 * 1024 * 1024]).unwrap(); // 2 MB
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir,
        max_total_size: Some(1024 * 1024), // Limit total extraction to 1 MB
        ..Default::default()
    };

    let result = ExtractionEngine::extract(&archive_path, &options);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExtractionError::ResourceLimitExceeded(_)),
        "Expected ResourceLimitExceeded error, got: {:?}",
        err
    );
}

#[test]
fn test_resource_limits_max_file_size() {
    let base_dir = tempdir().unwrap();
    let dest_dir = base_dir.path().join("dest");
    let archive_path = base_dir.path().join("single_large.zip");

    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("allowed.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"small").unwrap();
        zip.start_file("exceeds.dat", SimpleFileOptions::default()).unwrap();
        zip.write_all(&vec![0xAA; 3 * 1024 * 1024]).unwrap(); // 3 MB
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir,
        max_file_size: Some(1024 * 1024), // Limit single file to 1 MB
        ..Default::default()
    };

    let result = ExtractionEngine::extract(&archive_path, &options);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, ExtractionError::ResourceLimitExceeded(_)),
        "Expected ResourceLimitExceeded error, got: {:?}",
        err
    );
}

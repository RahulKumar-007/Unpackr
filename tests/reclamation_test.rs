use std::fs::{self, File};
use std::io::{Read, Write};
use tempfile::tempdir;
use unpackr::archive::{EntryState, ZipInspector};
use unpackr::extraction::{CollisionPolicy, ExtractionEngine, ExtractionOptions};
use unpackr::reclamation::fs_metrics::get_physical_allocated_bytes;
use unpackr::reclamation::puncher::{compute_inward_reclaim_range, DEFAULT_BLOCK_SIZE};
use unpackr::state::tracker::StateTracker;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

fn create_large_archive(path: &std::path::Path, num_entries: usize, entry_size: usize) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);

    for i in 0..num_entries {
        let name = format!("entry_{:02}.dat", i);
        // Use Stored method to guarantee known uncompressed and compressed sizes without flate2 variability
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file(&name, options).unwrap();

        // Write non-zero pseudo-data so the OS allocates real physical blocks
        let chunk = vec![(i as u8).wrapping_add(1); 4096];
        let chunks = entry_size / 4096;
        for _ in 0..chunks {
            zip.write_all(&chunk).unwrap();
        }
    }

    zip.finish().unwrap();
}

#[test]
fn test_inward_range_boundary_calculation() {
    // Test 1: Entry with 64 KB at offset 100
    // [100, 65636)
    // R_start = ceil(100/4096)*4096 = 4096
    // R_end = floor(65636/4096)*4096 = 65536
    // Reclaim range: [4096, 65536), length = 61440 (15 blocks)
    let range = compute_inward_reclaim_range(100, 65536, DEFAULT_BLOCK_SIZE);
    assert_eq!(range, Some((4096, 61440)));

    // Test 2: Sub-block entry -> no hole
    let sub = compute_inward_reclaim_range(100, 2048, DEFAULT_BLOCK_SIZE);
    assert_eq!(sub, None);

    // Test 3: Exactly aligned entry [4096, 12288) -> 8192 bytes
    let aligned = compute_inward_reclaim_range(4096, 8192, DEFAULT_BLOCK_SIZE);
    assert_eq!(aligned, Some((4096, 8192)));
}

#[test]
fn test_physical_disk_space_reclamation() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("reclaim_sample.zip");

    // 4 entries of 128 KB each = 512 KB
    create_large_archive(&archive_path, 4, 128 * 1024);

    let initial_physical_bytes = get_physical_allocated_bytes(&archive_path)
        .expect("Failed to get initial physical allocation");
    assert!(initial_physical_bytes >= 512 * 1024, "Archive should have physical blocks allocated");

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: true, // Enable in-place reclamation
        state_dir: None,
        verbose: true,
        quiet: false,
    };

    let summary = ExtractionEngine::extract(&archive_path, &options).expect("Extraction with reclamation failed");

    // 1. Verify reclaimed bytes recorded
    assert!(summary.reclaimed_archive_bytes > 0, "Expected reclaimed archive bytes > 0, got {}", summary.reclaimed_archive_bytes);
    assert_eq!(summary.extracted_files, 4);

    // 2. Query physical allocated blocks on the archive file
    let final_physical_bytes = get_physical_allocated_bytes(&archive_path)
        .expect("Failed to get final physical allocation");

    // Physical space must have decreased
    assert!(
        final_physical_bytes < initial_physical_bytes,
        "Physical space did not decrease! initial: {}, final: {}",
        initial_physical_bytes,
        final_physical_bytes
    );

    // 3. Verify manifest marks entries as Reclaimed
    let manifest_path = dest_dir.path().join(".unpackr/manifest.json");
    let tracker = StateTracker::load_from_file(&manifest_path).unwrap();
    assert_eq!(tracker.manifest.reclaimed_count(), 4);
    assert_eq!(tracker.manifest.verified_count(), 4);

    for record in tracker.manifest.entries.values() {
        assert_eq!(record.state, EntryState::Reclaimed);
    }

    // 4. Verify all extracted files in destination are complete and valid
    let failures = tracker.verify_extracted_output();
    assert!(failures.is_empty(), "Extracted files corrupted: {:?}", failures);

    // 5. Verify Central Directory and Local File Headers remain intact and parseable
    let inspection = ZipInspector::inspect(&archive_path).expect("Archive Central Directory must remain parseable after hole punching");
    assert_eq!(inspection.total_entries, 4);

    // Read local file header of entry 0 from archive file to verify signature 0x04034b50 is intact
    let mut file = File::open(&archive_path).unwrap();
    let mut sig_buf = [0u8; 4];
    file.read_exact(&mut sig_buf).unwrap();
    let sig = u32::from_le_bytes(sig_buf);
    assert_eq!(sig, 0x04034b50, "Local file header magic signature was destroyed!");
}

#[test]
fn test_default_mode_does_not_modify_read_only_archive() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("readonly_sample.zip");
    create_large_archive(&archive_path, 2, 64 * 1024);

    let initial_data = fs::read(&archive_path).unwrap();
    let initial_physical = get_physical_allocated_bytes(&archive_path).unwrap();

    // Set file permissions to read-only (0o444 on Unix)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&archive_path, fs::Permissions::from_mode(0o444)).unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false, // Default: read-only
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    let summary = ExtractionEngine::extract(&archive_path, &options).expect("Read-only extraction failed");
    assert_eq!(summary.reclaimed_archive_bytes, 0);

    // Verify archive was NOT modified in any way
    let current_data = fs::read(&archive_path).unwrap();
    assert_eq!(initial_data, current_data, "Read-only archive bytes were modified!");

    let current_physical = get_physical_allocated_bytes(&archive_path).unwrap();
    assert_eq!(initial_physical, current_physical);

    // Reset permissions for cleanup
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(&archive_path, fs::Permissions::from_mode(0o644));
    }
}

#[test]
fn test_sub_block_entries_reclaim_safely_without_holes() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("small_sample.zip");

    // Entries of 500 bytes (smaller than 4096-byte block)
    create_large_archive(&archive_path, 3, 500);

    let initial_data = fs::read(&archive_path).unwrap();

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: true, // Even with reclaim requested, sub-block entries cannot be punched
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    let summary = ExtractionEngine::extract(&archive_path, &options).unwrap();
    assert_eq!(summary.reclaimed_archive_bytes, 0);

    // Verify archive data is identical
    let after_data = fs::read(&archive_path).unwrap();
    assert_eq!(initial_data, after_data);
}

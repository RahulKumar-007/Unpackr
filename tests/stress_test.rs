use std::collections::HashMap;
use std::fs::{self, File};
use std::io::Write;
use tempfile::tempdir;
use unpackr::archive::ZipInspector;
use unpackr::extraction::{CollisionPolicy, ExtractionEngine, ExtractionOptions, ResumeOptions};
use unpackr::state::tracker::StateTracker;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn test_thousand_entries_deep_hierarchy_stress() {
    let base_dir = tempdir().unwrap();
    let archive_path = base_dir.path().join("thousand_stress.zip");
    let dest_dir = base_dir.path().join("thousand_dest");

    let total_dirs = 200;
    let total_files = 800;
    let mut file_payloads: HashMap<String, Vec<u8>> = HashMap::new();

    // 1. Create 1,000 entries (200 directories + 800 files of varying sizes)
    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);

        // Add 200 directories with deep nesting
        for d in 0..total_dirs {
            let depth = (d % 6) + 1;
            let mut dir_path = String::new();
            for lvl in 0..depth {
                dir_path.push_str(&format!("lvl{}_{:02}/", lvl, d % 10));
            }
            dir_path.push_str(&format!("leaf_dir_{:03}/", d));
            let _ = zip.add_directory(&dir_path, SimpleFileOptions::default());
        }

        // Add 800 files with diverse sizes and methods
        for f in 0..total_files {
            let depth = (f % 5) + 1;
            let mut parent = String::new();
            for lvl in 0..depth {
                parent.push_str(&format!("lvl{}_{:02}/", lvl, f % 10));
            }
            let filename = format!("{}file_{:04}.dat", parent, f);

            let (size, is_deflated) = match f % 4 {
                0 => ((f % 3000) + 10, false), // Sub-block: 10 to 3010 bytes (Stored)
                1 => (4096, true),              // Exactly 1 block (Deflated)
                2 => (8192, false),             // Exactly 2 blocks (Stored)
                _ => (32768, true),             // 8 blocks (Deflated)
            };

            let payload: Vec<u8> = (0..size).map(|b| ((b * 31 + f) % 251 + 1) as u8).collect();
            let method = if is_deflated {
                zip::CompressionMethod::Deflated
            } else {
                zip::CompressionMethod::Stored
            };
            let opts = SimpleFileOptions::default().compression_method(method);
            zip.start_file(&filename, opts).unwrap();
            zip.write_all(&payload).unwrap();
            file_payloads.insert(filename, payload);
        }

        zip.finish().unwrap();
    }

    // 2. Run extraction with in-place reclamation enabled
    let options = ExtractionOptions {
        destination: dest_dir.clone(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: true,
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    let summary = ExtractionEngine::extract(&archive_path, &options)
        .expect("Extraction of 1,000 entries failed");

    // 3. Verify counts
    assert_eq!(summary.extracted_files, total_files);
    assert_eq!(summary.created_directories, total_dirs);
    assert!(
        summary.reclaimed_archive_bytes > 0,
        "Expected multi-block entries to be reclaimed"
    );

    // 4. Verify Manifest State
    let manifest_path = dest_dir.join(".unpackr/manifest.json");
    let tracker = StateTracker::load_from_file(&manifest_path).unwrap();
    assert_eq!(tracker.manifest.entries.len(), total_dirs + total_files);
    assert_eq!(tracker.manifest.verified_count(), total_dirs + total_files);

    // 5. Verify Content of Extracted Files
    for (filename, expected) in file_payloads.iter() {
        let extracted_path = dest_dir.join(filename);
        assert!(extracted_path.is_file(), "File missing: {:?}", extracted_path);
        let content = fs::read(&extracted_path).unwrap();
        assert_eq!(
            content.len(),
            expected.len(),
            "Size mismatch for {}",
            filename
        );
        assert_eq!(&content, expected, "Data corrupted for {}", filename);
    }

    // 6. Verify Archive Central Directory is still fully parseable
    let inspection = ZipInspector::inspect(&archive_path)
        .expect("Reclaimed archive Central Directory must remain parseable");
    assert_eq!(inspection.total_entries, total_dirs + total_files);
}

#[test]
fn test_mixed_compression_and_sparsity_stress() {
    let base_dir = tempdir().unwrap();
    let archive_path = base_dir.path().join("mixed_stress.zip");
    let dest_dir = base_dir.path().join("mixed_dest");

    let mut payloads: HashMap<String, Vec<u8>> = HashMap::new();

    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);

        // 1. Sparse file: 4 MB of all zeros (compressed with Deflate)
        let sparse_data = vec![0u8; 4 * 1024 * 1024];
        zip.start_file(
            "sparse_4mb.bin",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&sparse_data).unwrap();
        payloads.insert("sparse_4mb.bin".to_string(), sparse_data);

        // 2. Incompressible pseudo-random data: 1 MB (Stored)
        let stored_data: Vec<u8> = (0..1024 * 1024).map(|i| (i * 19 + 7) as u8).collect();
        zip.start_file(
            "stored_random.bin",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&stored_data).unwrap();
        payloads.insert("stored_random.bin".to_string(), stored_data);

        // 3. Repetitive compressible text: 1 MB (Deflated)
        let text_data = "Unpackr Low-Disk-Space High-Throughput Engine!\n"
            .repeat(25000)
            .into_bytes();
        zip.start_file(
            "repetitive_text.txt",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(&text_data).unwrap();
        payloads.insert("repetitive_text.txt".to_string(), text_data);

        // 4. Sub-block file: 512 bytes (Stored)
        let sub_data = vec![0xEEu8; 512];
        zip.start_file(
            "sub_block_512.dat",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&sub_data).unwrap();
        payloads.insert("sub_block_512.dat".to_string(), sub_data);

        // 5. Boundary file: 4097 bytes (1 block + 1 byte)
        let b1_data = vec![0xABu8; 4097];
        zip.start_file(
            "boundary_4097.dat",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        zip.write_all(&b1_data).unwrap();
        payloads.insert("boundary_4097.dat".to_string(), b1_data);

        // 6. Zero-byte empty file
        zip.start_file(
            "empty.dat",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
        )
        .unwrap();
        payloads.insert("empty.dat".to_string(), Vec::new());

        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir.clone(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 200.0,
        reclaim_archive: true,
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    let summary = ExtractionEngine::extract(&archive_path, &options)
        .expect("Extraction of mixed compression dataset failed");

    assert_eq!(summary.extracted_files, 6);
    // SparseWriter must have saved the 4 MB of zero blocks
    assert!(
        summary.sparse_bytes_saved >= 4 * 1024 * 1024,
        "Sparse space saved ({}) should be at least 4 MB",
        summary.sparse_bytes_saved
    );
    // Hole puncher must have reclaimed storage for the multi-block stored files
    assert!(
        summary.reclaimed_archive_bytes > 0,
        "Expected reclaimed archive bytes > 0"
    );

    // Verify all payloads
    for (filename, expected) in payloads.iter() {
        let content = fs::read(dest_dir.join(filename)).unwrap();
        assert_eq!(&content, expected, "Mismatch for {}", filename);
    }
}

#[test]
fn test_repeated_rolling_crash_recovery_stress() {
    let base_dir = tempdir().unwrap();
    let archive_path = base_dir.path().join("rolling_crash.zip");
    let dest_dir = base_dir.path().join("rolling_dest");

    let num_entries = 20;
    let entry_size = 128 * 1024; // 128 KB each
    let mut original_data: Vec<Vec<u8>> = Vec::new();

    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);

        for i in 0..num_entries {
            let name = format!("chunk_{:02}.dat", i);
            let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
            zip.start_file(&name, opts).unwrap();
            let data = vec![(i as u8).wrapping_add(1); entry_size];
            zip.write_all(&data).unwrap();
            original_data.push(data);
        }

        zip.finish().unwrap();
    }

    let inspection = ZipInspector::inspect(&archive_path).unwrap();
    let job_id = unpackr::state::job::JobId::generate(&archive_path, &inspection.identity);

    // Initialize manifest
    let manifest = unpackr::state::manifest::ExtractionManifest::create_new(
        job_id.as_str(),
        &inspection,
        &dest_dir,
    )
    .unwrap();
    let manifest_path = dest_dir.join(".unpackr/manifest.json");
    fs::create_dir_all(dest_dir.join(".unpackr")).unwrap();
    manifest.save_atomic(&manifest_path).unwrap();

    let mut tracker = StateTracker::load_from_file(&manifest_path).unwrap();

    // Open archive for read/write reclamation
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&archive_path)
        .unwrap();
    let puncher_file = file.try_clone().unwrap();
    let mut puncher = unpackr::reclamation::ArchiveHolePuncher::new(
        puncher_file,
        inspection.file_size,
        inspection.central_directory_offset,
    );
    let mut reader_file = file;

    // Extract first 10 entries cleanly with in-place reclamation
    for entry in &inspection.entries[0..10] {
        tracker.set_entry_extracting(&entry.name);
        let result = unpackr::extraction::worker::EntryWorker::extract_entry(
            &mut reader_file,
            entry,
            &dest_dir,
            CollisionPolicy::Fail,
            true,
            100.0,
        )
        .unwrap();

        if let unpackr::extraction::worker::WorkerResult::Extracted { path, .. } = result {
            tracker.set_entry_extracted(&entry.name);
            tracker.set_entry_verified(&entry.name, path).unwrap();
            puncher.punch_entry(entry).unwrap();
            tracker.set_entry_reclaimed(&entry.name).unwrap();
        }
    }

    // Now simulate crash during entry 10:
    // Entry 10 left in Extracting state with an orphaned temporary file
    let entry_10 = &inspection.entries[10];
    tracker.set_entry_extracting(&entry_10.name);
    let orphaned_tmp = dest_dir.join(".chunk_10.dat.unpackr_tmp_9999");
    fs::write(&orphaned_tmp, b"corrupted partial data from interrupted stream").unwrap();
    assert!(orphaned_tmp.exists());

    // Entries 11..19 remain Pending. Archive blocks for 10..19 are unpunched.

    // Run resume
    let resume_opts = ResumeOptions {
        destination_override: Some(dest_dir.clone()),
        archive_override: None,
        retry_failed: true,
        verify_existing: true,
        collision_policy: None,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: true,
        verbose: false,
        quiet: false,
    };

    let resume_summary = ExtractionEngine::resume(job_id.as_str(), &resume_opts)
        .expect("Rolling crash resume failed");

    assert_eq!(resume_summary.extracted_files, num_entries);
    assert!(!orphaned_tmp.exists(), "Orphaned temp file was not cleaned!");
    assert!(dest_dir.join("chunk_10.dat").is_file(), "Re-extracted file missing!");

    // Verify content of all 20 files
    for (i, orig) in original_data.iter().enumerate().take(num_entries) {
        let name = format!("chunk_{:02}.dat", i);
        let content = fs::read(dest_dir.join(&name)).unwrap();
        assert_eq!(&content, orig);
    }
}

#[test]
fn test_memory_bounded_streaming_stress() {
    let base_dir = tempdir().unwrap();
    let archive_path = base_dir.path().join("streaming_stress.zip");
    let dest_dir = base_dir.path().join("streaming_dest");

    // 25 MB entry
    let entry_size = 25 * 1024 * 1024;
    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file(
            "large_stream.dat",
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();

        let chunk = vec![0x7A; 64 * 1024];
        let chunks = entry_size / chunk.len();
        for _ in 0..chunks {
            zip.write_all(&chunk).unwrap();
        }
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir.clone(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 2000.0,
        reclaim_archive: true,
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    let summary = ExtractionEngine::extract(&archive_path, &options)
        .expect("Streaming extraction of 25 MB failed");

    assert_eq!(summary.extracted_files, 1);
    assert_eq!(summary.total_uncompressed_bytes, entry_size as u64);
    assert!(summary.throughput_mb_per_sec > 0.0);

    let extracted_file = dest_dir.join("large_stream.dat");
    let meta = fs::metadata(&extracted_file).unwrap();
    assert_eq!(meta.len(), entry_size as u64);
}

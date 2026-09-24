use std::fs::{self, File};
use std::io::Write;
use std::path::Path;
use tempfile::tempdir;
use unpackr::archive::ZipInspector;
use unpackr::extraction::{CollisionPolicy, ExtractionEngine, ExtractionOptions, ResumeOptions};
use unpackr::reclamation::fs_metrics::get_physical_allocated_bytes;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

fn create_benchmark_archive(path: &Path, num_entries: usize, entry_size: usize) -> Vec<Vec<u8>> {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);
    let mut original_datasets = Vec::new();

    for i in 0..num_entries {
        let name = format!("bench_file_{:03}.dat", i);
        let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file(&name, options).unwrap();

        // Generate distinct pseudo-random patterned content per entry
        let mut entry_data = Vec::with_capacity(entry_size);
        for chunk_idx in 0..(entry_size / 4096) {
            let pattern_byte = ((i * 37 + chunk_idx * 17 + 1) % 251 + 1) as u8;
            entry_data.resize(entry_data.len() + 4096, pattern_byte);
        }
        zip.write_all(&entry_data).unwrap();
        original_datasets.push(entry_data);
    }

    zip.finish().unwrap();
    original_datasets
}

#[test]
fn test_benchmark_peak_storage_reclamation_vs_standard() {
    let base_dir = tempdir().unwrap();
    let archive_std_path = base_dir.path().join("bench_std.zip");
    let archive_rec_path = base_dir.path().join("bench_rec.zip");

    // 5 entries of 4 MB each = 20 MB total dataset
    let num_entries = 5;
    let entry_size = 4 * 1024 * 1024;
    let original_data = create_benchmark_archive(&archive_std_path, num_entries, entry_size);
    fs::copy(&archive_std_path, &archive_rec_path).unwrap();

    let initial_archive_phys = get_physical_allocated_bytes(&archive_std_path)
        .expect("Failed to get archive physical size");
    assert!(
        initial_archive_phys >= 20 * 1024 * 1024,
        "Initial archive should allocate at least 20 MB physically"
    );

    // 1. Run Standard Extraction (Read-only, no reclamation)
    let dest_std = base_dir.path().join("dest_std");
    let options_std = ExtractionOptions {
        destination: dest_std.clone(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
        quiet: false,
        ..Default::default()
    };
    let summary_std = ExtractionEngine::extract(&archive_std_path, &options_std)
        .expect("Standard extraction failed");

    // 2. Run Progressive Reclamation Extraction
    let dest_rec = base_dir.path().join("dest_rec");
    let options_rec = ExtractionOptions {
        destination: dest_rec.clone(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: true,
        state_dir: None,
        verbose: false,
        quiet: false,
        ..Default::default()
    };
    let summary_rec = ExtractionEngine::extract(&archive_rec_path, &options_rec)
        .expect("Reclaim extraction failed");

    // 3. Verify Footprint Reduction
    println!("Standard Peak Footprint:  {} bytes", summary_std.peak_disk_footprint_bytes);
    println!("Reclaim Peak Footprint:   {} bytes", summary_rec.peak_disk_footprint_bytes);
    println!("Standard Duration:        {:.2?}", summary_std.duration);
    println!("Reclaim Duration:         {:.2?}", summary_rec.duration);
    println!("Reclaim Throughput:       {:.2} MB/s", summary_rec.throughput_mb_per_sec);
    println!("Reclaimed Archive Bytes:  {} bytes", summary_rec.reclaimed_archive_bytes);

    // Standard peak must be archive + total extracted (~40 MB)
    assert!(
        summary_std.peak_disk_footprint_bytes >= 38 * 1024 * 1024,
        "Expected standard peak >= 38 MB, got {}",
        summary_std.peak_disk_footprint_bytes
    );

    // Reclaim peak must be strictly lower than standard peak
    assert!(
        summary_rec.peak_disk_footprint_bytes < summary_std.peak_disk_footprint_bytes,
        "Reclaim peak ({}) must be lower than standard peak ({})",
        summary_rec.peak_disk_footprint_bytes,
        summary_std.peak_disk_footprint_bytes
    );

    // Peak reduction should be at least 30% of standard peak
    let saved_peak = summary_std.peak_disk_footprint_bytes - summary_rec.peak_disk_footprint_bytes;
    let pct_saved = (saved_peak as f64 / summary_std.peak_disk_footprint_bytes as f64) * 100.0;
    println!("Peak Storage Reduction:   {:.1}%", pct_saved);
    assert!(
        pct_saved >= 30.0,
        "Expected at least 30% peak disk storage reduction, got {:.1}%",
        pct_saved
    );

    // 4. Verify Physical Allocation on Reclaimed Archive File
    let final_archive_phys = get_physical_allocated_bytes(&archive_rec_path)
        .expect("Failed to get final physical size of reclaimed archive");
    let initial_archive_phys_rec = get_physical_allocated_bytes(&archive_std_path).unwrap();
    assert!(
        final_archive_phys < initial_archive_phys_rec / 4,
        "Final archive physical size ({} bytes) should be less than 25% of initial ({} bytes)",
        final_archive_phys,
        initial_archive_phys_rec
    );

    // 5. Verify Byte-for-Byte Data Integrity of All Extracted Files
    for (i, orig) in original_data.iter().enumerate().take(num_entries) {
        let name = format!("bench_file_{:03}.dat", i);
        let extracted_std = fs::read(dest_std.join(&name)).expect("Failed to read std file");
        let extracted_rec = fs::read(dest_rec.join(&name)).expect("Failed to read rec file");

        assert_eq!(
            &extracted_std, orig,
            "Standard extraction data mismatch on entry {}",
            i
        );
        assert_eq!(
            &extracted_rec, orig,
            "Reclaimed extraction data mismatch on entry {}",
            i
        );
    }

    // 6. Verify Archive Structure & Central Directory Integrity
    let inspection_rec = ZipInspector::inspect(&archive_rec_path)
        .expect("Archive must remain valid and parseable by ZipInspector after reclamation");
    assert_eq!(inspection_rec.total_entries, num_entries);
}

#[test]
fn test_benchmark_interrupted_reclaim_and_resume_peak_storage() {
    let base_dir = tempdir().unwrap();
    let archive_path = base_dir.path().join("bench_resume.zip");
    let dest_dir = base_dir.path().join("dest_resume");

    let num_entries = 4;
    let entry_size = 2 * 1024 * 1024; // 2 MB each = 8 MB total
    let original_data = create_benchmark_archive(&archive_path, num_entries, entry_size);

    // Step 1: Extract first 2 entries manually via standard extraction options
    let options = ExtractionOptions {
        destination: dest_dir.clone(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: true,
        state_dir: None,
        verbose: false,
        quiet: false,
        ..Default::default()
    };

    let summary = ExtractionEngine::extract(&archive_path, &options).unwrap();
    assert_eq!(summary.extracted_files, 4);

    // Step 2: Now simulate a resume scenario on this directory
    let resume_options = ResumeOptions {
        destination_override: None,
        archive_override: None,
        retry_failed: false,
        verify_existing: true,
        collision_policy: None,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: true,
        verbose: false,
        quiet: false,
        ..Default::default()
    };

    let resume_summary = ExtractionEngine::resume(&summary.job_id, &resume_options)
        .expect("Resume failed");

    assert_eq!(resume_summary.extracted_files, 4);
    assert_eq!(resume_summary.total_uncompressed_bytes, (num_entries * entry_size) as u64);
    assert!(resume_summary.peak_disk_footprint_bytes > 0);

    // Verify all files match
    for (i, orig) in original_data.iter().enumerate().take(num_entries) {
        let name = format!("bench_file_{:03}.dat", i);
        let content = fs::read(dest_dir.join(&name)).unwrap();
        assert_eq!(&content, orig);
    }
}

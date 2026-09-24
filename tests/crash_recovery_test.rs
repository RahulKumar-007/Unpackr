use std::fs::{self, File};
use std::io::Write;
use std::process::Command;
use std::time::Duration;
use tempfile::tempdir;
use unpackr::archive::EntryState;
use unpackr::extraction::{CollisionPolicy, ExtractionEngine, ExtractionOptions, ResumeOptions};
use unpackr::state::tracker::StateTracker;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

fn create_sample_archive(path: &std::path::Path, count: usize) {
    let file = File::create(path).unwrap();
    let mut zip = ZipWriter::new(file);

    for i in 0..count {
        let name = format!("file_{:03}.txt", i);
        zip.start_file(&name, SimpleFileOptions::default()).unwrap();
        let content = format!("Payload data for entry {} with repetitive string padding ...\n", i).repeat(100);
        zip.write_all(content.as_bytes()).unwrap();
    }

    zip.finish().unwrap();
}

#[test]
fn test_crash_recovery_cleans_orphaned_tmp_and_resumes() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("sample.zip");
    create_sample_archive(&archive_path, 3);

    // Initial extraction of entry 0 only by setting up a manifest
    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
    };

    // Run extraction once to create base manifest
    ExtractionEngine::extract(&archive_path, &options).unwrap();

    let manifest_path = dest_dir.path().join(".unpackr/manifest.json");
    let mut tracker = StateTracker::load_from_file(&manifest_path).unwrap();

    // Simulate crash on file_001.txt:
    // 1. Leave an orphaned temporary file in destination
    let orphaned_tmp = dest_dir.path().join(".file_001.txt.unpackr_tmp_1");
    fs::write(&orphaned_tmp, b"partial broken decompressed stream bytes").unwrap();
    assert!(orphaned_tmp.exists());

    // 2. Modify file_001.txt and file_002.txt state in manifest to simulate in-flight crash
    if let Some(record) = tracker.manifest.entries.get_mut("file_001.txt") {
        record.state = EntryState::Extracting;
        record.output_path = None;
    }
    if let Some(record) = tracker.manifest.entries.get_mut("file_002.txt") {
        record.state = EntryState::Pending;
        record.output_path = None;
    }
    // Remove the actual extracted file_001.txt and file_002.txt to simulate they weren't finalized
    let _ = fs::remove_file(dest_dir.path().join("file_001.txt"));
    let _ = fs::remove_file(dest_dir.path().join("file_002.txt"));
    tracker.manifest.save_atomic(&manifest_path).unwrap();

    // 3. Run Resume
    let resume_opts = ResumeOptions {
        destination_override: None,
        archive_override: None,
        retry_failed: true,
        verify_existing: true,
        collision_policy: None,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        verbose: true,
    };

    let summary = ExtractionEngine::resume(
        dest_dir.path().to_str().unwrap(),
        &resume_opts,
    )
    .expect("Resume failed");

    // 4. Verify orphaned temporary file was cleaned up
    assert!(!orphaned_tmp.exists(), "Orphaned tmp file should have been removed");

    // 5. Verify all files are extracted and correct
    assert_eq!(summary.total_entries, 3);
    for i in 0..3 {
        let fpath = dest_dir.path().join(format!("file_{:03}.txt", i));
        assert!(fpath.exists());
    }

    // 6. Verify integrity check passes 100%
    let updated_tracker = StateTracker::load_from_file(&manifest_path).unwrap();
    let failures = updated_tracker.verify_extracted_output();
    assert!(failures.is_empty(), "Integrity failures on resume: {:?}", failures);
    assert_eq!(updated_tracker.manifest.verified_count(), 3);
}

#[test]
fn test_crash_recovery_recovers_already_written_file() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("sample.zip");
    create_sample_archive(&archive_path, 2);

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
    };

    // Extract all
    ExtractionEngine::extract(&archive_path, &options).unwrap();
    let manifest_path = dest_dir.path().join(".unpackr/manifest.json");

    // Modify manifest so file_001.txt is marked 'Extracting' even though file exists on disk with correct CRC
    let mut tracker = StateTracker::load_from_file(&manifest_path).unwrap();
    if let Some(record) = tracker.manifest.entries.get_mut("file_001.txt") {
        record.state = EntryState::Extracting;
    }
    tracker.manifest.save_atomic(&manifest_path).unwrap();

    // Call reconcile_and_clean
    let mut tracker2 = StateTracker::load_from_file(&manifest_path).unwrap();
    let rec_summary = tracker2.reconcile_and_clean(false, false).unwrap();

    // It should have recovered file_001.txt as Verified without re-extracting!
    assert_eq!(rec_summary.recovered_verified_entries, 1);
    assert_eq!(tracker2.manifest.verified_count(), 2);
}

#[test]
fn test_resume_rejects_tampered_source_archive() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("sample.zip");
    create_sample_archive(&archive_path, 2);

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
    };

    ExtractionEngine::extract(&archive_path, &options).unwrap();

    // Tamper with archive
    {
        let mut file = fs::OpenOptions::new().append(true).open(&archive_path).unwrap();
        file.write_all(b"tampering corrupt bytes appended to zip").unwrap();
    }

    let resume_opts = ResumeOptions::default();
    let result = ExtractionEngine::resume(dest_dir.path().to_str().unwrap(), &resume_opts);

    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("validation failed on resume") || err_msg.contains("identity mismatch"));
}

#[test]
fn test_resume_detects_and_reextracts_deleted_verified_file() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("sample.zip");
    create_sample_archive(&archive_path, 3);

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
    };

    ExtractionEngine::extract(&archive_path, &options).unwrap();

    // Delete file_001.txt from disk
    let deleted_file = dest_dir.path().join("file_001.txt");
    fs::remove_file(&deleted_file).unwrap();
    assert!(!deleted_file.exists());

    // Resume
    let resume_opts = ResumeOptions::default();
    let summary = ExtractionEngine::resume(dest_dir.path().to_str().unwrap(), &resume_opts).unwrap();

    // file_001.txt was detected missing, reset to Pending, and re-extracted!
    assert!(deleted_file.exists());
    assert_eq!(summary.extracted_files, 3);

    let manifest_path = dest_dir.path().join(".unpackr/manifest.json");
    let tracker = StateTracker::load_from_file(&manifest_path).unwrap();
    assert!(tracker.verify_extracted_output().is_empty());
}

#[test]
fn test_sigkill_child_process_recovery() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("large_sample.zip");
    create_sample_archive(&archive_path, 40);

    let bin_path = env!("CARGO_BIN_EXE_unpackr");

    // Spawn extraction child process
    let mut child = Command::new(bin_path)
        .args(["extract", archive_path.to_str().unwrap(), dest_dir.path().to_str().unwrap()])
        .spawn()
        .expect("Failed to spawn unpackr child process");

    // Poll until manifest file is created
    let manifest_path = dest_dir.path().join(".unpackr/manifest.json");
    let start = std::time::Instant::now();
    while !manifest_path.exists() && start.elapsed() < Duration::from_secs(3) {
        std::thread::sleep(Duration::from_millis(1));
    }
    assert!(manifest_path.exists(), "Manifest was not created in time by child process");

    // Sleep 5ms so it is actively decompressing entries
    std::thread::sleep(Duration::from_millis(5));

    // Send SIGKILL to child process mid-stream
    unsafe {
        libc::kill(child.id() as i32, libc::SIGKILL);
    }
    let _ = child.wait();

    // Verify manifest exists and is valid JSON (atomic rename prevented corruption)
    assert!(manifest_path.exists());
    let manifest_content = fs::read_to_string(&manifest_path).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&manifest_content).expect("Manifest must be valid JSON even after SIGKILL");
    assert_eq!(parsed["version"], 1);

    // Now resume extraction using CLI
    let resume_output = Command::new(bin_path)
        .args(["resume", dest_dir.path().to_str().unwrap()])
        .output()
        .expect("Failed to execute unpackr resume");

    assert!(resume_output.status.success(), "Resume failed: {}", String::from_utf8_lossy(&resume_output.stderr));

    // Verify all 40 files are extracted
    for i in 0..40 {
        let fpath = dest_dir.path().join(format!("file_{:03}.txt", i));
        assert!(fpath.exists(), "File {:?} was not extracted after resume", fpath);
    }

    // Run unpackr verify
    let verify_output = Command::new(bin_path)
        .args(["verify", dest_dir.path().to_str().unwrap()])
        .output()
        .expect("Failed to execute unpackr verify");

    assert!(verify_output.status.success());
    let verify_str = String::from_utf8_lossy(&verify_output.stdout);
    assert!(verify_str.contains("Integrity Status:     PASS"));
}

#[test]
fn test_cli_cancel_command() {
    let dest_dir = tempdir().unwrap();
    let zip_dir = tempdir().unwrap();
    let archive_path = zip_dir.path().join("cancel_sample.zip");
    create_sample_archive(&archive_path, 3);

    let bin_path = env!("CARGO_BIN_EXE_unpackr");

    // 1. Run extraction
    let extract_output = Command::new(bin_path)
        .args(["extract", archive_path.to_str().unwrap(), dest_dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(extract_output.status.success());

    // 2. Run cancel without --clean
    let cancel_output = Command::new(bin_path)
        .args(["cancel", dest_dir.path().to_str().unwrap()])
        .output()
        .unwrap();
    assert!(cancel_output.status.success());
    let cancel_str = String::from_utf8_lossy(&cancel_output.stdout);
    assert!(cancel_str.contains("UNPACKR JOB CANCELLED"));

    // 3. Run cancel with --clean
    let cancel_clean_output = Command::new(bin_path)
        .args(["cancel", dest_dir.path().to_str().unwrap(), "--clean"])
        .output()
        .unwrap();
    assert!(cancel_clean_output.status.success());

    // State directory .unpackr should be removed
    let manifest_path = dest_dir.path().join(".unpackr");
    assert!(!manifest_path.exists());
}

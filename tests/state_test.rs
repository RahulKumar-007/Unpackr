use std::fs::{self, File};
use std::io::Write;
use std::process::Command;
use tempfile::{tempdir, NamedTempFile};
use unpackr::archive::EntryState;
use unpackr::extraction::{CollisionPolicy, ExtractionEngine, ExtractionOptions};
use unpackr::state::job::{find_manifest_file, JobId};
use unpackr::state::tracker::StateTracker;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

#[test]
fn test_manifest_creation_and_state_lifecycle() {
    let dest_dir = tempdir().unwrap();
    let archive_path = std::path::Path::new("tests/test_data/valid_sample.zip");

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 200.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    let summary = ExtractionEngine::extract(archive_path, &options).expect("Extraction failed");

    // 1. Verify manifest exists at <destination>/.unpackr/manifest.json
    let manifest_path = dest_dir.path().join(".unpackr").join("manifest.json");
    assert!(manifest_path.exists());

    // 2. Load manifest and verify state
    let tracker = StateTracker::load_from_file(&manifest_path).expect("Failed to load manifest");
    assert_eq!(tracker.manifest.job_id, summary.job_id);
    assert_eq!(tracker.manifest.entries.len(), 5);
    assert_eq!(tracker.manifest.verified_count(), 5); // 4 files + 1 dir
    assert_eq!(tracker.manifest.failed_count(), 0);
    assert_eq!(tracker.manifest.pending_count(), 0);
    assert_eq!(tracker.manifest.progress_percent(), 100.0);

    // Verify all entries are in Verified state
    for record in tracker.manifest.entries.values() {
        assert_eq!(record.state, EntryState::Verified);
        assert!(record.output_path.is_some());
    }

    // 3. Verify integrity check passes
    let failures = tracker.verify_extracted_output();
    assert!(failures.is_empty(), "Expected 0 integrity failures, got: {:?}", failures);
}

#[test]
fn test_verification_detects_disk_corruption() {
    let dest_dir = tempdir().unwrap();
    let archive_path = std::path::Path::new("tests/test_data/valid_sample.zip");

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 200.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    ExtractionEngine::extract(archive_path, &options).unwrap();
    let manifest_path = dest_dir.path().join(".unpackr").join("manifest.json");
    let tracker = StateTracker::load_from_file(&manifest_path).unwrap();

    // Corrupt one file: append a byte to plain.txt
    let plain_file = dest_dir.path().join("plain.txt");
    fs::write(&plain_file, b"corrupted content!").unwrap();

    // Run verification
    let failures = tracker.verify_extracted_output();
    assert_eq!(failures.len(), 1);
    assert_eq!(failures[0].entry_name, "plain.txt");
    assert!(failures[0].reason.contains("mismatch"));

    // Delete one file: delete nested/data.txt
    let nested_file = dest_dir.path().join("nested/data.txt");
    fs::remove_file(&nested_file).unwrap();

    let failures2 = tracker.verify_extracted_output();
    assert_eq!(failures2.len(), 2);
    let names: Vec<String> = failures2.iter().map(|f| f.entry_name.clone()).collect();
    assert!(names.contains(&"plain.txt".to_string()));
    assert!(names.contains(&"nested/data.txt".to_string()));
}

#[test]
fn test_archive_identity_tampering_detection() {
    let dest_dir = tempdir().unwrap();
    let mut zip_file = NamedTempFile::new().unwrap();

    {
        let file = File::create(zip_file.path()).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("sample.txt", SimpleFileOptions::default()).unwrap();
        zip.write_all(b"initial data").unwrap();
        zip.finish().unwrap();
    }

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    ExtractionEngine::extract(zip_file.path(), &options).unwrap();
    let manifest_path = dest_dir.path().join(".unpackr").join("manifest.json");
    let tracker = StateTracker::load_from_file(&manifest_path).unwrap();

    // Verify identity passes on unchanged archive
    assert!(tracker.verify_archive_identity(zip_file.path()).is_ok());

    // Tamper with archive: modify byte in place
    zip_file.write_all(b"tampered content append").unwrap();
    zip_file.flush().unwrap();

    // Verify identity detects tampering
    assert!(tracker.verify_archive_identity(zip_file.path()).is_err());
}

#[test]
fn test_job_id_generation_and_lookup() {
    let dummy_path = std::path::Path::new("my_archive.zip");
    let job_id = JobId::generate(dummy_path, "1234567890abcdef");
    assert!(job_id.as_str().starts_with("my_archive_12345678_"));

    let dest_dir = tempdir().unwrap();
    let archive_path = std::path::Path::new("tests/test_data/valid_sample.zip");

    let options = ExtractionOptions {
        destination: dest_dir.path().to_path_buf(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        reclaim_archive: false,
        state_dir: None,
        verbose: false,
        quiet: false,
    };

    let summary = ExtractionEngine::extract(archive_path, &options).unwrap();

    // Find by path
    let found_by_path = find_manifest_file(dest_dir.path().to_str().unwrap());
    assert!(found_by_path.is_some());
    assert_eq!(found_by_path.unwrap(), dest_dir.path().join(".unpackr/manifest.json"));

    // Find by direct manifest path
    let direct_manifest = dest_dir.path().join(".unpackr/manifest.json");
    let found_by_manifest = find_manifest_file(direct_manifest.to_str().unwrap());
    assert!(found_by_manifest.is_some());

    // Find by job ID
    let found_by_id = find_manifest_file(&summary.job_id);
    assert!(found_by_id.is_some());
}

#[test]
fn test_cli_status_and_verify_commands() {
    let dest_dir = tempdir().unwrap();
    let archive_path = "tests/test_data/valid_sample.zip";

    let bin_path = env!("CARGO_BIN_EXE_unpackr");

    // 1. Run extraction via CLI
    let extract_output = Command::new(bin_path)
        .args(["extract", archive_path, dest_dir.path().to_str().unwrap()])
        .output()
        .expect("Failed to execute unpackr extract");
    assert!(extract_output.status.success());

    // 2. Run status via CLI
    let status_output = Command::new(bin_path)
        .args(["status", dest_dir.path().to_str().unwrap()])
        .output()
        .expect("Failed to execute unpackr status");
    assert!(status_output.status.success());
    let status_str = String::from_utf8_lossy(&status_output.stdout);
    assert!(status_str.contains("UNPACKR JOB STATUS"));
    assert!(status_str.contains("Progress:             100.0%"));
    assert!(status_str.contains("Verified:           5"));

    // 3. Run status with --json
    let status_json_output = Command::new(bin_path)
        .args(["status", dest_dir.path().to_str().unwrap(), "--json"])
        .output()
        .expect("Failed to execute unpackr status --json");
    assert!(status_json_output.status.success());
    let parsed_json: serde_json::Value = serde_json::from_slice(&status_json_output.stdout)
        .expect("Failed to parse status JSON");
    assert_eq!(parsed_json["version"], 1);

    // 4. Run verify via CLI (should pass)
    let verify_output = Command::new(bin_path)
        .args(["verify", dest_dir.path().to_str().unwrap()])
        .output()
        .expect("Failed to execute unpackr verify");
    assert!(verify_output.status.success());
    let verify_str = String::from_utf8_lossy(&verify_output.stdout);
    assert!(verify_str.contains("Integrity Status:     PASS"));

    // 5. Corrupt a file and test verify failure
    let file_to_corrupt = dest_dir.path().join("plain.txt");
    fs::write(&file_to_corrupt, b"tampered byte sequence").unwrap();

    let verify_fail_output = Command::new(bin_path)
        .args(["verify", dest_dir.path().to_str().unwrap()])
        .output()
        .expect("Failed to execute unpackr verify on corrupted output");
    assert!(!verify_fail_output.status.success());
    let verify_fail_str = String::from_utf8_lossy(&verify_fail_output.stdout);
    assert!(verify_fail_str.contains("Integrity Status:     FAIL"));
    assert!(verify_fail_str.contains("plain.txt"));
}

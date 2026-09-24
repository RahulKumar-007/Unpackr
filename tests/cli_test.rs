use std::process::Command;
use tempfile::tempdir;

#[test]
fn test_cli_json_inspect_extract_resume_cancel() {
    let bin_path = env!("CARGO_BIN_EXE_unpackr");
    let archive_path = "tests/test_data/valid_sample.zip";
    let dest_dir = tempdir().unwrap();
    let dest_str = dest_dir.path().to_str().unwrap();

    // 1. inspect --json
    let inspect_output = Command::new(bin_path)
        .args(["inspect", archive_path, "--json"])
        .output()
        .expect("Failed to run inspect --json");
    assert!(inspect_output.status.success());
    let inspect_json: serde_json::Value =
        serde_json::from_slice(&inspect_output.stdout).expect("Invalid inspect JSON");
    assert_eq!(inspect_json["total_entries"], 5);
    assert!(inspect_json["entries"].is_array());
    assert!(!inspect_json["is_zip64"].as_bool().unwrap());

    // 2. extract --json
    let extract_output = Command::new(bin_path)
        .args(["extract", archive_path, dest_str, "--json"])
        .output()
        .expect("Failed to run extract --json");
    assert!(extract_output.status.success());
    let extract_json: serde_json::Value =
        serde_json::from_slice(&extract_output.stdout).expect("Invalid extract JSON");
    assert_eq!(extract_json["extracted_files"], 4);
    assert_eq!(extract_json["created_directories"], 1);
    assert_eq!(extract_json["total_entries"], 5);
    assert!(extract_json["job_id"].is_string());

    // 3. resume --json
    let resume_output = Command::new(bin_path)
        .args(["resume", dest_str, "--json"])
        .output()
        .expect("Failed to run resume --json");
    assert!(resume_output.status.success());
    let resume_json: serde_json::Value =
        serde_json::from_slice(&resume_output.stdout).expect("Invalid resume JSON");
    assert_eq!(resume_json["total_entries"], 5);
    assert_eq!(resume_json["extracted_files"], 4);

    // 4. cancel --json
    let cancel_output = Command::new(bin_path)
        .args(["cancel", dest_str, "--json"])
        .output()
        .expect("Failed to run cancel --json");
    assert!(cancel_output.status.success());
    let cancel_json: serde_json::Value =
        serde_json::from_slice(&cancel_output.stdout).expect("Invalid cancel JSON");
    assert!(cancel_json["job_id"].is_string());
    assert!(cancel_json["manifest_path"].is_string());
}

#[test]
fn test_cli_shell_completions() {
    let bin_path = env!("CARGO_BIN_EXE_unpackr");

    for shell in &["bash", "zsh", "fish"] {
        let output = Command::new(bin_path)
            .args(["completions", shell])
            .output()
            .unwrap_or_else(|_| panic!("Failed to run completions for {}", shell));
        assert!(output.status.success());
        let script = String::from_utf8_lossy(&output.stdout);
        assert!(
            script.contains("unpackr"),
            "Completion script for {} should mention unpackr",
            shell
        );
    }
}

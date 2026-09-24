use crate::state::job::find_manifest_file;
use crate::state::tracker::StateTracker;
use anyhow::{bail, Result};

pub fn run_verify(job_id_or_path: &str, json: bool) -> Result<()> {
    let manifest_path = match find_manifest_file(job_id_or_path) {
        Some(p) => p,
        None => bail!(
            "Could not find manifest for job or path: '{}'",
            job_id_or_path
        ),
    };

    let tracker = StateTracker::load_from_file(&manifest_path)?;

    // Check archive identity if archive is present
    let archive_status = if tracker.manifest.archive.path.exists() {
        match tracker.verify_archive_identity(&tracker.manifest.archive.path) {
            Ok(()) => "Source Archive: MATCH (Identical to initial inspection)",
            Err(_e) => "Source Archive: MISMATCH or modified",
        }
    } else {
        "Source Archive: NOT PRESENT (Already moved or deleted)"
    };

    let failures = tracker.verify_extracted_output();
    let verified_entries_count = tracker.manifest.verified_count();

    if json {
        let res = serde_json::json!({
            "job_id": tracker.manifest.job_id,
            "manifest_path": manifest_path,
            "total_verified_entries": verified_entries_count,
            "failures_count": failures.len(),
            "failures": failures.iter().map(|f| {
                serde_json::json!({
                    "entry": f.entry_name,
                    "path": f.path,
                    "reason": f.reason,
                })
            }).collect::<Vec<_>>(),
        });
        println!("{}", serde_json::to_string_pretty(&res)?);
        if !failures.is_empty() {
            bail!(
                "Integrity check failed with {} mismatch(es)",
                failures.len()
            );
        }
        return Ok(());
    }

    println!("================================================================================");
    println!("                         UNPACKR DESTINATION VERIFICATION                       ");
    println!("================================================================================");
    println!("Job ID:               {}", tracker.manifest.job_id);
    println!(
        "Destination:          {}",
        tracker.manifest.destination.display()
    );
    println!("{}", archive_status);
    println!("Checked Entries:      {}", verified_entries_count);
    println!("--------------------------------------------------------------------------------");

    if failures.is_empty() {
        println!("Integrity Status:     PASS (All files byte-verified against archive CRC-32)");
        println!(
            "================================================================================"
        );
        Ok(())
    } else {
        println!(
            "Integrity Status:     FAIL ({} integrity violation(s) detected!)",
            failures.len()
        );
        println!(
            "--------------------------------------------------------------------------------"
        );
        for f in &failures {
            println!("  FAIL: {} ({:?}) - {}", f.entry_name, f.path, f.reason);
        }
        println!(
            "================================================================================"
        );
        bail!(
            "Integrity verification failed for {} file(s)",
            failures.len()
        );
    }
}

use std::fs;
use anyhow::{bail, Result};
use crate::state::job::find_manifest_file;
use crate::state::tracker::StateTracker;

pub fn run_cancel(job_id_or_path: &str, clean: bool, json: bool) -> Result<()> {
    let manifest_path = match find_manifest_file(job_id_or_path) {
        Some(p) => p,
        None => bail!("Could not find manifest for job or path: '{}'", job_id_or_path),
    };

    let mut tracker = StateTracker::load_from_file(&manifest_path)?;
    let cancelled_count = tracker.cancel()?;

    let mut cleaned_files = 0;
    if clean {
        // Remove all extracted files recorded in manifest
        for record in tracker.manifest.entries.values() {
            if let Some(p) = &record.output_path {
                if p.is_file() && fs::remove_file(p).is_ok() {
                    cleaned_files += 1;
                }
            }
        }
        // Remove .unpackr state directory
        if let Some(parent) = manifest_path.parent() {
            let _ = fs::remove_dir_all(parent);
        }
    }

    if json {
        let res = serde_json::json!({
            "job_id": tracker.manifest.job_id,
            "manifest_path": manifest_path,
            "cancelled_entries": cancelled_count,
            "cleaned_files": cleaned_files,
            "cleaned": clean,
        });
        println!("{}", serde_json::to_string_pretty(&res)?);
        return Ok(());
    }

    println!("================================================================================");
    println!("                             UNPACKR JOB CANCELLED                              ");
    println!("================================================================================");
    println!("Job ID:               {}", tracker.manifest.job_id);
    println!("Cancelled Entries:    {}", cancelled_count);
    if clean {
        println!("Cleaned Files:        {}", cleaned_files);
        println!("State Cleaned:        Yes (.unpackr directory removed)");
    } else {
        println!("State Cleaned:        No (manifest updated to FAILED state)");
    }
    println!("================================================================================");

    Ok(())
}

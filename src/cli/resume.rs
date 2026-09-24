use std::path::PathBuf;
use anyhow::Result;
use crate::cli::inspect::format_bytes;
use crate::extraction::{CollisionPolicy, ExtractionEngine, ResumeOptions};

pub fn run_resume(
    target: &str,
    destination: Option<PathBuf>,
    archive: Option<PathBuf>,
    retry_failed: bool,
    verify_existing: bool,
    collision: Option<String>,
    json: bool,
    verbose: bool,
) -> Result<()> {
    let collision_policy = collision
        .as_deref()
        .map(CollisionPolicy::from_str_lossy);

    let options = ResumeOptions {
        destination_override: destination,
        archive_override: archive,
        retry_failed,
        verify_existing,
        collision_policy,
        enable_sparse: true,
        max_compression_ratio: 100.0,
        verbose,
    };

    let summary = ExtractionEngine::resume(target, &options)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("================================================================================");
    println!("                           UNPACKR RESUME COMPLETE                              ");
    println!("================================================================================");
    println!("Job ID:               {}", summary.job_id);
    println!("Archive:              {}", summary.archive_path.display());
    println!("Destination:          {}", summary.destination.display());
    println!("Manifest:             {}", summary.manifest_path.display());
    println!("Total Entries:        {}", summary.total_entries);
    println!("Extracted Files:      {}", summary.extracted_files);
    println!("Created Directories:  {}", summary.created_directories);
    println!("Skipped Files:        {}", summary.skipped_files);
    println!("Data Written:         {}", format_bytes(summary.total_uncompressed_bytes));
    if summary.sparse_bytes_saved > 0 {
        println!("Sparse Space Saved:   {}", format_bytes(summary.sparse_bytes_saved));
    }
    println!("Duration:             {:.2?}", summary.duration);
    println!("================================================================================");

    Ok(())
}

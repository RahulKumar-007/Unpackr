use anyhow::{bail, Result};
use crate::cli::inspect::format_bytes;
use crate::state::job::find_manifest_file;
use crate::state::manifest::ExtractionManifest;

pub fn run_status(job_id_or_path: &str, json: bool) -> Result<()> {
    let manifest_path = match find_manifest_file(job_id_or_path) {
        Some(p) => p,
        None => bail!("Could not find manifest for job or path: '{}'", job_id_or_path),
    };

    let manifest = ExtractionManifest::load(&manifest_path)?;

    if json {
        println!("{}", serde_json::to_string_pretty(&manifest)?);
        return Ok(());
    }

    let total = manifest.entries.len();
    let verified = manifest.verified_count();
    let reclaimed = manifest.reclaimed_count();
    let skipped = manifest.skipped_count();
    let extracted = manifest.extracted_count();
    let extracting = manifest.extracting_count();
    let pending = manifest.pending_count();
    let failed = manifest.failed_count();
    let progress = manifest.progress_percent();

    println!("================================================================================");
    println!("                              UNPACKR JOB STATUS                                ");
    println!("================================================================================");
    println!("Job ID:               {}", manifest.job_id);
    println!("Archive Path:         {}", manifest.archive.path.display());
    println!("Archive Size:         {}", format_bytes(manifest.archive.size));
    println!("Archive Identity:     {}", manifest.archive.identity);
    println!("Destination:          {}", manifest.destination.display());
    println!("Manifest File:        {}", manifest_path.display());
    println!("Progress:             {:>5.1}%", progress);
    println!("--------------------------------------------------------------------------------");
    println!("Entry Counts (Total: {}):", total);
    println!("  Verified:           {}", verified);
    println!("  Reclaimed:          {}", reclaimed);
    println!("  Skipped:            {}", skipped);
    println!("  Extracted:          {}", extracted);
    println!("  Extracting:         {}", extracting);
    println!("  Pending:            {}", pending);
    println!("  Failed:             {}", failed);
    println!("--------------------------------------------------------------------------------");
    println!("Data Totals:");
    println!("  Total Expected:     {}", format_bytes(manifest.total_uncompressed_bytes()));
    println!("  Verified on Disk:   {}", format_bytes(manifest.verified_uncompressed_bytes()));

    if failed > 0 {
        println!("--------------------------------------------------------------------------------");
        println!("Failed Entries:");
        for entry in manifest.entries.values() {
            if let crate::archive::EntryState::Failed(reason) = &entry.state {
                println!("  - {}: {}", entry.name, reason);
            }
        }
    }

    println!("================================================================================");
    Ok(())
}

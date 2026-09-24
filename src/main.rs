use anyhow::Result;
use clap::Parser;
use unpackr::cli::{Cli, Commands};

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Inspect { archive, json, limit } => {
            let limit_opt = if limit == 0 { None } else { Some(limit) };
            unpackr::cli::inspect::run_inspect(&archive, json, limit_opt)?;
        }
        Commands::Extract {
            archive,
            destination,
            collision,
            no_sparse,
            max_ratio,
            reclaim_archive,
            max_total_size,
            max_file_size,
            max_entries,
            state_dir,
            json,
        } => {
            let collision_policy = unpackr::extraction::CollisionPolicy::from_str_lossy(&collision);
            let options = unpackr::extraction::ExtractionOptions {
                destination: destination.clone(),
                collision_policy,
                enable_sparse: !no_sparse,
                max_compression_ratio: max_ratio,
                reclaim_archive,
                state_dir,
                verbose: cli.verbose,
                quiet: cli.quiet || json,
                max_total_size,
                max_file_size,
                max_entries,
            };

            let summary = unpackr::extraction::ExtractionEngine::extract(&archive, &options)?;

            if json {
                println!("{}", serde_json::to_string_pretty(&summary)?);
            } else {
                println!("================================================================================");
                println!("                           UNPACKR EXTRACTION COMPLETE                          ");
                println!("================================================================================");
                println!("Job ID:               {}", summary.job_id);
                println!("Archive:              {}", summary.archive_path.display());
                println!("Destination:          {}", summary.destination.display());
                println!("Manifest:             {}", summary.manifest_path.display());
                println!("Extracted Files:      {}", summary.extracted_files);
                println!("Created Directories:  {}", summary.created_directories);
                println!("Skipped Files:        {}", summary.skipped_files);
                println!("Data Written:         {}", unpackr::cli::inspect::format_bytes(summary.total_uncompressed_bytes));
                if summary.sparse_bytes_saved > 0 {
                    println!("Sparse Space Saved:   {}", unpackr::cli::inspect::format_bytes(summary.sparse_bytes_saved));
                }
                if summary.reclaimed_archive_bytes > 0 {
                    println!("Archive Reclaimed:    {}", unpackr::cli::inspect::format_bytes(summary.reclaimed_archive_bytes));
                }
                println!("Peak Disk Footprint:  {}", unpackr::cli::inspect::format_bytes(summary.peak_disk_footprint_bytes));
                println!("Throughput:           {:.1} MB/s", summary.throughput_mb_per_sec);
                println!("Duration:             {:.2?}", summary.duration);
                println!("================================================================================");
            }
        }
        Commands::Resume {
            target,
            destination,
            archive,
            retry_failed,
            verify,
            collision,
            reclaim_archive,
            max_total_size,
            max_file_size,
            max_entries,
            json,
        } => {
            unpackr::cli::resume::run_resume(
                &target,
                destination,
                archive,
                retry_failed,
                verify,
                collision,
                reclaim_archive,
                max_total_size,
                max_file_size,
                max_entries,
                json,
                cli.verbose,
                cli.quiet,
            )?;
        }
        Commands::Status { job_id, json } => {
            unpackr::cli::status::run_status(&job_id, json)?;
        }
        Commands::Verify { job_id, json } => {
            unpackr::cli::verify::run_verify(&job_id, json)?;
        }
        Commands::Cancel {
            job_id,
            clean,
            json,
        } => {
            unpackr::cli::cancel::run_cancel(&job_id, clean, json)?;
        }
        Commands::Bench {
            archive,
            entries,
            size_mb,
            json,
        } => {
            unpackr::cli::bench::run_bench(archive, entries, size_mb, json, cli.verbose)?;
        }
    }

    Ok(())
}

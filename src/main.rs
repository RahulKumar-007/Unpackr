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
                println!("Sparse Space Saved:   {}", unpackr::cli::inspect::format_bytes(summary.sparse_bytes_saved));
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
            json,
        } => {
            unpackr::cli::resume::run_resume(
                &target,
                destination,
                archive,
                retry_failed,
                verify,
                collision,
                json,
                cli.verbose,
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
    }

    Ok(())
}

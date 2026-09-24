pub mod inspect;

use std::path::PathBuf;
use clap::{Parser, Subcommand};

#[derive(Parser, Debug)]
#[command(
    name = "unpackr",
    author = "Unpackr Authors",
    version = "0.1.0",
    about = "Production-quality, low-disk-space archive extraction engine",
    long_about = "Unpackr extracts large ZIP archives while minimizing peak disk space usage through incremental verified extraction and storage reclamation."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Verbose logging output
    #[arg(short, long, global = true)]
    pub verbose: bool,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Inspect an archive and display detailed entry metadata and offsets without extracting
    Inspect {
        /// Path to the target archive (.zip)
        archive: PathBuf,

        /// Output inspection details in JSON format
        #[arg(long)]
        json: bool,

        /// Limit the number of entries displayed in the table (default: 50)
        #[arg(long, default_value = "50")]
        limit: usize,
    },

    /// Extract an archive with streaming verification and low disk space usage
    Extract {
        /// Path to the archive
        archive: PathBuf,

        /// Destination directory
        destination: PathBuf,

        /// Collision policy if destination files exist: fail, skip, overwrite, rename (default: fail)
        #[arg(long, default_value = "fail")]
        collision: String,

        /// Disable sparse file hole detection
        #[arg(long)]
        no_sparse: bool,

        /// Maximum allowed compression ratio before aborting (zip bomb protection, default: 100.0)
        #[arg(long, default_value = "100.0")]
        max_ratio: f64,

        /// Reclaim archive storage in-place during extraction (experimental/destructive mode)
        #[arg(long)]
        reclaim_archive: bool,

        /// Custom state directory for crash recovery journals
        #[arg(long)]
        state_dir: Option<PathBuf>,

        /// Output extraction summary in JSON format
        #[arg(long)]
        json: bool,
    },

    /// Resume an interrupted extraction job
    Resume {
        /// Job ID or archive path to resume
        job_id: String,
    },

    /// Show current status and disk savings for a job
    Status {
        /// Job ID to inspect
        job_id: String,
    },

    /// Verify an extracted destination against archive metadata
    Verify {
        /// Job ID or archive path
        job_id: String,
    },

    /// Cancel a running job and clean up temporary state
    Cancel {
        /// Job ID to cancel
        job_id: String,
    },
}

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[cfg(feature = "bench")]
use crate::cli::inspect::format_bytes;
#[cfg(feature = "bench")]
use crate::extraction::{CollisionPolicy, ExtractionEngine, ExtractionOptions};
#[cfg(feature = "bench")]
use anyhow::Context;
#[cfg(feature = "bench")]
use std::fs::{self, File};
#[cfg(feature = "bench")]
use std::io::Write;
#[cfg(feature = "bench")]
use std::path::Path;
#[cfg(feature = "bench")]
use tempfile::tempdir;
#[cfg(feature = "bench")]
use zip::write::SimpleFileOptions;
#[cfg(feature = "bench")]
use zip::ZipWriter;

#[derive(Debug, Serialize, Deserialize)]
pub struct BenchmarkReport {
    pub workload: String,
    pub total_entries: usize,
    pub uncompressed_bytes: u64,
    pub standard_peak_bytes: u64,
    pub standard_duration_secs: f64,
    pub standard_throughput_mb_s: f64,
    pub reclaim_peak_bytes: u64,
    pub reclaim_duration_secs: f64,
    pub reclaim_throughput_mb_s: f64,
    pub peak_space_saved_bytes: u64,
    pub peak_space_saved_percent: f64,
    pub archive_bytes_reclaimed: u64,
    pub integrity_verified: bool,
}

#[cfg(feature = "bench")]
fn generate_synthetic_archive(
    path: &Path,
    num_entries: usize,
    entry_size_bytes: usize,
) -> Result<()> {
    let file = File::create(path)
        .with_context(|| format!("Failed to create benchmark archive at {:?}", path))?;
    let mut zip = ZipWriter::new(file);

    for i in 0..num_entries {
        let name = format!("bench_entry_{:03}.dat", i);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        zip.start_file(&name, opts)?;

        // Generate non-zero pseudo-random blocks so filesystem allocates physical disk storage
        let chunk_size = 4096;
        let num_chunks = entry_size_bytes / chunk_size;
        let mut chunk = vec![0u8; chunk_size];
        for c in 0..num_chunks {
            let byte_val = ((i * 37 + c * 13 + 1) % 251 + 1) as u8;
            chunk.fill(byte_val);
            zip.write_all(&chunk)?;
        }
    }

    zip.finish()?;
    Ok(())
}

#[cfg(feature = "bench")]
pub fn run_bench(
    archive: Option<PathBuf>,
    entries: usize,
    size_mb: usize,
    json: bool,
    verbose: bool,
) -> Result<()> {
    let temp_workspace =
        tempdir().with_context(|| "Failed to create temporary benchmark workspace")?;
    let (archive_std, archive_rec, workload_name) = match archive {
        Some(user_archive) => {
            if !user_archive.is_file() {
                anyhow::bail!("Specified archive does not exist: {:?}", user_archive);
            }
            let copy_path = temp_workspace.path().join("archive_reclaim_copy.zip");
            fs::copy(&user_archive, &copy_path).with_context(|| {
                format!(
                    "Failed to stage copy of {:?} for reclamation test",
                    user_archive
                )
            })?;
            let name = format!(
                "Custom Archive ({})",
                user_archive
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            );
            (user_archive, copy_path, name)
        }
        None => {
            let entries_count = entries.max(1);
            let mb = size_mb.max(1);
            let synth_std = temp_workspace.path().join("synthetic_std.zip");
            let synth_rec = temp_workspace.path().join("synthetic_rec.zip");

            if !json {
                println!(
                    "Generating synthetic benchmark workload ({} entries x {} MB)...",
                    entries_count, mb
                );
            }
            generate_synthetic_archive(&synth_std, entries_count, mb * 1024 * 1024)?;
            fs::copy(&synth_std, &synth_rec)?;

            let name = format!(
                "Synthetic Workload ({} entries x {} MB = {} MB total)",
                entries_count,
                mb,
                entries_count * mb
            );
            (synth_std, synth_rec, name)
        }
    };

    let dest_std = temp_workspace.path().join("dest_standard");
    let dest_rec = temp_workspace.path().join("dest_reclaim");

    if !json {
        println!("Running Standard Extraction (reclaim: false)...");
    }
    let opts_std = ExtractionOptions {
        destination: dest_std.clone(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 200.0,
        reclaim_archive: false,
        state_dir: None,
        verbose,
        quiet: json,
        ..Default::default()
    };
    let summary_std = ExtractionEngine::extract(&archive_std, &opts_std)?;

    if !json {
        println!("Running In-Place Reclaim Extraction (reclaim: true)...");
    }
    let opts_rec = ExtractionOptions {
        destination: dest_rec.clone(),
        collision_policy: CollisionPolicy::Fail,
        enable_sparse: true,
        max_compression_ratio: 200.0,
        reclaim_archive: true,
        state_dir: None,
        verbose,
        quiet: json,
        ..Default::default()
    };
    let summary_rec = ExtractionEngine::extract(&archive_rec, &opts_rec)?;

    // Verify byte-level integrity across extracted outputs (ignoring internal .unpackr state directory)
    let mut integrity_verified = true;
    for entry in walkdir::WalkDir::new(&dest_std)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        if entry.file_type().is_file() {
            let rel_path = entry.path().strip_prefix(&dest_std)?;
            if rel_path.starts_with(".unpackr") {
                continue;
            }
            let rec_file = dest_rec.join(rel_path);
            if !rec_file.is_file() {
                integrity_verified = false;
                break;
            }
            let data_std = fs::read(entry.path())?;
            let data_rec = fs::read(&rec_file)?;
            if data_std != data_rec {
                integrity_verified = false;
                break;
            }
        }
    }

    let peak_saved = summary_std
        .peak_disk_footprint_bytes
        .saturating_sub(summary_rec.peak_disk_footprint_bytes);
    let pct_saved = if summary_std.peak_disk_footprint_bytes > 0 {
        (peak_saved as f64 / summary_std.peak_disk_footprint_bytes as f64) * 100.0
    } else {
        0.0
    };

    let report = BenchmarkReport {
        workload: workload_name,
        total_entries: summary_std.extracted_files,
        uncompressed_bytes: summary_std.total_uncompressed_bytes,
        standard_peak_bytes: summary_std.peak_disk_footprint_bytes,
        standard_duration_secs: summary_std.duration.as_secs_f64(),
        standard_throughput_mb_s: summary_std.throughput_mb_per_sec,
        reclaim_peak_bytes: summary_rec.peak_disk_footprint_bytes,
        reclaim_duration_secs: summary_rec.duration.as_secs_f64(),
        reclaim_throughput_mb_s: summary_rec.throughput_mb_per_sec,
        peak_space_saved_bytes: peak_saved,
        peak_space_saved_percent: pct_saved,
        archive_bytes_reclaimed: summary_rec.reclaimed_archive_bytes,
        integrity_verified,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!("================================================================================");
    println!("                           UNPACKR BENCHMARK REPORT                             ");
    println!("================================================================================");
    println!("Workload:               {}", report.workload);
    println!("Extracted Files:        {}", report.total_entries);
    println!(
        "Total Data Size:        {}",
        format_bytes(report.uncompressed_bytes)
    );
    println!(
        "Integrity Check:        {}",
        if report.integrity_verified {
            "PASSED (100% byte-for-byte fidelity)"
        } else {
            "FAILED (data mismatch)"
        }
    );
    println!("--------------------------------------------------------------------------------");
    println!(
        "{:<24} {:<20} {:<20} {:<16}",
        "Metric", "Standard Mode", "Reclaim Mode", "Improvement"
    );
    println!("--------------------------------------------------------------------------------");
    println!(
        "{:<24} {:<20} {:<20} -{:.1}% (-{})",
        "Peak Disk Footprint",
        format_bytes(report.standard_peak_bytes),
        format_bytes(report.reclaim_peak_bytes),
        report.peak_space_saved_percent,
        format_bytes(report.peak_space_saved_bytes)
    );
    println!(
        "{:<24} {:<20} {:<20} {:<16}",
        "Extraction Duration",
        format!("{:.2}s", report.standard_duration_secs),
        format!("{:.2}s", report.reclaim_duration_secs),
        format!(
            "{:+.2}s",
            report.reclaim_duration_secs - report.standard_duration_secs
        )
    );
    println!(
        "{:<24} {:<20} {:<20} {:<16}",
        "Decompress Throughput",
        format!("{:.1} MB/s", report.standard_throughput_mb_s),
        format!("{:.1} MB/s", report.reclaim_throughput_mb_s),
        "Optimal"
    );
    println!(
        "{:<24} {:<20} {:<20} {:<16}",
        "Source Storage Freed",
        "0 B",
        format_bytes(report.archive_bytes_reclaimed),
        "Reclaimed in-place"
    );
    println!("================================================================================");

    Ok(())
}

#[cfg(not(feature = "bench"))]
pub fn run_bench(
    _archive: Option<PathBuf>,
    _entries: usize,
    _size_mb: usize,
    _json: bool,
    _verbose: bool,
) -> Result<()> {
    anyhow::bail!("Benchmark command requires compiling with `--features bench`");
}

use std::path::Path;
use anyhow::Result;
use crate::archive::ZipInspector;

pub fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = KB * 1024;
    const GB: u64 = MB * 1024;

    if bytes >= GB {
        format!("{:.2} GB ({} B)", bytes as f64 / GB as f64, bytes)
    } else if bytes >= MB {
        format!("{:.2} MB ({} B)", bytes as f64 / MB as f64, bytes)
    } else if bytes >= KB {
        format!("{:.2} KB ({} B)", bytes as f64 / KB as f64, bytes)
    } else {
        format!("{} B", bytes)
    }
}

pub fn run_inspect(archive_path: &Path, json: bool, limit: Option<usize>) -> Result<()> {
    let inspection = ZipInspector::inspect(archive_path)?;

    if json {
        let json_str = serde_json::to_string_pretty(&inspection)?;
        println!("{}", json_str);
        return Ok(());
    }

    println!("================================================================================");
    println!("                           UNPACKR ARCHIVE INSPECTION                           ");
    println!("================================================================================");
    println!("Archive Path:         {}", inspection.path.display());
    println!("Logical File Size:    {}", format_bytes(inspection.file_size));
    println!("Archive Identity:     {}", inspection.identity);
    println!("Total Entries:        {}", inspection.total_entries);
    println!("Uncompressed Data:    {}", format_bytes(inspection.total_uncompressed_size));
    println!("Compressed Data:      {}", format_bytes(inspection.total_compressed_size));
    println!("Central Dir Offset:   0x{:08X} ({})", inspection.central_directory_offset, inspection.central_directory_offset);
    println!("Central Dir Size:     {}", format_bytes(inspection.central_directory_size));
    println!("Zip64 Extended:       {}", if inspection.is_zip64 { "Yes" } else { "No" });
    println!("Overlapping Streams:  {}", if inspection.has_overlapping_entries { "WARNING: Yes (in-place reclamation forbidden)" } else { "No (safe for reclamation)" });
    println!("--------------------------------------------------------------------------------");
    println!(
        "{:<5} {:<32} {:<10} {:<12} {:<12} {:<8} {:<10} {:<10}",
        "IDX", "PATH", "METHOD", "COMPRESSED", "UNCOMPRESSED", "SAVINGS", "CRC-32", "DATA OFFSET"
    );
    println!("--------------------------------------------------------------------------------");

    let max_display = limit.unwrap_or(inspection.entries.len());
    for entry in inspection.entries.iter().take(max_display) {
        let display_name = if entry.name.len() > 30 {
            format!("...{}", &entry.name[entry.name.len() - 27..])
        } else {
            entry.name.clone()
        };

        let method_str = match entry.compression_method {
            crate::archive::CompressionMethod::Stored => "Stored",
            crate::archive::CompressionMethod::Deflated => "Deflate",
            crate::archive::CompressionMethod::Unsupported(_v) => "Unknown",
        };

        println!(
            "{:<5} {:<32} {:<10} {:<12} {:<12} {:>6.1}%  0x{:08X} 0x{:08X}",
            entry.index,
            display_name,
            method_str,
            format_bytes(entry.compressed_size).split_whitespace().next().unwrap_or("-"),
            format_bytes(entry.uncompressed_size).split_whitespace().next().unwrap_or("-"),
            entry.savings_ratio(),
            entry.crc32,
            entry.data_offset
        );
    }

    if inspection.entries.len() > max_display {
        println!(
            "... and {} more entries (use --limit 0 to show all)",
            inspection.entries.len() - max_display
        );
    }

    println!("================================================================================");
    Ok(())
}

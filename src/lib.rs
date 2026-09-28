//! # Unpackr
//!
//! Production-quality, low-disk-space ZIP archive extraction engine with progressive
//! in-place storage reclamation, crash-resumable state journals, and a native desktop GUI.
//!
//! ## Core Modules
//! - [`archive`]: Central Directory parsing, entry metadata inspection, and overlap detection.
//! - [`extraction`]: Streaming decompression, CRC-32 verification, sparse hole detection, and atomic staging.
//! - [`reclaim`][reclamation]: Block-aligned (`4096`-byte) inward `fallocate(FALLOC_FL_PUNCH_HOLE)` storage reclamation.
//! - [`security`]: Zip Slip path sanitization, symlink poisoning defense, and decompression bomb guards.
//! - [`state`]: Crash-safe write-ahead manifest journals and job resumption tracking.
//! - [`cli`]: Command-line interface subcommands and benchmark runners.
//! - [`gui`]: Native desktop user interface powered by `eframe`/`egui`.

#![warn(clippy::all)]
#![warn(unused_imports, dead_code)]

pub mod archive;
pub mod cli;
pub mod extraction;
pub mod gui;
pub mod reclamation;
pub mod security;
pub mod state;

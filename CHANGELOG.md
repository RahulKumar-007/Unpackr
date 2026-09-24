# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.0] - 2026-09-24

### Security & Reliability Hardening
- **P0-01 Crash Safety**: Added explicit parent directory `fsync` following atomic manifest renames in `ExtractionManifest::save_atomic`.
- **P0-02 Path Sanitization**: Enforced strict UTF-8 decoding for ZIP entry names, rejecting non-UTF8 and `\u{FFFD}` sentinel bypasses.
- **P0-03 Inode Protection**: Configured default `max_entries` limit (100,000) to protect against inode exhaustion attacks.
- **P0-04 State Persistence**: Added warning logs and explicit tracking if `set_entry_failed()` persistence encounters I/O errors.
- **P1-01 I/O Overhead Reduction**: Introduced dirty count and time-based manifest checkpointing (saving every 100 entries or 1 second) rather than flushing on every entry.
- **P1-02 State Inversion Guard**: Set entries to `Verified` only after hole punching succeeds to ensure consistent state recovery.
- **P1-03 Symlink Handling**: Implemented safe symlink extraction validating targets against destination directory bounds.
- **P1-04 Canonicalization**: Canonicalized destination roots prior to traversal checks to mitigate symlink aliasing attacks.
- **P1-05 Dependency Hygiene**: Gated the `zip` crate behind the `bench` feature, moving test usage to dev-dependencies.
- **P1-06 CI/CD Pipeline**: Added GitHub Actions workflow (`.github/workflows/ci.yml`) checking formatting, clippy lints, and test suites.
- **P2-01 Permissions**: Stripped group-writable (`0o020`) bits along with world-writable and SUID/SGID bits.
- **P2-02 Robust CLI Enums**: Added strict `clap::ValueEnum` validation and error handling for `--collision` arguments.
- **P2-03 Traversal Protection**: Sanitized `job_id` against directory traversal characters before global job lookups.
- **P2-04 Fallocate Error Mapping**: Mapped `ENOTSUP` and `EINVAL` to `ReclamationError::UnsupportedFilesystem`.
- **P2-05 Filesystem Warning**: Added a one-time non-verbose warning when hole punching is unsupported (e.g., tmpfs).
- **P2-06 Manifest Version Validation**: Validated manifest version upon loading to prevent schema mismatches.
- **P2-07 Code Deduplication**: Unified extraction and resume processing loops into `process_entries()`.
- **P2-08 Tiered Bomb Defense**: Implemented tiered compression ratio checks across file sizes to catch sub-10MB zip bombs.
- **P2-09 Broken Pipe Handling**: Reset `SIGPIPE` to default behavior on Unix and handled `ErrorKind::BrokenPipe`.
- **P2-10 Resume Optimization**: Cached and reused archive inspection during resume identity verification.
- **P2-11 Allocation Elimination**: Replaced central directory comment string allocations with `Seek` skips.
- **P2-12 Memory Optimization**: Eliminated redundant duplicate entry name storage in manifest records.
- **P2-13 Destination Locking**: Implemented advisory file locking (`.lock`) to prevent concurrent extractions to the same destination.
- **P3-02 Deterministic Job IDs**: Removed wall-clock timestamps from Job IDs to derive deterministic identifiers from archive stem and identity prefix.
- **P3-08 Multi-disk Rejection**: Explicitly rejected multi-disk ZIP archives before processing.
- **P3-09/P3-10 SparseWriter SWAR**: Replaced unsafe pointer alignment with safe SWAR `as_chunks::<8>()` zero checks and added debug Drop guards.
- **P3-12 Documentation**: Documented default `<destination>/.unpackr/` path for `--state-dir`.
- **P3-14 Windows Devices**: Added `COM0`, `LPT0`, `CONIN$`, and `CONOUT$` to reserved Windows device name checks.
- **P3-16 Lint Attributes**: Added `#![warn(clippy::all)]` and `#![warn(unused_imports, dead_code)]`.
- **P3-03–P3-07 Test Coverage**: Added comprehensive integration tests for ZIP64 inspection, unsupported compression errors, CRC-32 mismatches, CLI JSON formats, and shell completions.

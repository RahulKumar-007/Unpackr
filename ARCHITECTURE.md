# Unpackr: Architectural Specification & Technical Design

## 1. Executive Summary & Problem Formulation

Standard archive extraction tools (`unzip`, `tar`, `7z`, etc.) assume that disk space is an abundant, cheap commodity:
$$\text{Peak Disk Required} \approx \text{Archive Size} + \text{Total Extracted Size} + \text{Overhead}$$

For a 100 GB compressed archive expanding to 100 GB of decompressed files, a user must have at least 200 GB of free space available throughout the entire extraction duration. In disk-constrained environments (embedded devices, edge servers, gaming systems, consumer laptops with small SSDs), this prevents extraction even when the final unpacked files would easily fit on the volume (e.g., 120 GB of free space).

**Unpackr** is a production-quality, crash-resilient extraction engine engineered in Rust to break this peak-disk constraint. Unpackr continuously tracks extraction progress, cryptographically verifies decompressed output on disk, and systematically reclaims the underlying source archive storage so that:
$$\text{Peak Disk Required} \approx \max(\text{Archive Size}, \text{Extracted Size}) + \delta_{\text{slack}}$$
where $\delta_{\text{slack}}$ represents the working buffer for the currently decompressing entry (typically $\le 4 \text{ KB} - 64 \text{ MB}$) plus minimal metadata overhead.

---

## 2. ZIP Physical Format & Constraints

A standard ZIP archive is not a simple linear stream; it is a backward-referenced container formatted as follows:

```
[Local File Header 1] [Compressed Data 1] [Data Descriptor 1 (optional)]
[Local File Header 2] [Compressed Data 2] [Data Descriptor 2 (optional)]
...
[Local File Header N] [Compressed Data N] [Data Descriptor N (optional)]
[Central Directory Header 1]
[Central Directory Header 2]
...
[Central Directory Header N]
[Zip64 End of Central Directory Record (optional)]
[Zip64 End of Central Directory Locator (optional)]
[End of Central Directory Record (EOCD)]
```

### Key Signatures and Structures

1. **Local File Header (LFH)** — Signature `0x04034b50` (4 bytes):
   - Version needed (2 bytes), General purpose bit flag (2 bytes), Compression method (2 bytes: 0=Store, 8=Deflate).
   - Last mod time & date (4 bytes).
   - CRC-32 (4 bytes), Compressed size (4 bytes), Uncompressed size (4 bytes).
     *(Note: If Bit 3 of general purpose bit flag is set, CRC and sizes are zero here and stored in the trailing Data Descriptor).*
   - File name length $N$ (2 bytes), Extra field length $M$ (2 bytes).
   - Variable data: File name ($N$ bytes), Extra field ($M$ bytes).
   - Followed immediately by: **Compressed file payload**.

2. **Data Descriptor** — Optional signature `0x08074b50` (4 bytes) followed by CRC-32, compressed size, uncompressed size (4 bytes each, or 8 bytes each for Zip64).

3. **Central Directory File Header (CDFH)** — Signature `0x02014b50` (4 bytes):
   - Authoritative directory record. Contains version made by, version needed, flags, compression method, timestamps, CRC-32, compressed size, uncompressed size, filename length, extra field length, comment length, disk number, internal/external file attributes, and **Relative Offset of Local Header** (4 bytes, or extended in Zip64 extra field `0x0001`).

4. **Zip64 EOCD Record & Locator** — Signatures `0x06064b50` & `0x07064b50`:
   - Extends 32-bit offset and size limits for archives $> 4\text{ GB}$ or containing $> 65,535$ entries.

5. **End of Central Directory (EOCD)** — Signature `0x06054b50` (22 bytes minimum):
   - Located at the end of the archive (variable only due to optional archive comment up to 65,535 bytes).
   - Contains: disk number, disk with start of CD, number of CD records on disk, total CD records, size of CD, and **Offset of start of CD with respect to starting disk number**.

---

## 3. Evaluation of In-Place Storage Reclamation vs. ZIP Validity

A central requirement of Unpackr is determining whether and how source archive storage can be reclaimed without silent corruption or unsafe assumptions.

### 3.1 Can Arbitrary Byte-Range Deletion Preserve a Valid ZIP?

**No.**
1. **Offset Invalidation**: Every Central Directory entry contains the absolute byte offset of its corresponding Local File Header. If bytes belonging to Entry 1 are physically removed by shifting subsequent bytes down (e.g., using `FALLOC_FL_COLLAPSE_RANGE`), all subsequent relative offsets stored in the Central Directory are invalidated. Standard ZIP parsers will immediately fail with offset mismatch or bad signature errors.
2. **Hole Punching Destroys Data Content**: Punching holes (`FALLOC_FL_PUNCH_HOLE`) preserves file length and relative offsets by releasing physical allocation blocks back to the filesystem, leaving a sparse region that reads as zeroes (`0x00`). While this keeps the container offset structure intact, standard tools reading that entry will receive zeroed bytes instead of the compressed payload, immediately triggering DEFLATE decompression errors or CRC-32 validation failure.
3. **Block Boundary Alignment Constraints**: Linux filesystems (ext4, XFS, Btrfs) allocate storage in blocks (almost universally 4096 bytes). `fallocate(FALLOC_FL_PUNCH_HOLE)` requires that the punched range be aligned to the filesystem block size. If Entry A ends at byte 4097 and Entry B begins at byte 4098, punching byte 4096..8192 would destroy the header or data of Entry B!

### 3.2 Operating Modes

To strictly adhere to correctness and safety, Unpackr formalizes two explicit modes:

#### Mode 1: Safe Non-Destructive Mode (Default)
- **Constraint**: The user-provided `.zip` file is opened **READ-ONLY**.
- **Working Storage**: Temporary working representations or staged extractions are managed independently.
- **Disk Accounting**: The engine reports real disk usage and warns if the target filesystem has insufficient space for both archive and output.

#### Mode 2: In-Place Sparse Reclamation Mode (`--reclaim-archive` / `--in-place`)
- **Explicit Intent**: The user explicitly authorizes Unpackr to consume the archive file in place during extraction to fit within a tight disk budget.
- **Safe Block Inset Punching**:
  Given an entry's compressed data range $[D_{\text{start}}, D_{\text{end}})$, the reclaimable range is calculated by aligning **inward** to the nearest filesystem block boundary:
  $$R_{\text{start}} = \left\lceil \frac{D_{\text{start}}}{\text{BlockSize}} \right\rceil \times \text{BlockSize}$$
  $$R_{\text{end}} = \left\lfloor \frac{D_{\text{end}}}{\text{BlockSize}} \right\rfloor \times \text{BlockSize}$$
  If $R_{\text{end}} > R_{\text{start}}$, `fallocate(fd, FALLOC_FL_PUNCH_HOLE | FALLOC_FL_KEEP_SIZE, R_{\text{start}}, R_{\text{end}} - R_{\text{start}})` is executed.
  - Neighboring entries are **never touched** because the inward rounding guarantees zero overlap with prior or subsequent entries.
  - The archive remains parseable (EOCD and Central Directory at the end remain 100% intact), but extracted entries are physically reclaimed as sparse holes on disk.
  - Once all entries are verified, the empty archive is cleanly unlinked.

---

## 4. Extraction State Machine & Crash Recovery

Extraction correctness is governed by an append-only Write-Ahead Log (WAL) / Journal and an atomic manifest.

```
       ┌──────────┐
       │ PENDING  │
       └────┬─────┘
            │ start_extraction()
            ▼
      ┌───────────┐
      │EXTRACTING │
      └─────┬─────┘
            │ output stream complete
            ▼
      ┌───────────┐
      │ EXTRACTED │
      └─────┬─────┘
            │ crc32_match && fsync()
            ▼
      ┌───────────┐
      │ VERIFIED  │
      └─────┬─────┘
            │ block-aligned punch (if reclaim enabled)
            ▼
      ┌───────────┐
      │ RECLAIMED │
      └───────────┘
```

### State Definitions
1. **`PENDING`**: Entry discovered in Central Directory; extraction not yet attempted.
2. **`EXTRACTING`**: Output file opened with `.unpackr_tmp` suffix. Decompressed bytes are actively streaming.
3. **`EXTRACTED`**: Entire stream decompressed, byte count matches uncompressed size. File flushed to OS buffers.
4. **`VERIFIED`**: Output CRC-32 strictly verified against Central Directory CRC. Output file atomically renamed to final target name. File fsync'd. **Only after this transition is source storage eligible for reclamation.**
5. **`RECLAIMED`**: Source storage blocks safely punched or freed.
6. **`FAILED`**: Extraction or verification failed (CRC mismatch, I/O error, disk full). Temporary files cleaned up; source data preserved.

### Archive Identity Without Hashing 100 GB
To detect if the input archive was modified or replaced during an interruption without hashing hundreds of gigabytes:
$$\text{ArchiveIdentity} = \text{BLAKE3}\Big(\text{FileSize} \,\|\, \text{mtime} \,\|\, \text{First 64KB} \,\|\, \text{Central Directory + EOCD (Tail)}\Big)$$
Because any archive modification changes either the file size, modification time, or Central Directory records, this identity check completes in $< 5 \text{ ms}$ even on a 500 GB file while providing cryptographic collision resistance.

---

## 5. Streaming Pipeline & Memory Bounds

Memory usage is strictly $O(1)$ relative to file and archive sizes.

```
┌─────────────────┐
│ Archive on Disk │
└────────┬────────┘
         │ bounded chunk read (64 KB)
         ▼
┌─────────────────┐
│ Bounded Reader  │
└────────┬────────┘
         │ streaming decompress (flate2::DeflateDecoder)
         ▼
┌─────────────────┐
│ Streaming CRC32 │ ──> calculates running CRC32 (crc32fast)
└────────┬────────┘
         │ bounded chunk write (64 KB)
         ▼
┌─────────────────┐
│ Output Target   │
└─────────────────┘
```

- Peak RAM allocation per worker thread is bounded by buffer sizes:
  $$\text{RAM}_{\text{thread}} \approx 64 \text{ KB (in)} + 32 \text{ KB (inflate window)} + 64 \text{ KB (out)} \approx 160 \text{ KB}$$
- Multi-gigabyte entries stream continuously without intermediate memory accumulation.

---

## 6. Security & Path Sanitization

Archives are treated as hostile input.

1. **Zip Slip Prevention**:
   - Every entry path is stripped of leading slashes, drive specifiers, and UNC paths.
   - Path components are analyzed: `.` is skipped; `..` returns a strict validation error.
   - The resolved target path is checked to ensure:
     $$\text{canonicalize}(\text{target\_path}).\text{starts\_with}(\text{canonicalize}(\text{dest\_dir}))$$
2. **Symlink Traversal Attack Prevention**:
   - Symlinks pointing outside `dest_dir` are rejected by default.
   - Symlinks are never followed when resolving extraction write destinations.
3. **Decompression Bomb Mitigation**:
   - Configurable maximum compression ratio threshold (default: $100:1$).
   - Absolute uncompressed file size cap configurable via CLI.
   - Output byte counter aborts immediately if written bytes exceed `uncompressed_size`.

---

## 7. Disk Space Accounting & Measurement

Instead of theoretical estimations, Unpackr queries the filesystem using `statvfs` (Linux/Unix):
- `f_bavail * f_frsize`: Actual free disk space available to unprivileged user.
- Tracking metrics:
  - `archive_total_bytes`: Total logical size of archive.
  - `archive_allocated_bytes`: Actual physical blocks consumed by archive (`st_blocks * 512`).
  - `extracted_bytes`: Total uncompressed bytes written to destination.
  - `reclaimed_bytes`: Physical archive bytes freed via hole punching.
  - `peak_disk_delta`: Maximum measured expansion above initial baseline.

---

## 8. Implementation Phases

- **Phase 1: ZIP Archive Inspector**
  - Robust Central Directory & EOCD parser (including Zip64).
  - CLI `unpackr inspect <archive.zip>` producing rich JSON / formatted table of entries, compression methods, offsets, sizes, and CRCs.
- **Phase 2: Streaming Extraction Engine**
  - Safe path resolution, bounded streaming decompressor, CRC-32 verification, atomic rename.
- **Phase 3: State Tracking & Journaling**
  - Append-only WAL, archive identity validation, state transitions.
- **Phase 4: Crash Recovery & Resumability**
  - Interrupted job recovery, orphan temp file cleanup, seamless resume.
- **Phase 5: Working Storage & Block-Aligned Reclamation**
  - Linux `fallocate` hole punching engine with block-boundary inset calculation.
- **Phase 6: Live Metrics & Disk Accounting**
  - Real filesystem space querying, peak tracking, terminal progress UI.
- **Phase 7: Performance Benchmarks & Stress Tests**
  - Benchmarking streaming throughput and peak disk requirements against standard tools.
- **Phase 8: Security Hardening**
  - Fuzz testing against Zip Slip, zip bombs, and corrupted headers.
- **Phase 9: CLI Polish & Full Documentation**

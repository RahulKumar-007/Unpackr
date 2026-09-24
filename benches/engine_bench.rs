use criterion::{black_box, criterion_group, criterion_main, Criterion};
use std::fs::File;
use std::io::Write;
use tempfile::tempdir;
use unpackr::archive::compute_archive_identity;
use unpackr::extraction::SparseWriter;
use unpackr::reclamation::puncher::compute_inward_reclaim_range;
use unpackr::security::sanitize_entry_path;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

fn bench_inward_reclaim_calculation(c: &mut Criterion) {
    let mut group = c.benchmark_group("reclaim_math");
    group.bench_function("compute_inward_reclaim_range_aligned", |b| {
        b.iter(|| {
            compute_inward_reclaim_range(
                black_box(4096),
                black_box(16384),
                black_box(4096),
            )
        })
    });

    group.bench_function("compute_inward_reclaim_range_unaligned", |b| {
        b.iter(|| {
            compute_inward_reclaim_range(
                black_box(1234),
                black_box(65536),
                black_box(4096),
            )
        })
    });

    group.bench_function("compute_inward_reclaim_range_sub_block", |b| {
        b.iter(|| {
            compute_inward_reclaim_range(
                black_box(500),
                black_box(2000),
                black_box(4096),
            )
        })
    });
    group.finish();
}

fn bench_path_sanitization(c: &mut Criterion) {
    let base = std::path::Path::new("/tmp/unpackr_dest");
    let mut group = c.benchmark_group("security_path");

    group.bench_function("sanitize_normal_path", |b| {
        b.iter(|| sanitize_entry_path(black_box("folder/subfolder/file.txt")))
    });

    let sanitized = std::path::Path::new("a/b/c/d/e/f/g/h/resource.dat");
    group.bench_function("resolve_safe_dest", |b| {
        b.iter(|| {
            unpackr::security::resolve_safe_dest(
                black_box(base),
                black_box(sanitized),
            )
        })
    });
    group.finish();
}

fn bench_sparse_writer_throughput(c: &mut Criterion) {
    let dir = tempdir().unwrap();
    let mut group = c.benchmark_group("sparse_writer");

    let zero_buffer = vec![0u8; 64 * 1024];
    let data_buffer = vec![0x42u8; 64 * 1024];

    group.bench_function("sparse_write_zeros_64kb", |b| {
        let file_path = dir.path().join("zeros.dat");
        b.iter(|| {
            let file = File::create(&file_path).unwrap();
            let mut writer = SparseWriter::new(file, true);
            writer.write_chunk(black_box(&zero_buffer)).unwrap();
            writer.finish(64 * 1024).unwrap();
        })
    });

    group.bench_function("sparse_write_data_64kb", |b| {
        let file_path = dir.path().join("data.dat");
        b.iter(|| {
            let file = File::create(&file_path).unwrap();
            let mut writer = SparseWriter::new(file, true);
            writer.write_chunk(black_box(&data_buffer)).unwrap();
            writer.finish(64 * 1024).unwrap();
        })
    });
    group.finish();
}

fn bench_archive_identity(c: &mut Criterion) {
    let dir = tempdir().unwrap();
    let archive_path = dir.path().join("sample.zip");

    // Create a 1 MB archive
    {
        let file = File::create(&archive_path).unwrap();
        let mut zip = ZipWriter::new(file);
        zip.start_file("test.dat", SimpleFileOptions::default()).unwrap();
        let payload = vec![0xAA; 1024 * 1024];
        zip.write_all(&payload).unwrap();
        zip.finish().unwrap();
    }

    let mut group = c.benchmark_group("archive_identity");
    group.bench_function("blake3_archive_identity_1mb", |b| {
        b.iter(|| compute_archive_identity(black_box(&archive_path)))
    });
    group.finish();
}

criterion_group!(
    benches,
    bench_inward_reclaim_calculation,
    bench_path_sanitization,
    bench_sparse_writer_throughput,
    bench_archive_identity
);
criterion_main!(benches);

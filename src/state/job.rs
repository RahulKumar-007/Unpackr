use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobId(pub String);

impl JobId {
    /// Generates a human-readable, unique Job ID based on the archive name,
    /// an 8-character identity prefix, and timestamp.
    pub fn generate(archive_path: &Path, identity_hash: &str) -> Self {
        let stem = archive_path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("job")
            .chars()
            .filter(|c| c.is_alphanumeric() || *c == '-' || *c == '_')
            .collect::<String>();

        let short_id = if identity_hash.len() >= 8 {
            &identity_hash[..8]
        } else {
            "00000000"
        };

        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        JobId(format!("{}_{}_{}", stem, short_id, timestamp))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Returns the default global directory for storing Unpackr job state: `~/.unpackr/jobs`
pub fn global_jobs_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .map(|home| home.join(".unpackr").join("jobs"))
}

use crate::state::manifest::ExtractionManifest;

/// Locates a manifest file given a job ID, destination directory, or direct file path.
pub fn find_manifest_file(job_id_or_path: &str) -> Option<PathBuf> {
    let direct_path = PathBuf::from(job_id_or_path);

    // 1. Direct path to manifest file
    if direct_path.is_file() {
        return Some(direct_path);
    }

    // 2. Direct path to a directory containing manifest.json or .unpackr/manifest.json
    if direct_path.is_dir() {
        let m1 = direct_path.join(".unpackr").join("manifest.json");
        if m1.is_file() {
            return Some(m1);
        }
        let m2 = direct_path.join("manifest.json");
        if m2.is_file() {
            return Some(m2);
        }
    }

    // 3. Global job directory `~/.unpackr/jobs/<job-id>/manifest.json`
    if let Some(global_dir) = global_jobs_dir() {
        let global_manifest = global_dir.join(job_id_or_path).join("manifest.json");
        if global_manifest.is_file() {
            if let Ok(manifest) = ExtractionManifest::load(&global_manifest) {
                let dest_manifest = manifest.destination.join(".unpackr").join("manifest.json");
                if dest_manifest.is_file() {
                    return Some(dest_manifest);
                }
            }
            return Some(global_manifest);
        }
    }

    None
}

/// Locates a manifest file given a target (job ID, path, or archive) and optional destination directory.
pub fn find_manifest_for_job(target: &str, destination: Option<&Path>) -> Option<PathBuf> {
    // 1. If explicit destination is provided, check there first
    if let Some(dest) = destination {
        let m1 = dest.join(".unpackr").join("manifest.json");
        if m1.is_file() {
            return Some(m1);
        }
        let m2 = dest.join("manifest.json");
        if m2.is_file() {
            return Some(m2);
        }
    }

    // 2. Check target directly (path, directory, or job ID)
    if let Some(path) = find_manifest_file(target) {
        return Some(path);
    }

    None
}


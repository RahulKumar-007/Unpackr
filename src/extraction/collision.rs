use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, clap::ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum CollisionPolicy {
    /// Abort extraction with an error if the destination file exists (conservative default)
    #[default]
    Fail,
    /// Skip the entry if the destination file already exists
    Skip,
    /// Overwrite the existing destination file atomically after verification
    Overwrite,
    /// Rename the destination file (e.g. `file.1.txt`, `file.2.txt`)
    Rename,
}

impl std::str::FromStr for CollisionPolicy {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "fail" => Ok(CollisionPolicy::Fail),
            "overwrite" => Ok(CollisionPolicy::Overwrite),
            "skip" => Ok(CollisionPolicy::Skip),
            "rename" => Ok(CollisionPolicy::Rename),
            other => Err(format!(
                "Invalid collision policy '{}'. Expected 'fail', 'skip', 'overwrite', or 'rename'.",
                other
            )),
        }
    }
}

impl CollisionPolicy {
    pub fn from_str_lossy(s: &str) -> Self {
        s.parse().unwrap_or(CollisionPolicy::Fail)
    }

    /// Resolves an available target path according to the collision policy.
    /// Returns `None` if the entry should be skipped.
    pub fn resolve_collision(&self, target_path: &Path) -> Result<Option<PathBuf>, String> {
        if !target_path.exists() {
            return Ok(Some(target_path.to_path_buf()));
        }

        match self {
            CollisionPolicy::Fail => Err(format!(
                "Destination file already exists: {:?}",
                target_path
            )),
            CollisionPolicy::Skip => Ok(None),
            CollisionPolicy::Overwrite => Ok(Some(target_path.to_path_buf())),
            CollisionPolicy::Rename => {
                let parent = target_path.parent().unwrap_or_else(|| Path::new(""));
                let stem = target_path
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("file");
                let extension = target_path.extension().and_then(|e| e.to_str());

                for i in 1..=10000 {
                    let new_name = match extension {
                        Some(ext) => format!("{}.{}.{}", stem, i, ext),
                        None => format!("{}.{}", stem, i),
                    };
                    let candidate = parent.join(new_name);
                    if !candidate.exists() {
                        return Ok(Some(candidate));
                    }
                }
                Err(format!(
                    "Exhausted unique rename candidates for {:?}",
                    target_path
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_collision_policies() {
        let tmp = NamedTempFile::new().unwrap();
        let path = tmp.path();

        // Fail
        assert!(CollisionPolicy::Fail.resolve_collision(path).is_err());

        // Skip
        assert_eq!(CollisionPolicy::Skip.resolve_collision(path).unwrap(), None);

        // Overwrite
        assert_eq!(
            CollisionPolicy::Overwrite.resolve_collision(path).unwrap(),
            Some(path.to_path_buf())
        );

        // Rename
        let renamed = CollisionPolicy::Rename
            .resolve_collision(path)
            .unwrap()
            .unwrap();
        assert_ne!(renamed, path.to_path_buf());
        assert!(!renamed.exists());
    }
}

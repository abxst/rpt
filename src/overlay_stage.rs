//! Linux OverlayFS staging preparation. No dpkg execution or live-root commit.
//! Mounting and dpkg execution require separate namespace isolation; this module
//! intentionally does not provide a mount executor.
use anyhow::{bail, Context, Result};
use std::{fs, path::{Path, PathBuf}};

#[derive(Debug, Clone)]
pub struct OverlayStage {
    pub lower: PathBuf,
    pub upper: PathBuf,
    pub work: PathBuf,
    pub merged: PathBuf,
}

impl OverlayStage {
    /// Prepare an isolated directory layout. The upper and work directories
    /// must be on the same filesystem for OverlayFS to accept the mount.
    /// This does not mount anything or mutate the lower filesystem.
    pub fn prepare(lower: &Path, base: &Path) -> Result<Self> {
        let lower = fs::canonicalize(lower).context("resolving lower filesystem")?;
        if !lower.is_dir() { bail!("lower root must be a directory"); }
        fs::create_dir_all(base).context("creating overlay staging base")?;
        let base = fs::canonicalize(base)?;
        if base == lower || lower.starts_with(&base) || base.starts_with(&lower) {
            bail!("overlay base must not overlap the lower filesystem tree");
        }
        for name in ["upper", "work", "merged"] {
            let path = base.join(name);
            if path.exists() { bail!("staging path already exists: {}", path.display()); }
        }
        let upper = base.join("upper");
        let work = base.join("work");
        let merged = base.join("merged");
        fs::create_dir(&upper)?;
        fs::create_dir(&work)?;
        fs::create_dir(&merged)?;
        Ok(Self { lower, upper, work, merged })
    }

    pub fn is_mounted(&self) -> Result<bool> {
        let merged = self.merged.to_string_lossy();
        Ok(fs::read_to_string("/proc/self/mountinfo")?
            .lines().any(|line| line.split_whitespace().nth(4) == Some(merged.as_ref())))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[test]
    fn prepares_fresh_staging_layout_without_mounting() {
        let root = std::env::temp_dir().join(format!("rpt-overlay-test-{}-{}",
            std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let lower = root.join("lower");
        fs::create_dir_all(&lower).unwrap();
        fs::write(lower.join("original"), "unchanged").unwrap();
        let stage = OverlayStage::prepare(&lower, &root.join("stage")).unwrap();
        assert!(stage.upper.is_dir() && stage.work.is_dir() && stage.merged.is_dir());
        assert_eq!(fs::read_to_string(lower.join("original")).unwrap(), "unchanged");
        assert!(OverlayStage::prepare(&lower, &root.join("stage")).is_err());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn rejects_nested_lower_tree() {
        let root = std::env::temp_dir().join(format!("rpt-overlay-nested-{}",
            std::process::id()));
        let lower = root.join("lower");
        fs::create_dir_all(&lower).unwrap();
        assert!(OverlayStage::prepare(&lower, &lower.join("stage")).is_err());
        fs::remove_dir_all(root).unwrap();
    }
}

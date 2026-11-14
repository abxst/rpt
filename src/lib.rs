//! RPT engine building blocks. This is an experimental transport MVP, not an APT replacement.
pub mod download;
pub mod drivers;
pub mod cache_query;
pub mod metadata_query;
pub mod install_plan;
pub mod transaction_plan_audit;
pub mod removal_impact;
pub mod removal_planner;
pub mod policy;
pub mod apt_preferences;
pub mod apt_pin_eval;
pub mod apt_phasing;
pub mod update;
pub mod package_provenance;
pub mod artifact_store;
pub mod sandbox_execution;
pub mod sandbox_session;
pub mod native_rootfs;
pub mod native_snapshot;
pub mod package_index;
pub mod index_catalog;
pub mod version;
pub mod error;
pub mod sources;
pub mod transaction;
pub mod transaction_journal;
pub mod recovery_inspector;
pub mod recovery_audit;
pub mod process_supervisor;
pub mod dry_run_executor;
pub mod overlay_stage;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::path::{Component, Path};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Package {
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub size: u64,
    /// Relative path below the selected destination directory.
    pub filename: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub packages: Vec<Package>,
}

pub fn validate_package(p: &Package) -> Result<()> {
    if p.name.is_empty() || (p.name.contains('\n') || p.name.contains('\r')) {
        bail!("invalid package name");
    }
    let url = reqwest::Url::parse(&p.url)?;
    if !matches!(url.scheme(), "https" | "http") {
        bail!("only HTTP(S) URLs are supported");
    }
    if p.sha256.len() != 64 || hex::decode(&p.sha256).is_err() {
        bail!("invalid SHA-256 for {}", p.name);
    }
    let path = Path::new(&p.filename);
    if path.as_os_str().is_empty()
        || !path.components().all(|c| matches!(c, Component::Normal(_)))
    {
        bail!("unsafe filename: {}", p.filename);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn example() -> Package {
        Package {
            name: "sample".into(),
            url: "https://example.com/a.deb".into(),
            sha256: "a".repeat(64),
            size: 128,
            filename: "pool/a.deb".into(),
        }
    }
    #[test]
    fn accepts_valid_package() {
        validate_package(&example()).unwrap();
    }
    #[test]
    fn rejects_path_traversal() {
        let mut p = example();
        p.filename = "../escape.deb".into();
        assert!(validate_package(&p).is_err());
    }
    #[test]
    fn rejects_invalid_digest() {
        let mut p = example();
        p.sha256 = "xyz".into();
        assert!(validate_package(&p).is_err());
    }
}

//! Distro-independent package management contracts.
//! A driver encapsulates native versioning, installed DB access, repository
//! metadata and eventually transaction execution. Unsupported backends fail closed.
pub mod deb;
pub mod rpm;
pub mod apk;
pub mod pacman;

use anyhow::{bail, Context, Result};
use futures_util::future::BoxFuture;
use std::{cmp::Ordering, collections::BTreeMap, fs, path::Path, str::FromStr};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DriverKind { Deb, Rpm, Apk, Pacman }

impl DriverKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Deb => "deb",
            Self::Rpm => "rpm",
            Self::Apk => "apk",
            Self::Pacman => "pacman",
        }
    }
}
impl FromStr for DriverKind {
    type Err = anyhow::Error;
    fn from_str(input: &str) -> Result<Self> {
        match input {
            "deb" => Ok(Self::Deb),
            "rpm" => Ok(Self::Rpm),
            "apk" => Ok(Self::Apk),
            "pacman" => Ok(Self::Pacman),
            _ => bail!("unknown package driver: {input} (expected deb, rpm, apk, pacman)"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub state: String,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operation {
    Update, Search, Install, Reinstall, Remove, Purge, Upgrade,
    FullUpgrade, Autoremove, Clean,
}

/// Defined at the native-package layer. Driver implementations must never
/// silently fall back to another distro's package manager or compare algorithm.
pub trait PackageDriver: Send + Sync {
    fn kind(&self) -> DriverKind;
    fn artifact_extension(&self) -> &'static str;
    fn installed_database(&self) -> &'static str;

    fn compare_versions(&self, _left: &str, _right: &str) -> Result<Ordering> {
        bail!("{} version ordering is not implemented", self.kind().as_str())
    }
    fn installed_packages(&self, _status: &Path, _architecture: &str) -> Result<BTreeMap<String, InstalledPackage>> {
        bail!("{} installed state backend is not implemented", self.kind().as_str())
    }
    /// Read-only install planning is available only when a backend can
    /// interpret its own source metadata and installed package database.
    fn plan_install_preview(
        &self, _cache: &Path, _status: &Path, _configuration: &Path,
        _name: &str, _architecture: &str, _max_packages: usize,
    ) -> Result<Vec<crate::install_plan::PlanStep>> {
        bail!("{} driver has no dependency planner", self.kind().as_str())
    }
    /// Repository-native package acquisition, still only planning/download.
    fn prepare_download(
        &self, _cache: &Path, _names: &[String], _architecture: &str,
    ) -> Result<crate::Manifest> {
        bail!("{} driver has no signed package acquisition", self.kind().as_str())
    }
    fn update_indexes<'a>(
        &'a self, _root: &'a Path, _output: &'a Path,
        _options: crate::update::UpdateOptions,
    ) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            bail!("{} repository update is not implemented", self.kind().as_str())
        })
    }
    fn cache_versions(&self, _cache: &Path, _package: &str, _arch: &str) -> Result<Vec<String>> {
        bail!("{} package cache reader not implemented", self.kind().as_str())
    }
    fn policy_preview(
        &self, _cache: &Path, _status: &Path, _config: &Path,
        _package: &str, _arch: &str,
    ) -> Result<crate::policy::PolicyPreview> {
        bail!("{} candidate policy not implemented", self.kind().as_str())
    }
    fn show_package(
        &self, _cache: &Path, _arch: &str, _package: &str,
    ) -> Result<Option<crate::metadata_query::PackageSummary>> {
        bail!("{} package metadata reader not implemented", self.kind().as_str())
    }
    fn search_packages(
        &self, _cache: &Path, _arch: &str, _query: &str, _limit: usize,
    ) -> Result<Vec<crate::metadata_query::PackageSummary>> {
        bail!("{} package metadata search not implemented", self.kind().as_str())
    }
    fn list_packages(
        &self, _cache: &Path, _arch: &str, _prefix: &str, _limit: usize,
    ) -> Result<Vec<crate::metadata_query::PackageSummary>> {
        bail!("{} package metadata listing not implemented", self.kind().as_str())
    }
    fn reverse_dependencies(
        &self, _cache: &Path, _arch: &str, _package: &str, _limit: usize,
    ) -> Result<Vec<crate::metadata_query::PackageSummary>> {
        bail!("{} reverse dependency lookup not implemented", self.kind().as_str())
    }
    fn operation_supported(&self, _operation: Operation) -> bool { false }
    fn require_operation(&self, operation: Operation) -> Result<()> {
        if !self.operation_supported(operation) {
            bail!("{} driver does not implement {operation:?}; refusing cross-distro fallback", self.kind().as_str());
        }
        Ok(())
    }
}

pub fn driver(kind: DriverKind) -> &'static dyn PackageDriver {
    match kind {
        DriverKind::Deb => &deb::DebDriver,
        DriverKind::Rpm => &rpm::RpmDriver,
        DriverKind::Apk => &apk::ApkDriver,
        DriverKind::Pacman => &pacman::PacmanDriver,
    }
}

fn os_release_field(contents: &str, needle: &str) -> Option<String> {
    contents.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != needle { return None }
        let v = value.trim().trim_matches('"').trim_matches('\'');
        Some(v.to_ascii_lowercase())
    })
}

/// Discovery only. It never enables a backend or mutates host package state.
/// Callers must distinguish a recognized distro from an implemented backend.
pub fn detect_from_os_release(contents: &str) -> Result<DriverKind> {
    let id = os_release_field(contents, "ID").context("os-release missing ID")?;
    let like = os_release_field(contents, "ID_LIKE").unwrap_or_default();
    let tokens = std::iter::once(id.as_str()).chain(like.split_whitespace());
    for token in tokens {
        match token {
            "debian" | "ubuntu" | "linuxmint" | "pop" => return Ok(DriverKind::Deb),
            "fedora" | "rhel" | "centos" | "rocky" | "almalinux" | "ol" | "opensuse" | "suse" => return Ok(DriverKind::Rpm),
            "alpine" => return Ok(DriverKind::Apk),
            "arch" | "archlinux" | "manjaro" | "endeavouros" => return Ok(DriverKind::Pacman),
            _ => {}
        }
    }
    bail!("unknown distro in os-release; explicit package driver required")
}
pub fn detect_file(path: &Path) -> Result<DriverKind> {
    let contents = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    detect_from_os_release(&contents)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn discovers_families_without_enabling_them() {
        for (contents, expected) in [
            ("ID=ubuntu\nID_LIKE=debian\n", DriverKind::Deb),
            ("ID=rocky\nID_LIKE=\"rhel centos fedora\"\n", DriverKind::Rpm),
            ("ID=alpine\n", DriverKind::Apk),
            ("ID=manjaro\nID_LIKE=arch\n", DriverKind::Pacman),
        ] {
            assert_eq!(detect_from_os_release(contents).unwrap(), expected);
        }
        assert!(detect_from_os_release("ID=unknown\n").is_err());
    }
    #[test]
    fn unimplemented_drivers_fail_closed() {
        for kind in [DriverKind::Rpm, DriverKind::Apk, DriverKind::Pacman] {
            let d = driver(kind);
            assert!(d.compare_versions("1.0", "2.0").is_err());
            assert!(d.installed_packages(Path::new("/nonexistent"), "x86_64").is_err());
            assert!(d.require_operation(Operation::Install).is_err());
        }
        assert!(!driver(DriverKind::Deb).operation_supported(Operation::Install));
    }
}

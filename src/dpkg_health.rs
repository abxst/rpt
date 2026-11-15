//! Read-only dpkg status health audit. This is NOT a repair tool or a
//! substitute for dpkg --audit. It never acquires dpkg locks or runs scripts.
use crate::{package_index, version};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    os::unix::fs::MetadataExt,
    path::Path,
};

const MAX_STATUS_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Serialize)]
pub struct DpkgIssue {
    pub package: String,
    pub architecture: String,
    pub version: Option<String>,
    pub selection: String,
    pub error: String,
    pub state: String,
    pub kind: &'static str,
}

#[derive(Debug, Serialize)]
pub struct DpkgHealth {
    pub schema_version: u32,
    pub status_sha256: String,
    pub total_records: usize,
    pub fully_installed: usize,
    pub residual_config: usize,
    pub not_installed: usize,
    pub issues: Vec<DpkgIssue>,
    pub healthy: bool,
    pub read_only: bool,
    pub apt_parity_verified: bool,
    pub host_execution_allowed: bool,
    pub warning: &'static str,
}

fn valid_name(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()
            || matches!(b, b'+' | b'.' | b'-'))
        && value.as_bytes()[0].is_ascii_alphanumeric()
}

fn valid_architecture(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

pub fn inspect_snapshot(bytes: &[u8]) -> Result<DpkgHealth> {
    if bytes.len() as u64 > MAX_STATUS_BYTES {
        bail!("dpkg status exceeds 64 MiB");
    }
    let text = std::str::from_utf8(bytes).context("dpkg status must be UTF-8")?;
    let records = package_index::parse(text).context("malformed dpkg status")?;
    let mut identities = BTreeSet::new();
    let mut fully_installed = 0;
    let mut residual_config = 0;
    let mut not_installed = 0;
    let mut issues = Vec::new();
    for record in &records {
        let name = record.field("Package").context("dpkg record missing Package")?;
        let arch = record.field("Architecture").context("dpkg record missing Architecture")?;
        if !valid_name(name) || !valid_architecture(arch) {
            bail!("invalid dpkg package identity: {name}:{arch}");
        }
        if !identities.insert((name.to_owned(), arch.to_owned())) {
            bail!("duplicate dpkg package identity: {name}:{arch}");
        }
        let raw = record.field("Status").context("dpkg record missing Status")?;
        let parts = raw.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 3 {
            bail!("malformed dpkg Status for {name}:{arch}");
        }
        let (selection, error, state) = (parts[0], parts[1], parts[2]);
        if !matches!(selection, "unknown" | "install" | "hold" | "deinstall" | "purge")
            || !matches!(error, "ok" | "reinstreq")
            || !matches!(state, "not-installed" | "config-files" | "half-installed"
                | "unpacked" | "half-configured" | "triggers-awaited"
                | "triggers-pending" | "installed")
        {
            bail!("unknown dpkg Status for {name}:{arch}: {raw}");
        }
        let version = record.field("Version");
        if let Some(value) = version {
            if value.is_empty() {
                bail!("empty dpkg version for {name}:{arch}");
            }
            version::compare(value, value)?;
        } else if state == "installed" {
            bail!("installed binary missing Version: {name}:{arch}");
        }
        let kind = if error == "reinstreq" {
            Some("reinstreq")
        } else {
            match state {
                "installed" if matches!(selection, "install" | "hold") => {
                    fully_installed += 1;
                    None
                }
                "installed" => Some("selection_mismatch"),
                "config-files" if matches!(selection, "deinstall" | "purge" | "unknown") => {
                    residual_config += 1;
                    None
                }
                "config-files" => Some("pending_selection"),
                "not-installed" if matches!(selection, "install" | "hold") => {
                    Some("pending_selection")
                }
                "not-installed" => {
                    not_installed += 1;
                    None
                }
                _ => Some("incomplete_transaction"),
            }
        };
        if let Some(kind) = kind {
            issues.push(DpkgIssue {
                package: name.to_owned(),
                architecture: arch.to_owned(),
                version: version.map(str::to_owned),
                selection: selection.to_owned(),
                error: error.to_owned(),
                state: state.to_owned(),
                kind,
            });
        }
    }
    Ok(DpkgHealth {
        schema_version: 1,
        status_sha256: hex::encode(Sha256::digest(bytes)),
        total_records: records.len(),
        fully_installed,
        residual_config,
        not_installed,
        healthy: issues.is_empty(),
        issues,
        read_only: true,
        apt_parity_verified: false,
        host_execution_allowed: false,
        warning: "Diagnostic only; inspect dpkg state and recover in an isolated VM. No repair, host mutation, or full dpkg --audit parity is implemented.",
    })
}

/// Read an ordinary dpkg status file with a bounded size and reject a final
/// path symlink. Bind the opened fd to the pre-open inode to prevent replacing
/// an ordinary file with a symlink between preflight and open.
pub fn inspect(path: &Path) -> Result<DpkgHealth> {
    let before = fs::symlink_metadata(path)
        .with_context(|| format!("reading dpkg status {}", path.display()))?;
    if !before.file_type().is_file() || before.len() > MAX_STATUS_BYTES {
        bail!("dpkg status must be an ordinary, non-symlink file <=64 MiB");
    }
    let file = File::open(path)?;
    let opened = file.metadata()?;
    if !opened.is_file() || before.dev() != opened.dev()
        || before.ino() != opened.ino() || opened.len() > MAX_STATUS_BYTES
    {
        bail!("dpkg status changed or was redirected during opening");
    }
    let mut bytes = Vec::new();
    file.take(MAX_STATUS_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STATUS_BYTES {
        bail!("dpkg status grew beyond 64 MiB");
    }
    inspect_snapshot(&bytes)
}

/// Fail closed before preparing signed whole-system upgrade plans. No dpkg
/// mutation is implied; a clean status file alone is not APT parity.
pub fn require_clean_snapshot(bytes: &[u8]) -> Result<()> {
    let report = inspect_snapshot(bytes)?;
    if !report.healthy {
        bail!("dpkg health audit reports {} incomplete or inconsistent package(s); recover the guest first",
            report.issues.len());
    }
    Ok(())
}

pub fn require_clean_file(path: &Path) -> Result<()> {
    let report = inspect(path)?;
    if !report.healthy {
        bail!("dpkg health audit reports {} incomplete or inconsistent package(s); recover the guest first",
            report.issues.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn clean_installed_and_residual_configs_are_not_guessed_as_broken() {
        let bytes = b"Package: base\nVersion: 1\nArchitecture: amd64\nStatus: hold ok installed\n\nPackage: old\nArchitecture: amd64\nVersion: 0.9\nStatus: deinstall ok config-files\n\n";
        let result = inspect_snapshot(bytes).unwrap();
        assert!(result.healthy);
        assert_eq!(result.fully_installed, 1);
        assert_eq!(result.residual_config, 1);
        assert!(result.issues.is_empty());
        assert_eq!(result.status_sha256.len(), 64);
        assert!(!result.host_execution_allowed);
    }
    #[test]
    fn finds_partial_configure_triggers_and_reinstall_required() {
        let bytes = b"Package: a\nVersion: 1\nArchitecture: amd64\nStatus: install ok unpacked\n\nPackage: b\nVersion: 2\nArchitecture: amd64\nStatus: install reinstreq half-configured\n\nPackage: c\nVersion: 3\nArchitecture: amd64\nStatus: hold ok triggers-pending\n\n";
        let result = inspect_snapshot(bytes).unwrap();
        assert!(!result.healthy);
        assert_eq!(result.issues.len(), 3);
        assert_eq!(result.issues[0].kind, "incomplete_transaction");
        assert_eq!(result.issues[1].kind, "reinstreq");
        assert_eq!(result.issues[2].state, "triggers-pending");
    }
    #[test]
    fn rejects_duplicate_or_unknown_status_without_guessing() {
        let record = "Package: a\nVersion: 1\nArchitecture: amd64\nStatus: install ok installed\n\n";
        assert!(inspect_snapshot(format!("{record}{record}").as_bytes()).is_err());
        assert!(inspect_snapshot(b"Package: a\nVersion: 1\nArchitecture: amd64\nStatus: install ok mystery\n\n").is_err());
        assert!(inspect_snapshot(b"Package: a\nVersion: 1\nStatus: install ok installed\n\n").is_err());
    }
    #[test]
    fn rejects_symlink_without_mutating_target() {
        let temp = tempfile::tempdir().unwrap();
        let real = temp.path().join("status");
        let link = temp.path().join("alias");
        fs::write(&real, b"Package: a\nVersion: 1\nArchitecture: all\nStatus: install ok installed\n\n").unwrap();
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(inspect(&link).is_err());
        assert!(inspect(&real).unwrap().healthy);
    }
}

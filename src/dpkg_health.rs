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
    /// Numbered fragments in dpkg's pending status updates journal.
    pub pending_status_updates: usize,
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
        pending_status_updates: 0,
        healthy: issues.is_empty(),
        issues,
        read_only: true,
        apt_parity_verified: false,
        host_execution_allowed: false,
        warning: "Diagnostic only; inspect dpkg state and recover in an isolated VM. No repair, host mutation, or full dpkg --audit parity is implemented.",
    })
}

/// The raw status file can be stale while dpkg-query overlays numbered
/// /var/lib/dpkg/updates entries. Do not replay these fragments: fail closed
/// until dpkg has reconciled the journal into its main database.
fn pending_status_update_files(path: &Path) -> Result<Vec<String>> {
    let parent = path.parent().context("dpkg status has no parent directory")?;
    let updates = parent.join("updates");
    let meta = match fs::symlink_metadata(&updates) {
        Ok(meta) => meta,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("checking dpkg status updates journal"),
    };
    if !meta.file_type().is_dir() {
        bail!("dpkg health updates journal must be a real non-symlink directory");
    }
    let mut result = Vec::new();
    let mut total = 0usize;
    for entry in fs::read_dir(&updates)? {
        let entry = entry?;
        total += 1;
        if total > 2048 { bail!("dpkg updates journal inventory exceeds 2048 entries"); }
        let name = entry.file_name();
        let name = name.to_str().context("non-UTF-8 dpkg updates journal filename")?;
        if !name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()) {
            result.push(name.to_owned());
            if result.len() > 128 { bail!("too many pending dpkg status updates"); }
        }
    }
    result.sort();
    Ok(result)
}

/// Snapshot-only callers must supply the actual status pathname separately,
/// since deferred dpkg updates are *not* present in the original file bytes.
pub fn require_no_pending_updates(path: &Path) -> Result<()> {
    let pending = pending_status_update_files(path)?;
    if !pending.is_empty() {
        bail!("dpkg health audit found {} pending numbered status update(s); recover dpkg before planning", pending.len());
    }
    Ok(())
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
    let mut report = inspect_snapshot(&bytes)?;
    let pending = pending_status_update_files(path)?;
    if !pending.is_empty() {
        report.pending_status_updates = pending.len();
        report.healthy = false;
        report.issues.push(DpkgIssue {
            package: "dpkg-status-journal".into(), architecture: "all".into(),
            version: None, selection: "unknown".into(), error: "ok".into(),
            state: "pending-updates".into(), kind: "pending_status_updates",
        });
    }
    Ok(report)
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

/// Hash a healthy, bounded, regular dpkg status snapshot without changing it.
pub fn clean_status_digest(path: &Path) -> Result<String> {
    let report = inspect(path)?;
    if !report.healthy {
        bail!("dpkg health audit reports {} incomplete or inconsistent package(s); recover the guest first",
            report.issues.len());
    }
    Ok(report.status_sha256)
}

/// Final TOCTOU preflight for read-only policy planners that might otherwise
/// return a candidate derived from a different dpkg inventory after a
/// concurrent writer changes /var/lib/dpkg/status during resolution.
pub fn verify_unchanged_clean_file(path: &Path, expected_digest: &str) -> Result<()> {
    let actual = clean_status_digest(path)?;
    if actual != expected_digest {
        bail!("dpkg status changed while building signed package plan; discard stale candidate");
    }
    Ok(())
}

pub fn require_clean_file(path: &Path) -> Result<()> {
    clean_status_digest(path).map(|_| ())
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
    fn rejects_concurrently_changed_status_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("status");
        let clean_v1 = b"Package: testpkg\nVersion: 1\nArchitecture: amd64\nStatus: install ok installed\n\n";
        let clean_v2 = b"Package: testpkg\nVersion: 2\nArchitecture: amd64\nStatus: install ok installed\n\n";
        fs::write(&path, clean_v1).unwrap();
        let original_digest = clean_status_digest(&path).unwrap();
        verify_unchanged_clean_file(&path, &original_digest).unwrap();
        fs::write(&path, clean_v2).unwrap();
        assert!(verify_unchanged_clean_file(&path, &original_digest).is_err());
        fs::write(&path, b"Package: testpkg\nVersion: 2\nArchitecture: amd64\nStatus: install ok half-configured\n\n").unwrap();
        assert!(verify_unchanged_clean_file(&path, &original_digest).is_err());
    }

    #[test]
    fn numbered_updates_make_clean_raw_status_untrustworthy() {
        let temp = tempfile::tempdir().unwrap();
        let status = temp.path().join("status");
        fs::write(&status, b"Package: a\nVersion: 1\nArchitecture: amd64\nStatus: install ok installed\n\n").unwrap();
        assert!(inspect(&status).unwrap().healthy);
        fs::create_dir(temp.path().join("updates")).unwrap();
        fs::write(temp.path().join("updates/0000"),
                  b"Package: a\nVersion: 2\nArchitecture: amd64\nStatus: install ok half-configured\n\n").unwrap();
        let report = inspect(&status).unwrap();
        assert!(!report.healthy);
        assert_eq!(report.pending_status_updates, 1);
        assert_eq!(report.issues.last().unwrap().kind, "pending_status_updates");
        assert!(require_clean_file(&status).is_err());
        assert!(require_no_pending_updates(&status).is_err());
        fs::remove_file(temp.path().join("updates/0000")).unwrap();
        assert!(inspect(&status).unwrap().healthy);
        fs::write(temp.path().join("updates/tmp.i"), b"staging").unwrap();
        assert!(require_no_pending_updates(&status).is_ok());
    }

    #[test]
    fn symlinked_dpkg_updates_directory_is_refused() {
        let temp = tempfile::tempdir().unwrap();
        let status = temp.path().join("status");
        fs::write(&status, b"").unwrap();
        fs::create_dir(temp.path().join("other")).unwrap();
        std::os::unix::fs::symlink(temp.path().join("other"), temp.path().join("updates")).unwrap();
        assert!(inspect(&status).is_err());
        assert!(require_no_pending_updates(&status).is_err());
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

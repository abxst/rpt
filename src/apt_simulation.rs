//! Unified *read-only* APT-like operation simulations. Never opens dpkg
//! locks, calls dpkg, writes package state or authorizes host execution.
//!
//! A complete real APT resolver is not implemented. These operations expose
//! the existing strict, bounded signed candidate/removal planners with a
//! uniform machine-readable result and explicit unsupported-policy gates.
use crate::{
    removal_planner, system_auto_upgrade::{self, AutoUpgradeSelection},
    system_upgrade, transaction_plan_audit::Change,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, io::ErrorKind, path::Path};

pub enum Operation<'a> {
    Remove { packages: &'a [String], cascade: bool },
    Purge { packages: &'a [String], cascade: bool },
    Autoremove { extended_states: &'a Path },
    Upgrade { cache: &'a Path, apt_root: &'a Path,
        max_roots: usize, max_packages: usize },
    FullUpgrade { cache: &'a Path, apt_root: &'a Path,
        max_roots: usize, max_packages: usize, max_removals: usize },
}

/// The only positive indication is "viable inside the supported subset".
/// It is NEVER an execution authorization, nor APT policy/CLI parity.
#[derive(Debug, Serialize)]
pub struct AptSimulation {
    pub schema_version: u32,
    pub operation: &'static str,
    pub outcome: &'static str,
    pub viable_in_supported_subset: bool,
    pub simulation_only: bool,
    pub apt_parity_verified: bool,
    pub host_execution_allowed: bool,
    pub dpkg_status_sha256: String,
    pub apt_marks_sha256: Option<String>,
    pub selected_changes: Vec<Change>,
    pub selected_removals: Vec<String>,
    pub blockers: Vec<String>,
    pub detailed_audit: Value,
    pub warning: &'static str,
}

fn checked_regular(path: &Path, may_be_missing: bool) -> Result<Option<Vec<u8>>> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound && may_be_missing => return Ok(None),
        Err(error) => return Err(error).with_context(|| format!("reading package state {}", path.display())),
    };
    if !metadata.file_type().is_file() || metadata.len() > 64 * 1024 * 1024 {
        bail!("APT/dpkg state must be a real regular file <=64 MiB: {}", path.display());
    }
    let bytes = fs::read(path)?;
    if bytes.len() > 64 * 1024 * 1024 {
        bail!("APT/dpkg state grew past 64 MiB");
    }
    Ok(Some(bytes))
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub fn simulate(status: &Path, native: &str, operation: Operation<'_>) -> Result<AptSimulation> {
    if native.is_empty() || native == "all" || !native.bytes().all(|c|
        c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'-') {
        bail!("unsupported native simulation architecture");
    }
    let before = checked_regular(status, false)?
        .context("dpkg status does not exist")?;
    let status_sha256 = digest(&before);
    // A full-system upgrade must not silently ignore partly configured dpkg
    // records when building a seemingly clean installed package inventory.
    if matches!(&operation, Operation::Upgrade { .. } | Operation::FullUpgrade { .. }) {
        crate::dpkg_health::require_clean_snapshot(&before)?;
    }
    let mut marks_snapshot: Option<(&Path, Option<Vec<u8>>)> = None;
    let (kind, selected_changes, selected_removals, blockers, audit) = match operation {
        Operation::Remove { packages, cascade } => {
            let plan = removal_planner::remove_snapshot(
                &before, native, packages, cascade,
            )?;
            let removed = plan.remove.clone();
            let blockers = plan.blockers.clone();
            ("remove", Vec::new(), removed, blockers, serde_json::to_value(plan)?)
        }
        Operation::Purge { packages, cascade } => {
            let plan = removal_planner::purge_snapshot(
                &before, native, packages, cascade,
            )?;
            let removed = plan.remove.clone();
            let blockers = plan.blockers.clone();
            ("purge", Vec::new(), removed, blockers, serde_json::to_value(plan)?)
        }
        Operation::Autoremove { extended_states } => {
            // Absence means no automatically installed marks, as in apt-mark:
            // do not remove unmarked packages by guessing.
            let marks = checked_regular(extended_states, true)?;
            let plan = removal_planner::autoremove_snapshot(
                &before, marks.as_deref().unwrap_or_default(), native,
            )?;
            marks_snapshot = Some((extended_states, marks));
            let removed = plan.remove.clone();
            let blockers = plan.blockers.clone();
            ("autoremove", Vec::new(), removed, blockers, serde_json::to_value(plan)?)
        }
        Operation::Upgrade { cache, apt_root, max_roots, max_packages } => {
            let plan = system_upgrade::preview(
                cache, status, apt_root, native, max_roots, max_packages,
            )?;
            let changes = plan.transaction.changes.clone();
            ("upgrade", changes, Vec::new(), Vec::new(), serde_json::to_value(plan)?)
        }
        Operation::FullUpgrade { cache, apt_root, max_roots, max_packages, max_removals } => {
            let plan = system_auto_upgrade::preview(
                cache, status, apt_root, native, max_roots, max_packages, max_removals,
            )?;
            let changes = match &plan.selection {
                AutoUpgradeSelection::NoRemoval { audit } => audit.transaction.changes.clone(),
                AutoUpgradeSelection::RemovalScenario { scenario } =>
                    scenario.upgrades.transaction.changes.clone(),
            };
            let removed = plan.selected_removals.clone();
            ("full-upgrade", changes, removed, Vec::new(), serde_json::to_value(plan)?)
        }
    };
    let after = checked_regular(status, false)?
        .context("dpkg status vanished while simulating")?;
    if digest(&after) != status_sha256 {
        bail!("dpkg status changed during simulation; discard plan");
    }
    let marks_sha256 = if let Some((path, before)) = marks_snapshot {
        let after = checked_regular(path, true)?;
        if after != before {
            bail!("APT automatic/manual marks changed during simulation; discard plan");
        }
        before.as_deref().map(digest)
    } else {
        None
    };
    let outcome = if !blockers.is_empty() {
        "blocked"
    } else if selected_changes.is_empty() && selected_removals.is_empty() {
        "no_changes"
    } else {
        "planned"
    };
    Ok(AptSimulation {
        schema_version: 1, operation: kind, outcome,
        viable_in_supported_subset: blockers.is_empty(),
        simulation_only: true, apt_parity_verified: false, host_execution_allowed: false,
        dpkg_status_sha256: status_sha256, apt_marks_sha256: marks_sha256,
        selected_changes, selected_removals, blockers, detailed_audit: audit,
        warning: "Restricted read-only Debian planner; result is NOT full APT parity and NEVER authorizes host package execution.",
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const INSTALLED: &[u8] = b"Package: client\nVersion: 1\nArchitecture: amd64\nStatus: install ok installed\nDepends: librpt (>= 1)\n\nPackage: librpt\nVersion: 1\nArchitecture: amd64\nStatus: install ok installed\n\n";
    fn fixture(status: &[u8]) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let file = dir.path().join("status");
        fs::write(&file, status).unwrap();
        (dir, file)
    }

    #[test]
    fn removing_required_dependency_reports_blockers_without_changes() {
        let (_dir, status) = fixture(INSTALLED);
        let first = fs::read(&status).unwrap();
        let blocked = simulate(&status, "amd64", Operation::Remove {
            packages: &["librpt".into()], cascade: false,
        }).unwrap();
        assert_eq!(blocked.operation, "remove");
        assert_eq!(blocked.outcome, "blocked");
        assert!(!blocked.viable_in_supported_subset);
        assert!(!blocked.blockers.is_empty());
        assert!(!blocked.host_execution_allowed);
        assert_eq!(fs::read(&status).unwrap(), first);
    }

    #[test]
    fn cascade_removal_is_explicit_and_keeps_real_status_unchanged() {
        let (_dir, status) = fixture(INSTALLED);
        let report = simulate(&status, "amd64", Operation::Remove {
            packages: &["librpt".into()], cascade: true,
        }).unwrap();
        assert_eq!(report.outcome, "planned");
        assert_eq!(report.selected_removals, ["client:amd64", "librpt:amd64"]);
        assert!(report.selected_changes.is_empty());
        assert!(!report.host_execution_allowed);
    }

    #[test]
    fn purge_supports_config_files_only_without_installing_or_mutating() {
        let (_dir, status) = fixture(
            b"Package: orphan\nArchitecture: amd64\nVersion: 1\nStatus: deinstall ok config-files\n\n",
        );
        let report = simulate(&status, "amd64", Operation::Purge {
            packages: &["orphan".into()], cascade: false,
        }).unwrap();
        assert_eq!(report.outcome, "planned");
        assert_eq!(report.selected_removals, ["orphan:amd64"]);
        assert_eq!(report.detailed_audit["mode"], "purge");
    }

    #[test]
    fn autoremove_works_with_missing_marks_without_guessing() {
        let (dir, status) = fixture(INSTALLED);
        let marks = dir.path().join("does-not-exist");
        let report = simulate(&status, "amd64", Operation::Autoremove {
            extended_states: &marks,
        }).unwrap();
        assert_eq!(report.outcome, "no_changes");
        assert_eq!(report.apt_marks_sha256, None);
        assert!(report.selected_removals.is_empty());
    }

    #[test]
    fn autoremove_preserves_manual_roots_but_removes_orphaned_auto_package() {
        let (dir, status) = fixture(b"Package: client\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\nPackage: orphan\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n");
        let marks = dir.path().join("extended_states");
        fs::write(&marks, b"Package: orphan\nArchitecture: amd64\nAuto-Installed: 1\n\n").unwrap();
        let report = simulate(&status, "amd64", Operation::Autoremove {
            extended_states: &marks,
        }).unwrap();
        assert_eq!(report.outcome, "planned");
        assert_eq!(report.selected_removals, ["orphan:amd64"]);
        assert_eq!(report.apt_marks_sha256, Some(digest(&fs::read(&marks).unwrap())));
    }

    #[test]
    fn rejects_symlinked_state_and_invalid_architecture_before_mutation() {
        let (dir, status) = fixture(INSTALLED);
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&status, &link).unwrap();
        let request = &["librpt".into()];
        assert!(simulate(&link, "amd64", Operation::Remove {
            packages: request, cascade: false,
        }).is_err());
        assert!(simulate(&status, "../amd64", Operation::Remove {
            packages: request, cascade: false,
        }).is_err());
        assert_eq!(fs::read(status).unwrap(), INSTALLED);
    }
}

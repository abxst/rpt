//! Read-only joint removal + signed system-upgrade *scenario* planning.
//! NOT apt-get dist-upgrade: no automatic removal ranking or dpkg execution.
use crate::{
    cache_query::{self, InstalledKey}, install_plan::PlanStep, metadata_query,
    package_provenance, removal_planner, system_upgrade,
};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fs, io::Write, path::Path};

#[derive(Debug, Serialize)]
pub struct RemovedBinary {
    pub name: String,
    pub architecture: String,
    pub installed_version: String,
}

#[derive(Debug, Serialize)]
pub struct JointUpgradeScenario {
    pub schema_version: u32,
    pub original_status_sha256: String,
    pub projected_status_sha256: String,
    pub removals: Vec<RemovedBinary>,
    pub upgrades: system_upgrade::SystemUpgradeAudit,
    pub final_state: removal_planner::UpgradeTransitionAudit,
    pub executable: bool,
    pub warning: String,
}

/// Validate hypothetical explicit removals against the *original* installed
/// dpkg state, then run the existing globally checked, authenticated joint
/// upgrade planner against the projected dpkg state. Finally, validate the
/// combined final state again against the ORIGINAL installed status.
///
/// The projection is stored only in a private temporary file. Nothing is
/// changed in the user's source status, APT metadata cache or dpkg database.
pub fn preview(
    cache: &Path, status: &Path, apt_root: &Path, architecture: &str,
    removals: &[String], max_roots: usize, max_packages: usize,
) -> Result<JointUpgradeScenario> {
    let original = fs::read(status).context("reading original dpkg status")?;
    if original.len() > 64 * 1024 * 1024 {
        bail!("dpkg status exceeds 64 MiB");
    }
    let original_sha256 = hex::encode(Sha256::digest(&original));
    let original_installed = cache_query::installed_catalog_from_bytes(&original)?;
    let (projected, removed_keys) =
        removal_planner::project_status_without(&original, architecture, removals)?;
    let projected_sha256 = hex::encode(Sha256::digest(&projected));
    let mut temp = tempfile::NamedTempFile::new()
        .context("creating private read-only upgrade projection")?;
    temp.write_all(&projected)?;
    temp.flush()?;

    // This separately verifies each signed upgrade candidate, joint solver,
    // and every survivor dependency. No self-declared version is trusted.
    let upgrades = system_upgrade::preview(
        cache, temp.path(), apt_root, architecture, max_roots, max_packages,
    ).context("resolving signed policy upgrades after hypothetical removals")?;
    let mut removed = Vec::with_capacity(removed_keys.len());
    for key in &removed_keys {
        let installed = original_installed.get(key)
            .context("removal no longer matches the original installed status")?;
        removed.push(RemovedBinary {
            name: key.name.clone(), architecture: key.architecture.clone(),
            installed_version: installed.version.clone(),
        });
    }
    let mut steps = Vec::new();
    let mut trusted = BTreeMap::new();
    for change in &upgrades.transaction.changes {
        let key = InstalledKey {
            name: change.package.clone(), architecture: change.architecture.clone(),
        };
        let origins = package_provenance::verified_version_origins(
            cache, &key.name, architecture,
        )?;
        let verified = metadata_query::verified_exact_version(
            cache, architecture, &key.name, &change.to, &origins,
        )?;
        if verified.name != key.name || verified.architecture != key.architecture {
            bail!("staged candidate identity drift after removal scenario");
        }
        if trusted.insert(key, verified).is_some() {
            bail!("duplicated signed staged binary in scenario");
        }
        steps.push(PlanStep {
            package: change.package.clone(), architecture: change.architecture.clone(),
            version: change.to.clone(), installed: change.from.clone(),
        });
    }
    let final_state = removal_planner::audit_transition_with_removals(
        &original, architecture, &steps, &trusted, &removed_keys,
    ).context("combined removals and signed upgrades violate original installed-state constraints")?;
    if final_state.removed_binaries != removed_keys.len()
        || final_state.staged_binaries != steps.len() {
        bail!("joint upgrade scenario count mismatch");
    }
    // This is a read-only status snapshot; if a concurrent change was made
    // to the source status while planning, the resulting report is stale.
    if hex::encode(Sha256::digest(fs::read(status)?)) != original_sha256 {
        bail!("original dpkg status changed during scenario planning");
    }
    Ok(JointUpgradeScenario {
        schema_version: 1,
        original_status_sha256: original_sha256,
        projected_status_sha256: projected_sha256,
        removals: removed,
        upgrades,
        final_state,
        executable: false,
        warning: "Read-only hypothetical explicit-removals plus signed upgrade plan. NOT apt-get dist-upgrade parity: no automatic removals, no host dpkg operations, no atomic removal/upgrade execution, no authorized transaction intent. Scenario is limited to removals that leave the interim installed state consistent.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn impossible_or_empty_removal_set_fails_before_repo_use() {
        assert!(preview(Path::new("/none"), Path::new("/none"),
            Path::new("/none"), "amd64", &["invalid-".into()], 128, 200).is_err());
    }
}

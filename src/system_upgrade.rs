//! Read-only, whole-installed-set upgrade discovery with a bounded joint
//! policy resolver. This is not apt-get upgrade/full-upgrade and cannot run dpkg.
use crate::{apt_pin_eval, apt_preferences, cache_query, install_plan, metadata_query,
    package_provenance, policy, transaction_plan_audit, version};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{cmp::Ordering, collections::BTreeSet, path::Path};

#[derive(Debug, Serialize)]
pub struct SystemUpgradeAudit {
    pub scanned_installed: usize,
    pub unchanged_or_policy_kept: usize,
    pub installed_only: Vec<String>,
    pub held: Vec<String>,
    pub requested_upgrades: Vec<String>,
    pub transaction: transaction_plan_audit::PlanAudit,
    pub executable: bool,
    pub warning: String,
}

fn qualify(name: &str, arch: &str) -> String {
    format!("{name}:{arch}")
}

/// Eligibility for an upgrade candidate. Never treat an APT downgrade pin
/// or an installed hold as permission for a system-wide downgrade or upgrade.
fn is_upgrade(current: &str, candidate: &str, held: bool) -> Result<bool> {
    Ok(!held && version::compare(candidate, current)? == Ordering::Greater)
}

/// Audit a bounded, no-removals upgrade of the *complete* installed native/all
/// scope. If more roots need upgrades than the solver can jointly evaluate,
/// REFUSE the request instead of silently previewing a misleading subset.
///
/// Every proposed root must originate in an authenticated RPT index and pass
/// its exact candidate-policy selection before running the joint dependency
/// solver. The returned report is never an execution authorization.
pub fn preview(
    cache: &Path, status: &Path, apt_root: &Path, architecture: &str,
    max_roots: usize, max_packages: usize,
) -> Result<SystemUpgradeAudit> {
    if architecture.is_empty() || architecture == "all"
        || !architecture.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        bail!("invalid native upgrade architecture");
    }
    if max_roots == 0 || max_roots > 128 {
        bail!("system upgrade preview supports max_roots 1..=128");
    }
    if max_packages == 0 || max_packages > 5000 {
        bail!("max_packages must be 1..=5000");
    }
    let installed = cache_query::installed_catalog_by_arch(status)?;
    let catalog = metadata_query::catalog_by_arch(cache, architecture, 1_000_000)?;
    let preferences = apt_preferences::load(apt_root)?;
    let target_release = policy::read_default_release(apt_root)?;
    // An unsupported preference is a global failure even if it happens
    // not to apply to a currently installed binary.
    apt_pin_eval::evaluate_with_phasing(
        "rpt-system-upgrade-validation", &[], &preferences, target_release.as_deref(), None,
    )?;

    let mut roots = Vec::new();
    let mut installed_only = Vec::new();
    let mut held = Vec::new();
    let mut unchanged = 0usize;
    let mut scanned = 0usize;
    let mut seen_names = BTreeSet::new();

    for (key, entry) in &installed {
        if key.architecture != architecture && key.architecture != "all" { continue; }
        scanned += 1;
        if !seen_names.insert(key.name.clone()) {
            bail!("ambiguous installed native/Architecture: all identity: {}", key.name);
        }
        if entry.status == "hold ok installed" {
            held.push(qualify(&key.name, &key.architecture));
            continue;
        }
        if !catalog.contains_key(&metadata_query::native_key_for(
            &catalog, &key.name, architecture,
        ).unwrap_or(key.clone())) {
            installed_only.push(qualify(&key.name, &key.architecture));
            continue;
        }
        let catalog_key = metadata_query::native_key_for(&catalog, &key.name, architecture)?;
        if catalog_key.architecture != key.architecture {
            bail!("repository changed architecture of installed {}:{}",
                key.name, key.architecture);
        }
        let origins = package_provenance::verified_version_origins(
            cache, &key.name, architecture,
        )?;
        let candidates = origins.iter().filter(|o| o.architecture == key.architecture)
            .cloned().collect::<Vec<_>>();
        let selected = apt_pin_eval::preview_candidate_with_phasing(
            &key.name, &candidates, &preferences, Some(&entry.version),
            target_release.as_deref(), None,
        )?.candidate;
        let Some(candidate) = selected else {
            unchanged += 1;
            continue;
        };
        if !is_upgrade(&entry.version, &candidate, false)? {
            unchanged += 1;
            continue;
        }
        if !candidates.iter().any(|origin| origin.version == candidate) {
            bail!("upgrade candidate {}:{} {} lacks authenticated version origin",
                key.name, key.architecture, candidate);
        }
        let verified = metadata_query::verified_exact_version(
            cache, architecture, &key.name, &candidate, &candidates,
        )?;
        if verified.architecture != key.architecture {
            bail!("upgrade candidate architecture switch forbidden");
        }
        roots.push(key.name.clone());
        if roots.len() > max_roots {
            bail!("system upgrade needs more than {max_roots} roots; refusing a partial upgrade. Increase --max-roots only within the bounded solver limit (128)");
        }
    }

    let steps = if roots.is_empty() {
        Vec::new()
    } else {
        install_plan::preview_many_with_policy(
            cache, status, apt_root, &roots, architecture, max_packages,
        ).context("joint signed candidate upgrade preview failed")?
    };
    let transaction = transaction_plan_audit::classify(&steps)?;
    if transaction.downgrades > 0 || transaction.reinstalls > 0 {
        bail!("system-wide upgrade preview cannot downgrade or reinstall packages");
    }
    for change in &transaction.changes {
        let key = cache_query::InstalledKey {
            name: change.package.clone(), architecture: change.architecture.clone(),
        };
        if installed.get(&key).is_some_and(|item| item.status == "hold ok installed") {
            bail!("joint upgrade would modify held {}:{}", key.name, key.architecture);
        }
    }
    for root in &roots {
        if !transaction.changes.iter().any(|step|
            step.package == *root && matches!(
                step.action, transaction_plan_audit::Action::Upgrade
            )) {
            bail!("joint solver did not preserve required upgrade root {root}");
        }
    }
    Ok(SystemUpgradeAudit {
        scanned_installed: scanned,
        unchanged_or_policy_kept: unchanged,
        installed_only,
        held,
        requested_upgrades: roots,
        transaction,
        executable: false,
        warning: "Read-only bounded no-removal upgrade preview, NOT apt-get upgrade/full-upgrade parity. No dpkg operations, no live-host execution; unsupported installed dependency/reverse-dependency transitions may require additional APT simulation checks.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_eligibility_uses_debian_ordering_and_holds() {
        assert!(is_upgrade("1.0~rc1", "1.0", false).unwrap());
        assert!(is_upgrade("2.0", "1:1.0", false).unwrap());
        assert!(!is_upgrade("2.0", "1.0", false).unwrap());
        assert!(!is_upgrade("2.0", "2.0", false).unwrap());
        assert!(!is_upgrade("1.0", "2.0", true).unwrap());
    }

    #[test]
    fn invalid_scope_rejected_before_file_access() {
        for arch in ["", "all", "../amd64"] {
            assert!(preview(
                Path::new("/no"), Path::new("/no"), Path::new("/no"),
                arch, 8, 100,
            ).is_err());
        }
        assert!(preview(
            Path::new("/no"), Path::new("/no"), Path::new("/no"),
            "amd64", 129, 100,
        ).is_err());
    }
}

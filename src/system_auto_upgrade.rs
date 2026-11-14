//! Conservative automatic *proposal* of removals for signed upgrades.
//! This is a bounded read-only scenario search, not an APT dist-upgrade SAT
//! resolver and never a dpkg transaction/execution permission.
use crate::{removal_planner, system_full_upgrade, system_upgrade};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fs, path::Path};

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AutoUpgradeSelection {
    NoRemoval { audit: system_upgrade::SystemUpgradeAudit },
    RemovalScenario { scenario: system_full_upgrade::JointUpgradeScenario },
}

#[derive(Debug, Serialize)]
pub struct AutoFullUpgradeAudit {
    pub schema_version: u32,
    pub original_status_sha256: String,
    /// APT candidate-policy verified installed blockers, never arbitrary
    /// installed binaries or unsigned index entries.
    pub removal_candidates: Vec<String>,
    pub combinations_examined: usize,
    pub selected_removals: Vec<String>,
    pub selection: AutoUpgradeSelection,
    pub executable: bool,
    pub warning: String,
}

fn candidate_subsets(candidates: &[String], size: usize) -> Vec<Vec<String>> {
    fn collect(
        candidates: &[String], size: usize, next: usize,
        current: &mut Vec<String>, output: &mut Vec<Vec<String>>,
    ) {
        if current.len() == size {
            output.push(current.clone());
            return;
        }
        for i in next..candidates.len() {
            current.push(candidates[i].clone());
            collect(candidates, size, i + 1, current, output);
            current.pop();
        }
    }
    let mut output = Vec::new();
    collect(candidates, size, 0, &mut Vec::new(), &mut output);
    output
}

/// First try the already globally audited no-removal upgrade. Only on
/// failure may a small, signed-conflict-derived installed blocker subset
/// be considered. An automatic proposal is returned only when EXACTLY ONE
/// minimum-size removal set yields a globally safe signed upgrade plan.
///
/// APT dist-upgrade uses a wider SAT/search/ranking policy; this deliberately
/// reports ambiguity rather than picking an arbitrary equivalent subset.
pub fn preview(
    cache: &Path, status: &Path, apt_root: &Path, architecture: &str,
    max_roots: usize, max_packages: usize, max_removals: usize,
) -> Result<AutoFullUpgradeAudit> {
    if !(1..=3).contains(&max_removals) {
        bail!("bounded automatic removal search supports max_removals 1..=3");
    }
    if !(1..=128).contains(&max_roots) || !(1..=5000).contains(&max_packages) {
        bail!("unsupported solver bound for automatic full-upgrade");
    }
    let start = fs::read(status).context("reading original dpkg status")?;
    if start.len() > 64 * 1024 * 1024 {
        bail!("original dpkg status exceeds 64 MiB");
    }
    let digest = hex::encode(Sha256::digest(&start));
    let check_stable = || -> Result<()> {
        if hex::encode(Sha256::digest(fs::read(status)?)) != digest {
            bail!("original dpkg status changed during automatic scenario search");
        }
        Ok(())
    };
    if let Ok(no_removal) = system_upgrade::preview(
        cache, status, apt_root, architecture, max_roots, max_packages,
    ) {
        check_stable()?;
        return Ok(AutoFullUpgradeAudit {
            schema_version: 1, original_status_sha256: digest,
            removal_candidates: Vec::new(), combinations_examined: 0,
            selected_removals: Vec::new(),
            selection: AutoUpgradeSelection::NoRemoval { audit: no_removal },
            executable: false,
            warning: "Read-only signed no-removal system upgrade; no automatic removals needed. Not APT dist-upgrade parity or dpkg execution.".into(),
        });
    }
    let discovery = system_upgrade::discover_signed_upgrades(
        cache, status, apt_root, architecture, max_roots,
    ).context("unable to authenticate exact candidate upgrades")?;
    if discovery.roots.is_empty() {
        bail!("no candidate upgrade roots and no globally valid no-removal plan");
    }
    let blockers = removal_planner::signed_upgrade_removal_blockers(
        &start, architecture, &discovery.candidates,
    ).context("automatic removal candidates must originate from signed new-version conflicts")?;
    let candidate_names = blockers.iter().map(|key|
        format!("{}:{}", key.name, key.architecture)).collect::<Vec<_>>();
    if candidate_names.is_empty() {
        bail!("no signed Conflicts/Breaks removal candidates; global no-removal failure requires manual review");
    }

    // All combinations at each cardinality must be evaluated. A first-fit
    // shortcut here would silently invent a removal preference absent from
    // the supported APT policy model. The bounded space is <= 92 scenarios.
    let mut attempts = 0usize;
    let mut selected = None;
    for size in 1..=max_removals.min(candidate_names.len()) {
        let mut success = None;
        for subset in candidate_subsets(&candidate_names, size) {
            attempts += 1;
            if attempts > 92 {
                bail!("automatic removal search budget exhausted");
            }
            let Ok(scenario) = system_full_upgrade::preview(
                cache, status, apt_root, architecture,
                &subset, max_roots, max_packages,
            ) else {
                continue;
            };
            // No caller-selected root may disappear just because some
            // irrelevant installed blocker was hypothetically removed.
            if scenario.upgrades.requested_upgrades != discovery.roots
                || scenario.upgrades.transaction.upgrades == 0
                || scenario.final_state.removed_binaries != subset.len() {
                continue;
            }
            if success.is_some() {
                bail!("multiple equally small valid signed removal sets; automatic selection is ambiguous and requires operator choice");
            }
            success = Some((subset, scenario));
        }
        if success.is_some() {
            selected = success;
            break;
        }
    }
    let Some((selected_removals, scenario)) = selected else {
        bail!("no globally valid signed full-upgrade scenario within the bounded explicit-removal subset; manual review required");
    };
    check_stable()?;
    Ok(AutoFullUpgradeAudit {
        schema_version: 1, original_status_sha256: digest,
        removal_candidates: candidate_names, combinations_examined: attempts,
        selected_removals,
        selection: AutoUpgradeSelection::RemovalScenario { scenario },
        executable: false,
        warning: "Conservative bounded automatic *proposal*, NOT APT dist-upgrade removal ranking/parity. Only uniquely minimal sets among <=8 signed Conflicts/Breaks blockers and <=3 removals are considered. Never executable, no native-rootfs removal intent or host dpkg changes.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combinatorial_search_enumerates_all_minimal_sets() {
        let pool = vec!["a:amd64".into(), "b:amd64".into(), "c:amd64".into()];
        assert_eq!(candidate_subsets(&pool, 1),
            vec![vec![pool[0].clone()], vec![pool[1].clone()], vec![pool[2].clone()]]);
        assert_eq!(candidate_subsets(&pool, 2),
            vec![
                vec![pool[0].clone(),pool[1].clone()],
                vec![pool[0].clone(),pool[2].clone()],
                vec![pool[1].clone(),pool[2].clone()],
            ]);
        assert_eq!(candidate_subsets(&pool, 3), vec![pool.clone()]);
        assert!(candidate_subsets(&pool, 4).is_empty());
    }

    #[test]
    fn strict_search_limits_are_checked_before_io() {
        assert!(preview(Path::new("/missing"),Path::new("/missing"),
            Path::new("/missing"),"amd64",128,200,0).is_err());
        assert!(preview(Path::new("/missing"),Path::new("/missing"),
            Path::new("/missing"),"amd64",128,200,4).is_err());
        assert!(preview(Path::new("/missing"),Path::new("/missing"),
            Path::new("/missing"),"amd64",129,200,1).is_err());
    }
}

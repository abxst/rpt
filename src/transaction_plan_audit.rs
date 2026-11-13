//! Read-only transaction classification. A report is never an execution authorization.
use crate::{install_plan::PlanStep, version};
use anyhow::{bail, Result};
use serde::Serialize;
use std::{cmp::Ordering, collections::BTreeSet};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Action { Install, Upgrade, Downgrade, Reinstall }

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Change {
    pub package: String,
    pub architecture: String,
    pub from: Option<String>,
    pub to: String,
    pub action: Action,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlanAudit {
    pub changes: Vec<Change>,
    pub installs: usize,
    pub upgrades: usize,
    pub downgrades: usize,
    pub reinstalls: usize,
    pub executable: bool,
    pub warning: String,
}

/// Classify a candidate-verified plan without inferring removals or modifying dpkg.
pub fn classify(steps: &[PlanStep]) -> Result<PlanAudit> {
    let mut identities = BTreeSet::new();
    let mut changes = Vec::with_capacity(steps.len());
    let mut installs = 0;
    let mut upgrades = 0;
    let mut downgrades = 0;
    let mut reinstalls = 0;
    for step in steps {
        if !identities.insert((&step.package, &step.architecture)) {
            bail!("duplicate planned binary identity {}:{}", step.package, step.architecture);
        }
        version::compare(&step.version, &step.version)?;
        let action = match step.installed.as_deref() {
            None => { installs += 1; Action::Install }
            Some(current) => match version::compare(&step.version, current)? {
                Ordering::Greater => { upgrades += 1; Action::Upgrade }
                Ordering::Less => { downgrades += 1; Action::Downgrade }
                Ordering::Equal => { reinstalls += 1; Action::Reinstall }
            },
        };
        changes.push(Change {
            package: step.package.clone(),
            architecture: step.architecture.clone(),
            from: step.installed.clone(),
            to: step.version.clone(),
            action,
        });
    }
    Ok(PlanAudit {
        changes, installs, upgrades, downgrades, reinstalls,
        executable: false,
        warning: "Read-only preview. Removal, autoremove, dpkg execution and full APT parity are NOT implemented.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn step(name: &str, current: Option<&str>, target: &str) -> PlanStep {
        PlanStep {
            package: name.into(), architecture: "amd64".into(),
            version: target.into(), installed: current.map(str::to_owned),
        }
    }
    #[test]
    fn classifies_all_actions_without_execution() {
        let report = classify(&[
            step("fresh", None, "1"), step("newer", Some("1"), "2"),
            step("older", Some("2"), "1"), step("same", Some("1"), "1"),
        ]).unwrap();
        assert_eq!((report.installs, report.upgrades, report.downgrades, report.reinstalls), (1,1,1,1));
        assert!(!report.executable);
        assert_eq!(report.changes[2].action, Action::Downgrade);
    }
    #[test]
    fn rejects_duplicate_identities() {
        assert!(classify(&[step("dup", None, "1"), step("dup", Some("1"), "2")]).is_err());
    }
    #[test]
    fn debian_epoch_version_order_is_respected() {
        assert_eq!(classify(&[step("epoch", Some("2.0"), "1:1.0")]).unwrap().upgrades, 1);
    }
}

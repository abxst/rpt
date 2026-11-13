//! Conservative removal impact inspection against installed dpkg status.
//! Read-only. Not an APT removal/autoremove resolver.
use crate::{cache_query::{self, InstalledKey}, package_index};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{collections::{BTreeMap, BTreeSet}, fs, path::Path};

#[derive(Debug, Serialize)]
pub struct RemovalImpact {
    pub requested: Vec<String>,
    pub blockers: Vec<String>,
    pub safe_to_remove: bool,
    pub executable: bool,
    pub warning: String,
}

/// Refuse complex dependency grammar rather than claiming a safe removal.
fn dependency_names(raw: &str) -> Result<Vec<String>> {
    if raw.trim().is_empty() { return Ok(Vec::new()); }
    let mut names = Vec::new();
    for group in raw.split(',') {
        if group.contains(['|', '[', ']', '<', '>', '!']) {
            bail!("complex dependency syntax requires full APT resolver: {group}");
        }
        let name = group.trim().split_once('(').map_or(group.trim(), |(n, _)| n.trim());
        if name.is_empty() || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-' | b':')) {
            bail!("invalid installed dependency: {group}");
        }
        names.push(name.split(':').next().unwrap_or(name).to_owned());
    }
    Ok(names)
}

pub fn inspect(status: &Path, architecture: &str, requested: &[String]) -> Result<RemovalImpact> {
    if requested.is_empty() || requested.len() > 128 { bail!("expected 1..=128 packages"); }
    if architecture.is_empty() || !architecture.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        bail!("invalid architecture");
    }
    let installed = cache_query::installed_catalog_by_arch(status)?;
    let bytes = fs::read(status)?;
    if bytes.len() > 64 * 1024 * 1024 { bail!("dpkg status exceeds safe size"); }
    let stanzas = package_index::parse(std::str::from_utf8(&bytes)?)?;
    let mut relations = BTreeMap::<InstalledKey, (Vec<String>, bool)>::new();
    for stanza in stanzas {
        if !stanza.field("Status").is_some_and(cache_query::is_installed_status) { continue; }
        let key = InstalledKey {
            name: stanza.field("Package").context("missing installed package")?.to_owned(),
            architecture: stanza.field("Architecture").context("missing installed architecture")?.to_owned(),
        };
        let depends = dependency_names(stanza.field("Depends").unwrap_or(""))?;
        let pre = dependency_names(stanza.field("Pre-Depends").unwrap_or(""))?;
        let protected = stanza.field("Essential") == Some("yes")
            || stanza.field("Protected") == Some("yes")
            || stanza.field("Status") == Some("hold ok installed");
        if relations.insert(key, (depends.into_iter().chain(pre).collect(), protected)).is_some() {
            bail!("duplicate installed relation");
        }
    }
    let mut targets = BTreeSet::new();
    for name in requested {
        if name.len() < 2 || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'.' | b'-')) {
            bail!("invalid requested package: {name}");
        }
        if !targets.insert(name.clone()) { bail!("duplicate requested package: {name}"); }
        let matches = installed.keys().filter(|k| k.name == *name && (k.architecture == architecture || k.architecture == "all")).count();
        if matches != 1 { bail!("requested package {name} is missing or ambiguous for {architecture}"); }
    }
    let mut blockers = BTreeSet::new();
    for (key, (dependencies, protected)) in &relations {
        if targets.contains(&key.name) && *protected {
            blockers.insert(format!("{}:{} is held, Essential or Protected", key.name, key.architecture));
        }
        if targets.contains(&key.name) { continue; }
        for dependency in dependencies {
            if targets.contains(dependency) {
                blockers.insert(format!("{}:{} depends on {}", key.name, key.architecture, dependency));
            }
        }
    }
    Ok(RemovalImpact {
        requested: targets.into_iter().collect(),
        safe_to_remove: blockers.is_empty(),
        blockers: blockers.into_iter().collect(),
        executable: false,
        warning: "Read-only conservative impact inspection; no dependency alternatives, Provides, Multi-Arch removal, autoremove, dpkg execution or APT parity.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn grammar_refuses_alternatives() {
        assert!(dependency_names("foo | bar").is_err());
        assert_eq!(dependency_names("foo (>= 1), bar").unwrap(), vec!["foo", "bar"]);
    }
    #[test]
    fn reports_reverse_dependency_and_protected_target() {
        let file = std::env::temp_dir().join(format!("rpt-removal-impact-{}", std::process::id()));
        fs::write(&file, "Package: libfoo\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nEssential: yes\n\nPackage: appfoo\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: libfoo (>= 1)\n\n").unwrap();
        let report = inspect(&file, "amd64", &["libfoo".into()]).unwrap();
        assert!(!report.safe_to_remove);
        assert!(!report.executable);
        assert_eq!(report.blockers.len(), 2);
        let _ = fs::remove_file(file);
    }
}

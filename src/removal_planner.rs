//! Read-only removal and autoremove transaction planning from installed dpkg state.
//! This is a conservative preview, not an authorization to invoke dpkg.
use crate::{cache_query::{self, InstalledKey}, package_index, version};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{cmp::Ordering, collections::{BTreeMap, BTreeSet, VecDeque}, fs, path::Path};

#[derive(Debug, Clone)]
struct Requirement {
    name: String,
    arch: Option<String>,
    relation: Option<(String, String)>,
}

#[derive(Debug)]
struct InstalledPackage {
    version: String,
    multi_arch: String,
    depends: Vec<Vec<Requirement>>,
    recommends: Vec<Vec<Requirement>>,
    provides: Vec<Requirement>,
    protected: bool,
}

#[derive(Debug, Serialize)]
pub struct RemovalPlan {
    pub mode: String,
    pub remove: Vec<String>,
    pub blockers: Vec<String>,
    pub removable: bool,
    pub executable: bool,
    pub warning: String,
}

/// Avoid synthetic package identities and unsupported dpkg syntax.
fn checked_name(name: &str) -> Result<()> {
    if name.len() < 2
        || !name.as_bytes()[0].is_ascii_lowercase() && !name.as_bytes()[0].is_ascii_digit()
        || !name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()
            || matches!(b, b'+' | b'.' | b'-')) {
        bail!("invalid package name: {name}");
    }
    Ok(())
}
fn checked_arch(arch: &str) -> Result<()> {
    if arch.is_empty() || !arch.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
        bail!("invalid architecture: {arch}");
    }
    Ok(())
}
fn parse_requirement(raw: &str) -> Result<Requirement> {
    let raw = raw.trim();
    if raw.contains(['[', ']', '!', '<', '>']) && !raw.contains('(') {
        bail!("unsupported dependency restriction: {raw}");
    }
    let (head, relation) = match raw.split_once('(') {
        Some((head, tail)) if tail.ends_with(')') => {
            let parts = tail[..tail.len()-1].split_whitespace().collect::<Vec<_>>();
            if parts.len() != 2 || !matches!(parts[0], "<<" | "<=" | "=" | ">=" | ">>") {
                bail!("unsupported version relation: {raw}");
            }
            version::compare(parts[1], parts[1])?;
            (head.trim(), Some((parts[0].into(), parts[1].into())))
        }
        Some(_) => bail!("malformed dependency: {raw}"),
        None => (raw, None),
    };
    let (name, arch) = match head.split_once(':') {
        Some((name, "any")) => bail!("Multi-Arch :any is not supported in removal planning: {name}"),
        Some((name, arch)) => {
            if arch != "native" { checked_arch(arch)?; }
            (name, Some(arch.to_owned()))
        }
        None => (head, None),
    };
    checked_name(name)?;
    Ok(Requirement { name: name.into(), arch, relation })
}
fn parse_groups(raw: &str) -> Result<Vec<Vec<Requirement>>> {
    if raw.trim().is_empty() { return Ok(Vec::new()); }
    raw.split(',').map(|group|
        group.split('|').map(parse_requirement).collect::<Result<Vec<_>>>()
    ).collect()
}
fn parse_provides(raw: &str) -> Result<Vec<Requirement>> {
    if raw.trim().is_empty() { return Ok(Vec::new()); }
    let mut provided = Vec::new();
    for token in raw.split(',') {
        if token.contains('|') { bail!("invalid Provides alternatives"); }
        let req = parse_requirement(token)?;
        if req.arch.is_some() || req.relation.as_ref().is_some_and(|(op, _)| op != "=") {
            bail!("unsupported versioned Provides: {token}");
        }
        provided.push(req);
    }
    Ok(provided)
}
fn relation_matches(version_string: &str, relation: &Option<(String, String)>) -> Result<bool> {
    let Some((op, required)) = relation else { return Ok(true) };
    let cmp = version::compare(version_string, required)?;
    Ok(match op.as_str() {
        "<<" => cmp == Ordering::Less,
        "<=" => cmp != Ordering::Greater,
        "=" => cmp == Ordering::Equal,
        ">=" => cmp != Ordering::Less,
        ">>" => cmp == Ordering::Greater,
        _ => false,
    })
}
fn accepted_arch(req: &Requirement, source_arch: &str, candidate_arch: &str, native: &str,
                 candidate: &InstalledPackage) -> bool {
    match req.arch.as_deref() {
        Some("native") => candidate_arch == native || candidate_arch == "all",
        Some(exact) => candidate_arch == exact,
        None => candidate_arch == source_arch || candidate_arch == "all"
            || (candidate.multi_arch == "foreign" && candidate_arch == native),
    }
}
fn satisfiers(
    packages: &BTreeMap<InstalledKey, InstalledPackage>, group: &[Requirement],
    source_arch: &str, native: &str,
) -> Result<BTreeSet<InstalledKey>> {
    let mut hits = BTreeSet::new();
    for req in group {
        for (key, candidate) in packages {
            if !accepted_arch(req, source_arch, &key.architecture, native, candidate) { continue; }
            if req.name == key.name && relation_matches(&candidate.version, &req.relation)? {
                hits.insert(key.clone());
                continue;
            }
            for virtual_cap in &candidate.provides {
                if virtual_cap.name != req.name { continue; }
                if req.relation.is_none() {
                    hits.insert(key.clone());
                } else if let Some((_, provided_version)) = &virtual_cap.relation {
                    if relation_matches(provided_version, &req.relation)? {
                        hits.insert(key.clone());
                    }
                }
            }
        }
    }
    Ok(hits)
}
fn read_installed(status: &Path) -> Result<BTreeMap<InstalledKey, InstalledPackage>> {
    let verified = cache_query::installed_catalog_by_arch(status)?;
    let data = fs::read(status)?;
    if data.len() > 64 * 1024 * 1024 { bail!("dpkg status exceeds 64 MiB"); }
    let mut installed = BTreeMap::new();
    for record in package_index::parse(std::str::from_utf8(&data)?)? {
        if !record.field("Status").is_some_and(cache_query::is_installed_status) { continue; }
        let key = InstalledKey {
            name: record.field("Package").context("missing package name")?.into(),
            architecture: record.field("Architecture").context("missing architecture")?.into(),
        };
        let version = record.field("Version").context("missing version")?.to_owned();
        if verified.get(&key).is_none_or(|pkg| pkg.version != version) {
            bail!("inconsistent installed metadata for {}:{}", key.name, key.architecture);
        }
        let multi_arch = record.field("Multi-Arch").unwrap_or("no").to_owned();
        if !matches!(multi_arch.as_str(), "no" | "same" | "foreign" | "allowed") {
            bail!("invalid Multi-Arch declaration for {}", key.name);
        }
        let critical = |field: &str| -> Result<bool> {
            match record.field(field) {
                None | Some("no") => Ok(false),
                Some("yes") => Ok(true),
                _ => bail!("invalid {field} for {}", key.name),
            }
        };
        let entry = InstalledPackage {
            version,
            multi_arch,
            depends: parse_groups(record.field("Depends").unwrap_or(""))?
                .into_iter().chain(parse_groups(record.field("Pre-Depends").unwrap_or(""))?).collect(),
            recommends: parse_groups(record.field("Recommends").unwrap_or(""))?,
            provides: parse_provides(record.field("Provides").unwrap_or(""))?,
            protected: record.field("Status") == Some("hold ok installed")
                || critical("Essential")? || critical("Protected")?,
        };
        if installed.insert(key.clone(), entry).is_some() {
            bail!("duplicate installed identity: {}:{}", key.name, key.architecture);
        }
    }
    if installed.len() != verified.len() { bail!("incomplete installed package projection"); }
    Ok(installed)
}
fn key_label(key: &InstalledKey) -> String { format!("{}:{}", key.name, key.architecture) }

/// Ensure the initial installed state is consistent in the supported subset.
fn ensure_initial_dependencies(
    installed: &BTreeMap<InstalledKey, InstalledPackage>, native: &str,
) -> Result<()> {
    for (key, record) in installed {
        for group in &record.depends {
            if satisfiers(installed, group, &key.architecture, native)?.is_empty() {
                bail!("pre-existing unsatisfied dependency for {}; refuse removal preview", key_label(key));
            }
        }
    }
    Ok(())
}
fn removal_blockers(
    installed: &BTreeMap<InstalledKey, InstalledPackage>, removed: &BTreeSet<InstalledKey>, native: &str,
) -> Result<BTreeSet<String>> {
    let remaining = installed.iter().filter(|(key, _)| !removed.contains(*key))
        .map(|(key, value)| (key.clone(), InstalledPackage {
            version: value.version.clone(), multi_arch: value.multi_arch.clone(),
            depends: value.depends.clone(), recommends: Vec::new(),
            provides: value.provides.clone(), protected: value.protected,
        })).collect::<BTreeMap<_, _>>();
    let mut blockers = BTreeSet::new();
    for key in removed {
        if installed.get(key).is_some_and(|p| p.protected) {
            blockers.insert(format!("{} is held, Essential or Protected", key_label(key)));
        }
    }
    for (key, record) in &remaining {
        for group in &record.depends {
            if satisfiers(&remaining, group, &key.architecture, native)?.is_empty() {
                blockers.insert(format!("removing targets breaks dependencies of {}", key_label(key)));
            }
        }
    }
    Ok(blockers)
}
fn result(mode: &str, removed: &BTreeSet<InstalledKey>, blockers: BTreeSet<String>) -> RemovalPlan {
    RemovalPlan {
        mode: mode.into(),
        remove: removed.iter().map(key_label).collect(),
        removable: blockers.is_empty(),
        blockers: blockers.into_iter().collect(),
        executable: false,
        warning: "Read-only candidate plan only. APT removal/auto-mark parity, maintainer scripts and dpkg execution not implemented.".into(),
    }
}

/// Plan explicit removals. With cascade, compute reverse-dependent fixed point.
pub fn remove(status: &Path, native: &str, requested: &[String], cascade: bool) -> Result<RemovalPlan> {
    checked_arch(native)?;
    if requested.is_empty() || requested.len() > 128 { bail!("expected 1..=128 packages"); }
    let installed = read_installed(status)?;
    ensure_initial_dependencies(&installed, native)?;
    let mut removed = BTreeSet::new();
    for request in requested {
        let req = parse_requirement(request)?;
        if req.relation.is_some() || req.arch.as_deref() == Some("native") {
            bail!("removal root must be a binary identity, optionally :architecture");
        }
        let matches = installed.keys().filter(|key| key.name == req.name
            && match req.arch.as_deref() {
                Some(arch) => key.architecture == arch,
                None => key.architecture == native || key.architecture == "all",
            }).cloned().collect::<Vec<_>>();
        if matches.len() != 1 { bail!("requested binary missing or ambiguous: {request}"); }
        if !removed.insert(matches[0].clone()) { bail!("duplicate removal root: {request}"); }
    }
    if cascade {
        for _ in 0..installed.len() {
            let remaining = installed.iter().filter(|(key, _)| !removed.contains(*key))
                .map(|(key, _)| key.clone()).collect::<Vec<_>>();
            let survivors = installed.iter().filter(|(key, _)| !removed.contains(*key))
                .map(|(key, p)| (key.clone(), InstalledPackage {
                    version: p.version.clone(), multi_arch: p.multi_arch.clone(),
                    depends: p.depends.clone(), recommends: Vec::new(),
                    provides: p.provides.clone(), protected: p.protected,
                })).collect::<BTreeMap<_, _>>();
            let mut next = BTreeSet::new();
            for key in remaining {
                for group in &installed[&key].depends {
                    if satisfiers(&survivors, group, &key.architecture, native)?.is_empty() {
                        next.insert(key.clone());
                        break;
                    }
                }
            }
            if next.is_empty() { break; }
            removed.extend(next);
        }
    }
    Ok(result("remove", &removed, removal_blockers(&installed, &removed, native)?))
}
fn auto_marks(path: &Path, installed: &BTreeMap<InstalledKey, InstalledPackage>) -> Result<BTreeSet<InstalledKey>> {
    let bytes = fs::read(path).with_context(|| format!("reading APT extended states: {}", path.display()))?;
    if bytes.len() > 64 * 1024 * 1024 { bail!("APT extended states exceed 64 MiB"); }
    let mut auto = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for record in package_index::parse(std::str::from_utf8(&bytes)?)? {
        let name = record.field("Package").context("APT state entry without Package")?;
        checked_name(name)?;
        let arch = match record.field("Architecture") {
            Some(arch) => arch.to_owned(),
            None => {
                let matches = installed.keys().filter(|key| key.name == name).collect::<Vec<_>>();
                if matches.len() != 1 { bail!("ambiguous architecture in extended state for {name}"); }
                matches[0].architecture.clone()
            }
        };
        checked_arch(&arch)?;
        let key = InstalledKey { name: name.into(), architecture: arch };
        if !seen.insert(key.clone()) { bail!("duplicate extended state: {}", key_label(&key)); }
        match record.field("Auto-Installed").unwrap_or("0") {
            "1" if installed.contains_key(&key) => { auto.insert(key); },
            "0" | "1" => {},
            other => bail!("unsupported Auto-Installed marker: {other}"),
        }
    }
    Ok(auto)
}

/// Conservative autoremove: mark all satisfiers reachable from manual roots,
/// essential/held packages and foreign architecture packages.
pub fn autoremove(status: &Path, extended_states: &Path, native: &str) -> Result<RemovalPlan> {
    checked_arch(native)?;
    let installed = read_installed(status)?;
    ensure_initial_dependencies(&installed, native)?;
    let auto = auto_marks(extended_states, &installed)?;
    let mut keep = BTreeSet::new();
    let mut queue = VecDeque::new();
    for (key, entry) in &installed {
        if !auto.contains(key) || entry.protected || (key.architecture != native && key.architecture != "all") {
            keep.insert(key.clone());
            queue.push_back(key.clone());
        }
    }
    while let Some(key) = queue.pop_front() {
        let entry = &installed[&key];
        for group in entry.depends.iter().chain(entry.recommends.iter()) {
            // Keep every valid alternative, not just the first one. This
            // intentionally retains more packages than apt to avoid unsafe guesses.
            for provider in satisfiers(&installed, group, &key.architecture, native)? {
                if keep.insert(provider.clone()) { queue.push_back(provider); }
            }
        }
    }
    let remove = auto.into_iter().filter(|key| !keep.contains(key)).collect::<BTreeSet<_>>();
    Ok(result("autoremove", &remove, removal_blockers(&installed, &remove, native)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(name: &str, contents: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!("rpt-plan-{name}-{}", std::process::id()));
        fs::write(&path, contents).unwrap();
        path
    }
    #[test]
    fn alternatives_allow_removing_one_provider() {
        let status = fixture("alt", "Package: apppkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: firstpkg | secondpkg\n\nPackage: firstpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\nPackage: secondpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n");
        let plan = remove(&status, "amd64", &["firstpkg".into()], false).unwrap();
        assert!(plan.removable);
        assert!(!plan.executable);
        let blocked = remove(&status, "amd64", &["firstpkg".into(), "secondpkg".into()], false).unwrap();
        assert!(!blocked.removable);
        fs::remove_file(status).unwrap();
    }
    #[test]
    fn cascade_removes_reverse_dependencies() {
        let status = fixture("cascade", "Package: apppkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: libpkg\n\nPackage: libpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n");
        let report = remove(&status, "amd64", &["libpkg".into()], true).unwrap();
        assert_eq!(report.remove, ["apppkg:amd64", "libpkg:amd64"]);
        assert!(report.removable);
        fs::remove_file(status).unwrap();
    }
    #[test]
    fn autoremove_retains_dependencies_of_manual_roots() {
        let status = fixture("auto-status", "Package: apppkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: libpkg\n\nPackage: libpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\nPackage: orphanpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n");
        let states = fixture("auto-marks", "Package: libpkg\nArchitecture: amd64\nAuto-Installed: 1\n\nPackage: orphanpkg\nArchitecture: amd64\nAuto-Installed: 1\n\n");
        let plan = autoremove(&status, &states, "amd64").unwrap();
        assert_eq!(plan.remove, ["orphanpkg:amd64"]);
        assert!(plan.removable);
        assert!(!plan.executable);
        fs::remove_file(status).unwrap(); fs::remove_file(states).unwrap();
    }
    #[test]
    fn versioned_virtual_provides_are_not_provider_versions() {
        let status = fixture("virtual", "Package: apppkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: api-virtual (>= 2)\n\nPackage: providerpkg\nArchitecture: amd64\nVersion: 100\nStatus: install ok installed\nProvides: api-virtual (= 1)\n\n");
        assert!(remove(&status, "amd64", &["providerpkg".into()], false).is_err());
        fs::remove_file(status).unwrap();
    }
}

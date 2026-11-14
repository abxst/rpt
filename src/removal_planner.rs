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
    let effective_source = if source_arch == "all" { native } else { source_arch };
    match req.arch.as_deref() {
        Some("native") => candidate_arch == native || candidate_arch == "all",
        Some(exact) => candidate_arch == exact,
        None => candidate_arch == effective_source || candidate_arch == "all"
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
fn read_installed_bytes(data: &[u8]) -> Result<BTreeMap<InstalledKey, InstalledPackage>> {
    let verified = cache_query::installed_catalog_from_bytes(data)?;
    if data.len() > 64 * 1024 * 1024 { bail!("dpkg status exceeds 64 MiB"); }
    let mut installed = BTreeMap::new();
    for record in package_index::parse(std::str::from_utf8(data)?)? {
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
        warning: "Read-only removal intent (executable=false). Applying it requires a separate journaled, state-bound executor; full APT removal and autoremove parity is not guaranteed.".into(),
    }
}

/// Plan explicit removals. With cascade, compute reverse-dependent fixed point.
pub fn remove(status: &Path, native: &str, requested: &[String], cascade: bool) -> Result<RemovalPlan> {
    remove_snapshot(&fs::read(status)?, native, requested, cascade)
}

/// Plan against an exact dpkg status snapshot from an isolated container.
/// No host temporary status files are created.
pub fn remove_snapshot(status: &[u8], native: &str, requested: &[String], cascade: bool) -> Result<RemovalPlan> {
    checked_arch(native)?;
    if requested.is_empty() || requested.len() > 128 { bail!("expected 1..=128 packages"); }
    let installed = read_installed_bytes(status)?;
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
/// Purge may target both installed packages and packages whose only remaining
/// dpkg state is "deinstall ok config-files". The latter cannot satisfy
/// dependencies and must never be passed to the ordinary removal planner.
/// Dependency and Essential/Protected checks still apply to installed roots.
pub fn purge_snapshot(status: &[u8], native: &str, requested: &[String], cascade: bool) -> Result<RemovalPlan> {
    checked_arch(native)?;
    if requested.is_empty() || requested.len() > 128 { bail!("expected 1..=128 packages"); }
    let installed = read_installed_bytes(status)?;
    ensure_initial_dependencies(&installed, native)?;
    let mut residual = BTreeSet::new();
    for record in package_index::parse(std::str::from_utf8(status)?)? {
        if record.field("Status") != Some("deinstall ok config-files") { continue; }
        let name = record.field("Package").context("residual dpkg state without Package")?;
        let arch = record.field("Architecture").context("residual dpkg state without Architecture")?;
        checked_name(name)?;
        checked_arch(arch)?;
        let key = InstalledKey { name: name.into(), architecture: arch.into() };
        if installed.contains_key(&key) || !residual.insert(key.clone()) {
            bail!("duplicate residual dpkg identity: {}", key_label(&key));
        }
    }
    let mut installed_roots = Vec::new();
    let mut residual_roots = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for request in requested {
        let req = parse_requirement(request)?;
        if req.relation.is_some() || req.arch.as_deref() == Some("native") {
            bail!("purge root must be a binary identity, optionally :architecture");
        }
        let matches = installed.keys().chain(residual.iter())
            .filter(|key| key.name == req.name &&
                match req.arch.as_deref() {
                    Some(arch) => key.architecture == arch,
                    None => key.architecture == native || key.architecture == "all",
                })
            .cloned().collect::<Vec<_>>();
        if matches.len() != 1 { bail!("requested purge root missing or ambiguous: {request}"); }
        let key = &matches[0];
        if !seen.insert(key.clone()) { bail!("duplicate purge root: {request}"); }
        if installed.contains_key(key) {
            installed_roots.push(request.clone());
        } else {
            residual_roots.insert(key.clone());
        }
    }
    let mut plan = if installed_roots.is_empty() {
        result("purge", &BTreeSet::new(), BTreeSet::new())
    } else {
        remove_snapshot(status, native, &installed_roots, cascade)?
    };
    let mut targets = plan.remove.iter().cloned().collect::<BTreeSet<_>>();
    targets.extend(residual_roots.iter().map(key_label));
    plan.remove = targets.into_iter().collect();
    plan.mode = "purge".into();
    Ok(plan)
}

fn auto_marks_from_bytes(bytes: &[u8], installed: &BTreeMap<InstalledKey, InstalledPackage>, native: &str) -> Result<BTreeSet<InstalledKey>> {
    if bytes.len() > 64 * 1024 * 1024 { bail!("APT extended states exceed 64 MiB"); }
    let mut auto = BTreeSet::new();
    let mut seen = BTreeSet::new();
    for record in package_index::parse(std::str::from_utf8(bytes)?)? {
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
        let mut key = InstalledKey { name: name.into(), architecture: arch };
        // apt-mark stores Architecture: all binaries under the configured
        // *native* architecture in extended_states. dpkg status still records
        // Architecture: all; without this narrowly scoped projection we would
        // silently treat genuine auto-installed packages as manual.
        if key.architecture == native && !installed.contains_key(&key) {
            let independent = InstalledKey { name: name.into(), architecture: "all".into() };
            if installed.contains_key(&independent) {
                if installed.keys().any(|other|
                    other.name == name && other.architecture != "all") {
                    bail!("ambiguous APT native/all auto mark for {name}");
                }
                key = independent;
            }
        }
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
    autoremove_snapshot(&fs::read(status)?, &fs::read(extended_states)?, native)
}

/// Conservatively derive orphan candidates from an exact status and APT
/// extended_states snapshot, without writes to either file.
pub fn autoremove_snapshot(status: &[u8], extended_states: &[u8], native: &str) -> Result<RemovalPlan> {
    checked_arch(native)?;
    let installed = read_installed_bytes(status)?;
    ensure_initial_dependencies(&installed, native)?;
    let auto = auto_marks_from_bytes(extended_states, &installed, native)?;
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
    fn snapshots_and_file_plans_match() {
        let status = b"Package: rootpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: libpkg\n\nPackage: libpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n";
        let marked = b"Package: libpkg\nArchitecture: amd64\nAuto-Installed: 1\n\n";
        let snapshot = remove_snapshot(status, "amd64", &["libpkg".into()], false).unwrap();
        assert!(!snapshot.removable);
        assert_eq!(snapshot.remove, ["libpkg:amd64".to_owned()]);
        assert!(snapshot.warning.contains("journaled, state-bound executor"));
        assert!(!snapshot.warning.contains("dpkg execution not implemented"));
        let autoremove = autoremove_snapshot(status, marked, "amd64").unwrap();
        assert!(autoremove.remove.is_empty());
        assert!(autoremove.removable);
    }

    #[test]
    fn purge_accepts_residual_config_without_changing_remove_semantics() {
        let state = b"Package: configpkg\nArchitecture: all\nVersion: 1\nStatus: deinstall ok config-files\n\n\
Package: installedpkg\nArchitecture: amd64\nVersion: 2\nStatus: install ok installed\n\n";
        assert!(remove_snapshot(state, "amd64", &["configpkg".into()], false).is_err());
        let residual = purge_snapshot(state, "amd64", &["configpkg".into()], false).unwrap();
        assert_eq!(residual.mode, "purge");
        assert!(residual.removable);
        assert!(!residual.executable);
        assert_eq!(residual.remove, ["configpkg:all"]);

        let combined = purge_snapshot(state, "amd64",
            &["configpkg".into(), "installedpkg".into()], false).unwrap();
        assert_eq!(combined.remove, ["configpkg:all", "installedpkg:amd64"]);
        assert!(combined.removable);
        assert!(purge_snapshot(state, "amd64", &["configpkg".into(), "configpkg:all".into()], false).is_err());
        assert!(purge_snapshot(state, "amd64", &["missingpkg".into()], false).is_err());
    }

    #[test]
    fn purge_rejects_ambiguous_and_dependency_breaking_requests() {
        let ambiguous = b"Package: configpkg\nArchitecture: all\nVersion: 1\nStatus: deinstall ok config-files\n\n\
Package: configpkg\nArchitecture: amd64\nVersion: 1\nStatus: deinstall ok config-files\n\n";
        assert!(purge_snapshot(ambiguous, "amd64", &["configpkg".into()], false).is_err());
        assert_eq!(purge_snapshot(ambiguous, "amd64",
            &["configpkg:all".into()], false).unwrap().remove, ["configpkg:all"]);

        let required = b"Package: apppkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: libpkg\n\n\
Package: libpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n\
Package: configpkg\nArchitecture: all\nVersion: 1\nStatus: deinstall ok config-files\n\n";
        let plan = purge_snapshot(required, "amd64",
            &["libpkg".into(), "configpkg".into()], false).unwrap();
        assert!(!plan.removable);
        assert!(plan.blockers.iter().any(|b| b.contains("apppkg")));
        assert_eq!(plan.remove, ["configpkg:all", "libpkg:amd64"]);
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
        assert_eq!(report.remove, vec!["apppkg:amd64".to_owned(), "libpkg:amd64".to_owned()]);
        assert!(report.removable);
        fs::remove_file(status).unwrap();
    }
    #[test]
    fn autoremove_retains_dependencies_of_manual_roots() {
        let status = fixture("auto-status", "Package: apppkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: libpkg\n\nPackage: libpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\nPackage: orphanpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n");
        let states = fixture("auto-marks", "Package: libpkg\nArchitecture: amd64\nAuto-Installed: 1\n\nPackage: orphanpkg\nArchitecture: amd64\nAuto-Installed: 1\n\n");
        let plan = autoremove(&status, &states, "amd64").unwrap();
        assert_eq!(plan.remove, vec!["orphanpkg:amd64".to_owned()]);
        assert!(plan.removable);
        assert!(!plan.executable);
        fs::remove_file(status).unwrap(); fs::remove_file(states).unwrap();
    }
    #[test]
    fn apt_mark_native_architecture_maps_only_unambiguous_all_binaries() {
        let status = b"Package: commonpkg\nArchitecture: all\nVersion: 1\nStatus: install ok installed\n\n\
Package: nativepkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n";
        let native_mark = b"Package: commonpkg\nArchitecture: amd64\nAuto-Installed: 1\n\n";
        let plan = autoremove_snapshot(status, native_mark, "amd64").unwrap();
        assert!(plan.removable);
        assert_eq!(plan.remove, vec!["commonpkg:all"]);
        let foreign_mark = b"Package: commonpkg\nArchitecture: i386\nAuto-Installed: 1\n\n";
        assert!(autoremove_snapshot(status, foreign_mark, "amd64").unwrap().remove.is_empty());
        let duplicate = b"Package: commonpkg\nArchitecture: amd64\nAuto-Installed: 1\n\n\
Package: commonpkg\nArchitecture: all\nAuto-Installed: 1\n\n";
        assert!(autoremove_snapshot(status, duplicate, "amd64").is_err());
    }
    #[test]
    fn native_all_alias_rejects_ambiguous_installed_multiarch() {
        let status = b"Package: commonpkg\nArchitecture: all\nVersion: 1\nStatus: install ok installed\n\n\
Package: commonpkg\nArchitecture: i386\nVersion: 1\nStatus: install ok installed\n\n";
        let mark = b"Package: commonpkg\nArchitecture: amd64\nAuto-Installed: 1\n\n";
        assert!(autoremove_snapshot(status, mark, "amd64").is_err());
    }
    #[test]
    fn versioned_virtual_provides_are_not_provider_versions() {
        let status = fixture("virtual", "Package: apppkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: api-virtual (>= 2)\n\nPackage: providerpkg\nArchitecture: amd64\nVersion: 100\nStatus: install ok installed\nProvides: api-virtual (= 1)\n\n");
        assert!(remove(&status, "amd64", &["providerpkg".into()], false).is_err());
        fs::remove_file(status).unwrap();
    }
}

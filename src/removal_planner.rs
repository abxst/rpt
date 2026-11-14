//! Read-only removal and autoremove transaction planning from installed dpkg state.
//! This is a conservative preview, not an authorization to invoke dpkg.
use crate::{cache_query::{self, InstalledKey}, install_plan::PlanStep, metadata_query::PackageSummary, package_index, version};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use std::{cmp::Ordering, collections::{BTreeMap, BTreeSet, VecDeque}, fs, path::Path};

#[derive(Debug, Clone)]
struct Requirement {
    name: String,
    arch: Option<String>,
    relation: Option<(String, String)>,
}

#[derive(Clone, Debug)]
struct InstalledPackage {
    version: String,
    multi_arch: String,
    depends: Vec<Vec<Requirement>>,
    recommends: Vec<Vec<Requirement>>,
    provides: Vec<Requirement>,
    conflicts: Vec<Vec<Requirement>>,
    breaks: Vec<Vec<Requirement>>,
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
            conflicts: parse_groups(record.field("Conflicts").unwrap_or(""))?,
            breaks: parse_groups(record.field("Breaks").unwrap_or(""))?,
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
            provides: value.provides.clone(),
            conflicts: Vec::new(), breaks: Vec::new(),
            protected: value.protected,
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

/// Identify already installed binaries that conflict with the signed,
/// selected upgrade candidates. Only forward Conflicts/Breaks from a new
/// candidate and reverse Conflicts/Breaks from installed status are evidence
/// for an automatic removal proposal. Never derive removals from unverified
/// repository versions or from a package's mere absence in the index.
///
/// Caller MUST independently verify candidate versions under APT policy.
pub(crate) fn signed_upgrade_removal_blockers(
    status: &[u8], native: &str, candidates: &[PackageSummary],
) -> Result<Vec<InstalledKey>> {
    checked_arch(native)?;
    let installed = read_installed_bytes(status)?;
    ensure_initial_dependencies(&installed, native)?;
    let mut blockers = BTreeSet::new();
    let mut staged_identities = BTreeSet::new();
    for item in candidates {
        checked_name(&item.name)?;
        checked_arch(&item.architecture)?;
        if item.architecture != native && item.architecture != "all" {
            bail!("cannot automatically remove blockers for foreign candidate");
        }
        let key = InstalledKey {
            name: item.name.clone(), architecture: item.architecture.clone(),
        };
        if !staged_identities.insert(key.clone()) {
            bail!("duplicate signed upgrade candidate");
        }
        let old = installed.get(&key).with_context(||
            format!("automatic removal requires an installed upgrade target: {}",
                key_label(&key)))?;
        if old.protected {
            bail!("automatic upgrade would change held, Essential or Protected {}",
                key_label(&key));
        }
        if version::compare(&item.version, &old.version)? != Ordering::Greater {
            bail!("signed candidate for {} is not an upgrade", key_label(&key));
        }
        let signed = staged_status(item, false)?;
        for group in signed.conflicts.iter().chain(signed.breaks.iter()) {
            for found in satisfiers(&installed, group, &key.architecture, native)? {
                if found != key { blockers.insert(found); }
            }
        }
        let singleton = BTreeMap::from([(key.clone(), signed)]);
        for (other_key, other) in &installed {
            if other_key == &key { continue; }
            for group in other.conflicts.iter().chain(other.breaks.iter()) {
                if satisfiers(&singleton, group, &other_key.architecture, native)?
                    .contains(&key) {
                    blockers.insert(other_key.clone());
                }
            }
        }
    }
    // An updated binary is not a removable old blocker; its new version
    // must be considered by the joint solver. If the no-removal solve is
    // still inconsistent, the scenario search will refuse this case.
    blockers.retain(|key| !staged_identities.contains(key));
    if blockers.len() > 8 {
        bail!("automatic full-upgrade found more than eight signed conflict blockers; manual review required");
    }
    for key in &blockers {
        if installed.get(key).is_some_and(|entry| entry.protected) {
            bail!("automatic full-upgrade conflicts with held, Essential or Protected {}", key_label(key));
        }
    }
    Ok(blockers.into_iter().collect())
}

/// A strictly checked, read-only projection of dpkg status after removing
/// an explicit set of complete installed binary identities. This is used
/// only to ask the existing signed policy solver which new candidate versions
/// can be considered after a *hypothetical* removal. It does not change dpkg.
pub fn project_status_without(
    status: &[u8], native: &str, requested: &[String],
) -> Result<(Vec<u8>, Vec<InstalledKey>)> {
    checked_arch(native)?;
    if requested.is_empty() || requested.len() > 32 {
        bail!("explicit full-upgrade removal scenario requires 1..=32 binaries");
    }
    let installed = read_installed_bytes(status)?;
    ensure_initial_dependencies(&installed, native)?;
    let mut removed = BTreeSet::new();
    for request in requested {
        let requirement = parse_requirement(request)?;
        if requirement.relation.is_some()
            || requirement.arch.as_deref() == Some("native") {
            bail!("removal must name an exact installed binary, optionally :architecture");
        }
        let matches = installed.keys().filter(|key|
            key.name == requirement.name && match requirement.arch.as_deref() {
                Some(arch) => key.architecture == arch,
                None => key.architecture == native || key.architecture == "all",
            }).cloned().collect::<Vec<_>>();
        if matches.len() != 1 {
            bail!("ambiguous or missing removal target: {request}");
        }
        let key = matches[0].clone();
        if installed[&key].protected {
            bail!("cannot remove held, Essential or Protected {}", key_label(&key));
        }
        if !removed.insert(key) {
            bail!("duplicate removal identity: {request}");
        }
    }
    // Parse and reserialize the *complete* source control document. Using a
    // line-filter here would silently corrupt multiline RFC822 fields.
    let raw = std::str::from_utf8(status)?;
    let stanzas = package_index::parse(raw)?;
    let mut result = String::new();
    let mut discarded = BTreeSet::new();
    for stanza in stanzas {
        let identity = match (stanza.field("Package"), stanza.field("Architecture")) {
            (Some(name), Some(arch)) => Some(InstalledKey {
                name: name.into(), architecture: arch.into(),
            }),
            _ => None,
        };
        if let Some(key) = identity {
            if removed.contains(&key) && stanza.field("Status").is_some_and(cache_query::is_installed_status) {
                if !discarded.insert(key.clone()) {
                    bail!("duplicate installed status for {}", key_label(&key));
                }
                continue;
            }
        }
        for (field, value) in stanza.fields {
            let mut lines = value.split('\n');
            result.push_str(&field);
            result.push_str(": ");
            result.push_str(lines.next().unwrap_or(""));
            result.push('\n');
            for continuation in lines {
                result.push(' ');
                result.push_str(continuation);
                result.push('\n');
            }
        }
        result.push('\n');
    }
    if discarded != removed {
        bail!("cannot project all requested removals from installed dpkg state");
    }
    let projected = result.into_bytes();
    let survivor = read_installed_bytes(&projected)?;
    if survivor.len() + removed.len() != installed.len() {
        bail!("hypothetical removal changed unrelated installed identities");
    }
    ensure_initial_dependencies(&survivor, native)
        .context("hypothetical removals already break installed dependencies before upgrade; refuse this scenario")?;
    Ok((projected, removed.into_iter().collect()))
}

/// One global simulated dpkg state after a signed no-removals upgrade.
/// A pass is evidence of consistency only in this conservative dependency
/// subset; it does NOT authorize executing any package on the live host.
#[derive(Debug, Clone, Serialize)]
pub struct UpgradeTransitionAudit {
    pub installed_before: usize,
    pub installed_after: usize,
    pub staged_binaries: usize,
    pub removed_binaries: usize,
    pub retained_dependents_checked: usize,
    pub dependency_groups_checked: usize,
    pub negative_relations_checked: usize,
    pub executable: bool,
}

fn staged_status(item: &PackageSummary, protected: bool) -> Result<InstalledPackage> {
    if !matches!(item.multi_arch.as_str(), "no" | "same" | "foreign" | "allowed") {
        bail!("invalid staged Multi-Arch value");
    }
    Ok(InstalledPackage {
        version: item.version.clone(),
        multi_arch: item.multi_arch.clone(),
        depends: parse_groups(&item.depends)?.into_iter()
            .chain(parse_groups(&item.pre_depends)?).collect(),
        recommends: parse_groups(&item.recommends)?,
        provides: parse_provides(&item.provides)?,
        conflicts: parse_groups(&item.conflicts)?,
        breaks: parse_groups(&item.breaks)?,
        protected,
    })
}

/// Check the *entire* installed state after replacing the exact staged
/// identities with authenticated candidate metadata. In particular, packages
/// absent from the solver's explicit root closure can be reverse dependents
/// of a new version or an old virtual Provides that disappears on upgrade.
///
/// Installed metadata is authoritative for every unchanged package. Staged
/// metadata must have been independently selected/verified from signed cache
/// and must exactly match every PlanStep identity/version.
pub fn audit_upgrade_transition(
    status: &[u8], native: &str, steps: &[PlanStep],
    signed: &BTreeMap<InstalledKey, PackageSummary>,
) -> Result<UpgradeTransitionAudit> {
    audit_transition_with_removals(status, native, steps, signed, &[])
}

/// Bound a combined installation/upgrade/removal plan to the *original*
/// installed status (not merely the hypothetical no-removal solver input).
/// This is intentionally a read-only global consistency oracle; no removal
/// or upgrade is executable from its result.
pub fn audit_transition_with_removals(
    status: &[u8], native: &str, steps: &[PlanStep],
    signed: &BTreeMap<InstalledKey, PackageSummary>, removals: &[InstalledKey],
) -> Result<UpgradeTransitionAudit> {
    checked_arch(native)?;
    let original = read_installed_bytes(status)?;
    ensure_initial_dependencies(&original, native)?;
    let mut projected = original.clone();
    let mut removed_keys = BTreeSet::new();
    for key in removals {
        if !removed_keys.insert(key.clone()) {
            bail!("duplicate removal identity: {}", key_label(key));
        }
        let previous = original.get(key).with_context(||
            format!("removal is not installed: {}", key_label(key)))?;
        if previous.protected {
            bail!("cannot remove held, Essential or Protected {}", key_label(key));
        }
        projected.remove(key);
    }
    let mut staged_keys = BTreeSet::new();
    for step in steps {
        let key = InstalledKey {
            name: step.package.clone(), architecture: step.architecture.clone(),
        };
        checked_name(&key.name)?;
        checked_arch(&key.architecture)?;
        if !staged_keys.insert(key.clone()) {
            bail!("duplicate staged package identity {}", key_label(&key));
        }
        if removed_keys.contains(&key) {
            bail!("removal overlaps with staged installation {}", key_label(&key));
        }
        let exact = signed.get(&key).with_context(||
            format!("missing independently authenticated staged candidate {}", key_label(&key)))?;
        if exact.name != key.name || exact.architecture != key.architecture
            || exact.version != step.version {
            bail!("signed staged candidate identity/version mismatch for {}", key_label(&key));
        }
        let existing = original.get(&key);
        if step.installed.as_deref() != existing.map(|p| p.version.as_str()) {
            bail!("stale staged installed version for {}", key_label(&key));
        }
        if existing.is_some_and(|entry| entry.protected) {
            bail!("upgrade would alter held, Essential or Protected installed {}", key_label(&key));
        }
        projected.insert(key, staged_status(exact, false)?);
    }
    // The no-removals transaction cannot silently replace a native package
    // with the Architecture: all identity (or vice versa).
    for key in projected.keys() {
        if key.architecture != "all" && projected.contains_key(&InstalledKey {
            name: key.name.clone(), architecture: "all".into(),
        }) {
            bail!("post-upgrade native/all architecture identity collision for {}", key.name);
        }
    }

    let mut dependency_groups_checked = 0;
    let mut retained_dependents_checked = 0;
    for (key, pkg) in &projected {
        if !staged_keys.contains(key) { retained_dependents_checked += 1; }
        for group in &pkg.depends {
            dependency_groups_checked += 1;
            if satisfiers(&projected, group, &key.architecture, native)?.is_empty() {
                bail!("upgrade would break installed reverse dependency of {}",
                    key_label(key));
            }
        }
    }

    // Check all negative relationships involving a staged package, not just
    // the installed packages that appear in the requested dependency closure.
    // Old negative relationships *between two unchanged binaries* are outside
    // this transition and are intentionally not treated as a new conflict.
    let mut negative_relations_checked = 0;
    for (key, pkg) in &projected {
        for group in pkg.conflicts.iter().chain(pkg.breaks.iter()) {
            negative_relations_checked += 1;
            for matching in satisfiers(&projected, group, &key.architecture, native)? {
                if matching != *key
                    && (staged_keys.contains(key) || staged_keys.contains(&matching)) {
                    bail!("upgrade creates Conflicts/Breaks between {} and {}",
                        key_label(key), key_label(&matching));
                }
            }
        }
    }

    Ok(UpgradeTransitionAudit {
        installed_before: original.len(),
        installed_after: projected.len(),
        staged_binaries: staged_keys.len(),
        removed_binaries: removed_keys.len(),
        retained_dependents_checked,
        dependency_groups_checked,
        negative_relations_checked,
        executable: false,
    })
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
                    provides: p.provides.clone(),
                    conflicts: Vec::new(), breaks: Vec::new(),
                    protected: p.protected,
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
    fn signed_candidate(name: &str, version: &str, provides: &str) -> PackageSummary {
        PackageSummary {
            name: name.into(), version: version.into(),
            architecture: "amd64".into(), multi_arch: "no".into(),
            description: String::new(), section: String::new(),
            depends: String::new(), pre_depends: String::new(),
            recommends: String::new(), suggests: String::new(),
            provides: provides.into(), conflicts: String::new(),
            breaks: String::new(), replaces: String::new(),
        }
    }

    fn upgrade_step(name: &str, version: &str) -> PlanStep {
        PlanStep {
            package: name.into(), architecture: "amd64".into(),
            version: version.into(), installed: Some("1".into()),
        }
    }

    #[test]
    fn whole_installed_graph_detects_lost_virtual_provides_version() {
        let status = b"Package: consumer\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: virtual-api (<= 1)\n\n\
Package: service\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nProvides: virtual-api (= 1)\n\n";
        let id = InstalledKey {
            name: "service".into(), architecture: "amd64".into(),
        };
        let steps = [upgrade_step("service", "2")];
        let mut candidates = BTreeMap::from([
            (id.clone(), signed_candidate("service", "2", "virtual-api (= 2)")),
        ]);
        let broken = audit_upgrade_transition(status, "amd64", &steps, &candidates)
            .unwrap_err().to_string();
        assert!(broken.contains("consumer"), "{broken}");

        candidates.get_mut(&id).unwrap().provides = "virtual-api (= 1)".into();
        let safe = audit_upgrade_transition(status, "amd64", &steps, &candidates).unwrap();
        assert_eq!(safe.installed_before, 2);
        assert_eq!(safe.installed_after, 2);
        assert_eq!(safe.retained_dependents_checked, 1);
        assert!(!safe.executable);

        candidates.get_mut(&id).unwrap().provides.clear();
        assert!(audit_upgrade_transition(status, "amd64", &steps, &candidates).is_err());
    }

    #[test]
    fn alternative_installed_virtual_satisfier_keeps_reverse_dependencies() {
        let status = b"Package: consumer\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: service-cap (<= 1) | alternate-api (>= 2)\n\n\
Package: service\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nProvides: service-cap (= 1)\n\n\
Package: alternate\nArchitecture: amd64\nVersion: 2\nStatus: install ok installed\nProvides: alternate-api (= 2)\n\n";
        let id = InstalledKey {
            name: "service".into(), architecture: "amd64".into(),
        };
        let signed = BTreeMap::from([(
            id, signed_candidate("service", "2", "service-cap (= 2)"),
        )]);
        assert!(audit_upgrade_transition(status, "amd64",
            &[upgrade_step("service", "2")], &signed).is_ok());
    }

    #[test]
    fn staged_upgrade_negative_relation_and_hold_fail_closed() {
        let status = b"Package: consumer\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nConflicts: service (>= 2)\n\n\
Package: service\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n";
        let key = InstalledKey {
            name: "service".into(), architecture: "amd64".into(),
        };
        let steps = [upgrade_step("service", "2")];
        let mut signed = BTreeMap::from([(
            key.clone(), signed_candidate("service", "2", ""),
        )]);
        assert!(audit_upgrade_transition(status, "amd64", &steps, &signed)
            .unwrap_err().to_string().contains("Conflicts/Breaks"));
        // Signed provider metadata must match actual binary identity/version.
        signed.get_mut(&key).unwrap().version = "3".into();
        assert!(audit_upgrade_transition(status, "amd64", &steps, &signed).is_err());

        let held = b"Package: service\nArchitecture: amd64\nVersion: 1\nStatus: hold ok installed\n\n";
        signed.get_mut(&key).unwrap().version = "2".into();
        assert!(audit_upgrade_transition(held, "amd64", &steps, &signed)
            .unwrap_err().to_string().contains("held"));
    }

    #[test]
    fn newly_staged_dependency_without_proof_is_rejected() {
        let status = b"Package: service\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n";
        let id = InstalledKey {
            name: "service".into(), architecture: "amd64".into(),
        };
        let mut candidate = signed_candidate("service", "2", "");
        candidate.depends = "absent-helper (>= 1)".into();
        let signed = BTreeMap::from([(id, candidate)]);
        assert!(audit_upgrade_transition(status, "amd64",
            &[upgrade_step("service", "2")], &signed)
            .unwrap_err().to_string().contains("reverse dependency"));
    }

    #[test]
    fn removal_projection_preserves_multiline_records_and_unrelated_state() {
        let status = b"Package: oldpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDescription: old package\n .\n second paragraph\n\n\
Package: keeppkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDescription: remaining package\n .\n retained paragraph\n\n\
Package: oldconfig\nArchitecture: all\nVersion: 0\nStatus: deinstall ok config-files\nDescription: kept config file\n\n";
        let (bytes, removed) = project_status_without(status, "amd64", &["oldpkg".into()]).unwrap();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].name, "oldpkg");
        let parsed = package_index::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].field("Package"), Some("keeppkg"));
        assert_eq!(parsed[0].field("Description"),
            Some("remaining package\n.\nretained paragraph"));
        assert_eq!(parsed[1].field("Status"), Some("deinstall ok config-files"));
        assert!(project_status_without(status, "amd64",
            &["oldpkg".into(), "oldpkg:amd64".into()]).is_err());
        assert!(project_status_without(status, "amd64", &["oldconfig".into()]).is_err());
        assert!(project_status_without(status, "amd64", &["oldpkg:i386".into()]).is_err());
    }

    #[test]
    fn hypothetical_removal_rejects_protected_and_preexisting_reverse_breakage() {
        let status = b"Package: consumer\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: oldpkg\n\n\
Package: oldpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n";
        let error = project_status_without(status, "amd64", &["oldpkg".into()])
            .unwrap_err().to_string();
        assert!(error.contains("hypothetical removals"), "{error}");
        let held = b"Package: oldpkg\nArchitecture: amd64\nVersion: 1\nStatus: hold ok installed\n\n";
        assert!(project_status_without(held, "amd64", &["oldpkg".into()]).is_err());
        let essential = b"Package: oldpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nEssential: yes\n\n";
        assert!(project_status_without(essential, "amd64", &["oldpkg".into()]).is_err());
    }

    #[test]
    fn combined_transition_removes_old_conflict_and_checks_original_state() {
        let status = b"Package: oldpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n\
Package: service\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n";
        let mut new = signed_candidate("service", "2", "");
        new.conflicts = "oldpkg".into();
        let trusted = BTreeMap::from([(InstalledKey {
            name: "service".into(), architecture: "amd64".into(),
        }, new)]);
        let steps = [upgrade_step("service", "2")];
        let old = InstalledKey {
            name: "oldpkg".into(), architecture: "amd64".into(),
        };
        assert!(audit_transition_with_removals(status, "amd64",
            &steps, &trusted, &[]).is_err());
        let ok = audit_transition_with_removals(status, "amd64",
            &steps, &trusted, std::slice::from_ref(&old)).unwrap();
        assert_eq!(ok.removed_binaries, 1);
        assert_eq!(ok.installed_before, 2);
        assert_eq!(ok.installed_after, 1);
        assert!(audit_transition_with_removals(status, "amd64",
            &steps, &trusted, &[old.clone(), old.clone()]).is_err());
        assert!(audit_transition_with_removals(status, "amd64",
            &steps, &trusted, &[InstalledKey {
                name: "missing".into(), architecture: "amd64".into(),
            }]).is_err());
        assert!(audit_transition_with_removals(status, "amd64",
            &steps, &trusted, &[InstalledKey {
                name: "service".into(), architecture: "amd64".into(),
            }]).is_err());
    }

    #[test]
    fn combined_transition_rejects_broken_retained_dependency_after_removal() {
        let status = b"Package: consumer\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\nDepends: oldpkg\n\n\
Package: oldpkg\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n\
Package: service\nArchitecture: amd64\nVersion: 1\nStatus: install ok installed\n\n";
        let old = InstalledKey { name: "oldpkg".into(), architecture: "amd64".into() };
        assert!(audit_transition_with_removals(status, "amd64", &[],
            &BTreeMap::new(), &[old]).unwrap_err().to_string().contains("consumer"));
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

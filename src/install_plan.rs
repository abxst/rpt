//! Fail-closed dependency planning preview. NEVER executes dpkg or mutates packages.
//! This is not APT's full dependency resolver: only uniquely resolvable
//! alternatives and virtual Provides, plus bounded alternative backtracking;
//! no architecture qualifiers, pinning, package holds, triggers or removals.
use crate::{cache_query, metadata_query::{self, PackageKey, PackageSummary}, version};
use anyhow::{bail, Context, Result};
use std::{cell::Cell, cmp::Ordering, collections::{BTreeMap, BTreeSet}, path::Path, rc::Rc};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PlanStep {
    pub package: String,
    pub architecture: String,
    pub version: String,
    pub installed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Requirement {
    name: String,
    constraint: Option<(String, String)>,
    any_arch: bool,
    requested_arch: Option<String>,
}

fn parse_atom(text: &str) -> Result<Requirement> {
    let atom = text.trim();
    if atom.is_empty() {
        bail!("unsupported dependency syntax: {atom}");
    }
    // Explicitly reject architecture qualifiers and build profile syntax.
    if atom.contains(['[', ']', '!']) || (atom.contains(['<', '>']) && !atom.contains('(')) {
        bail!("unsupported architecture/profile dependency: {atom}");
    }
    let (name, constraint) = if let Some((name, remainder)) = atom.split_once('(') {
        if !remainder.ends_with(')') { bail!("unterminated dependency constraint: {atom}"); }
        let fields = remainder[..remainder.len()-1].split_whitespace().collect::<Vec<_>>();
        if fields.len() != 2 || !matches!(fields[0], "<<" | "<=" | "=" | ">=" | ">>") {
            bail!("unsupported dependency constraint: {atom}");
        }
        (name.trim(), Some((fields[0].to_owned(), fields[1].to_owned())))
    } else { (atom, None) };
    if name.len() < 2 || !name.as_bytes()[0].is_ascii_lowercase() && !name.as_bytes()[0].is_ascii_digit()
        || !name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'+' | b'.' | b'-')) {
        bail!("unsupported dependency name: {name}");
    }
    if let Some((_, required)) = &constraint { version::compare(required, required)?; }
    Ok(Requirement { name: name.to_owned(), constraint, any_arch: false, requested_arch: None })
}

/// Support only native-architecture dependency qualifiers. Foreign-arch
/// package co-installation, :any and architecture restrictions require true
/// Multi-Arch metadata and are deliberately refused.
fn parse_atom_for_arch(text: &str, arch: &str) -> Result<Requirement> {
    let atom = text.trim();
    let (head, tail) = atom.split_once('(').map_or((atom, None), |(h, t)| (h, Some(t)));
    let head = head.trim();
    let normalized = if let Some((name, qualifier)) = head.split_once(':') {
        if qualifier != "native" && qualifier != "any"
            && (qualifier.is_empty()
                || !qualifier.as_bytes()[0].is_ascii_lowercase() && !qualifier.as_bytes()[0].is_ascii_digit()
                || !qualifier.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')) {
            bail!("invalid architecture qualifier: {qualifier}");
        }
        if name.is_empty() || qualifier.contains(':') {
            bail!("invalid architecture-qualified dependency");
        }
        name
    } else { head };
    let rebuilt = match tail {
        Some(rest) => format!("{normalized} ({rest}"),
        None => normalized.to_owned(),
    };
    let mut requirement = parse_atom(&rebuilt)?;
    requirement.any_arch = head.ends_with(":any");
    requirement.requested_arch = head.split_once(':').and_then(|(_, qualifier)| {
        if qualifier == "any" { None }
        else if qualifier == "native" { Some(arch.to_owned()) }
        else { Some(qualifier.to_owned()) }
    });
    Ok(requirement)
}

/// Preflight a possible cross-architecture co-installation without mutating
/// package state. Debian permits same-name, different-architecture binaries
/// to coexist only when both declare Multi-Arch: same and have identical
/// versions. This check does not authorize installing a foreign package:
/// the planner still requires a complete foreign-architecture resolver.
fn validate_multiarch_same_pair(
    native: &PackageSummary,
    foreign: &PackageSummary,
) -> Result<()> {
    if native.name != foreign.name {
        bail!("Multi-Arch pair must refer to the same package name");
    }
    if native.architecture == foreign.architecture
        || native.architecture == "all" || foreign.architecture == "all" {
        bail!("Multi-Arch co-installation requires two distinct concrete architectures");
    }
    if native.multi_arch != "same" || foreign.multi_arch != "same" {
        bail!("cross-architecture co-installation requires Multi-Arch: same on both binaries");
    }
    if version::compare(&native.version, &foreign.version)? != Ordering::Equal {
        bail!("cross-architecture co-installation requires identical package versions");
    }
    Ok(())
}

/// Resolve a foreign co-installation partner by exact binary identity.
/// The foreign candidate must be present in the verified catalog and pass
/// Multi-Arch: same validation before a planner may consider it.
#[allow(dead_code)] // Foreign package installation remains fail-closed until solver integration.
fn validated_foreign_partner<'a>(
    catalog: &'a BTreeMap<PackageKey, PackageSummary>,
    native: &PackageSummary,
    foreign_arch: &str,
) -> Result<&'a PackageSummary> {
    let key = PackageKey {
        name: native.name.clone(),
        architecture: foreign_arch.to_owned(),
    };
    let foreign = catalog.get(&key).with_context(|| format!(
        "missing foreign candidate {}:{foreign_arch}", native.name
    ))?;
    validate_multiarch_same_pair(native, foreign)?;
    Ok(foreign)
}

/// Validate every pair in a proposed multi-architecture install set.
/// This deliberately does not infer removals, version synchronization, or
/// foreign candidates. Callers must supply the exact verified candidate
/// metadata for each planned binary identity.
fn validate_multiarch_plan(
    catalog: &BTreeMap<PackageKey, PackageSummary>,
    steps: &[PlanStep],
) -> Result<()> {
    let mut staged = BTreeMap::<PackageKey, &PackageSummary>::new();
    for step in steps {
        let key = PackageKey {
            name: step.package.clone(),
            architecture: step.architecture.clone(),
        };
        let candidate = catalog.get(&key).with_context(|| format!(
            "missing verified candidate {}:{}", step.package, step.architecture
        ))?;
        if candidate.name != key.name || candidate.architecture != key.architecture
            || candidate.version != step.version {
            bail!("inconsistent verified candidate identity or version for {}:{}", key.name, key.architecture);
        }
        // Unknown Multi-Arch declarations cannot be treated as 'no':
        // doing so could silently change dependency and co-install semantics.
        if !matches!(candidate.multi_arch.as_str(), "no" | "same" | "foreign" | "allowed") {
            bail!("unsupported Multi-Arch declaration for {}:{}", step.package, step.architecture);
        }
        if staged.insert(key, candidate).is_some() {
            bail!("duplicate staged binary identity {}:{}", step.package, step.architecture);
        }
    }
    // No package with Architecture: all may share its binary name with a
    // concrete architecture in the same transaction. Such a projection
    // would silently alias two distinct dpkg identities.
    for left_key in staged.keys() {
        for right_key in staged.keys() {
            if left_key >= right_key || left_key.name != right_key.name { continue; }
            if left_key.architecture == "all" || right_key.architecture == "all" {
                bail!("Architecture: all cannot be co-staged with a concrete binary of the same name");
            }
        }
    }
    for (left_key, left) in &staged {
        for (right_key, right) in &staged {
            if left_key >= right_key || left_key.name != right_key.name { continue; }
            validate_multiarch_same_pair(left, right)?;
        }
    }
    Ok(())
}

fn parse_groups_for_arch(raw: &str, arch: &str) -> Result<Vec<Vec<Requirement>>> {
    if raw.trim().is_empty() { return Ok(Vec::new()); }
    raw.split(',').map(|group| {
        group.split('|').map(|atom| parse_atom_for_arch(atom, arch))
            .collect::<Result<Vec<_>>>()
    }).collect()
}

fn satisfies(version_string: &str, required: &Requirement) -> Result<bool> {
    let Some((op, value)) = &required.constraint else { return Ok(true) };
    let cmp = version::compare(version_string, value)?;
    Ok(match op.as_str() {
        "<<" => cmp == Ordering::Less,
        "<=" => cmp != Ordering::Greater,
        "=" => cmp == Ordering::Equal,
        ">=" => cmp != Ordering::Less,
        ">>" => cmp == Ordering::Greater,
        _ => false,
    })
}

/// A conservative, single-name provider list. Debian versioned virtual
/// Provides require an exact "=", not a range. Never use a provider's *own*
/// version as proof of a versioned virtual capability.
fn provides(summary: &PackageSummary) -> Result<Vec<Requirement>> {
    if summary.provides.trim().is_empty() { return Ok(Vec::new()); }
    summary.provides.split(',').map(|part| {
        if part.contains('|') { bail!("unsupported alternatives in Provides for {}", summary.name); }
        let cap = parse_atom(part)?;
        if cap.constraint.as_ref().is_some_and(|(op, _)| op != "=") {
            bail!("unsupported version relation in Provides for {}", summary.name);
        }
        Ok(cap)
    }).collect()
}

fn capability_satisfies(cap: &Requirement, required: &Requirement) -> Result<bool> {
    if cap.name != required.name { return Ok(false); }
    if required.constraint.is_none() { return Ok(true); }
    // Unversioned Provides must never satisfy a versioned dependency.
    let Some((_, version)) = &cap.constraint else { return Ok(false) };
    satisfies(version, required)
}

#[derive(Clone, Debug, Default)]
struct InstalledRelations {
    version: String,
    multi_arch: String,
    conflicts: String,
    breaks: String,
    replaces: String,
    provides: String,
    held: bool,
    essential: bool,
    protected: bool,
}

/// Reject malformed critical flags instead of treating them as false.
fn parse_critical_flag(field: Option<&str>, key: &str) -> Result<bool> {
    match field {
        None | Some("no") => Ok(false),
        Some("yes") => Ok(true),
        Some(_) => bail!("invalid {key} flag in installed dpkg status"),
    }
}

/// An experimental plan does not support force-hold/force-essential overrides.
fn ensure_unchanged_protected(
    name: &str, current: &str, proposed: &str, relation: &InstalledRelations,
) -> Result<()> {
    if relation.version != current {
        bail!("inconsistent installed version metadata for {name}");
    }
    if current != proposed {
        if relation.held { bail!("refusing to change held package {name}"); }
        if relation.essential { bail!("refusing to change Essential package {name}"); }
        if relation.protected { bail!("refusing to change Protected package {name}"); }
    }
    Ok(())
}

/// Read conflict metadata from the *installed* dpkg status, never infer it
/// from the latest repository version. Dpkg status is local, not signed policy.
fn read_installed_relations_by_arch(status: &Path) -> Result<BTreeMap<cache_query::InstalledKey, InstalledRelations>> {
    let data = std::fs::read(status)?;
    if data.len() > 64 * 1024 * 1024 { bail!("dpkg status file exceeds safe size"); }
    let text = std::str::from_utf8(&data)?;
    let mut result = BTreeMap::new();
    for record in crate::package_index::parse(text)? {
        let state = record.field("Status").context("dpkg stanza missing Status")?;
        if !crate::cache_query::is_installed_status(state) { continue; }
        let arch = record.field("Architecture").context("installed status missing Architecture")?;
        let name = record.field("Package").context("installed status missing Package")?;
        let version = record.field("Version").context("installed status missing Version")?;
        parse_atom(name)?;
        version::compare(version, version)?;
        let entry = InstalledRelations {
            version: version.into(),
            multi_arch: record.field("Multi-Arch").unwrap_or("no").into(),
            conflicts: record.field("Conflicts").unwrap_or("").into(),
            breaks: record.field("Breaks").unwrap_or("").into(),
            replaces: record.field("Replaces").unwrap_or("").into(),
            provides: record.field("Provides").unwrap_or("").into(),
            held: state == "hold ok installed",
            essential: parse_critical_flag(record.field("Essential"), "Essential")?,
            protected: parse_critical_flag(record.field("Protected"), "Protected")?,
        };
        if arch.is_empty() || !arch.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
            bail!("invalid dpkg installed architecture for {name}");
        }
        let key = cache_query::InstalledKey { name: name.to_owned(), architecture: arch.to_owned() };
        if result.insert(key, entry).is_some() {
            bail!("duplicate installed package identity in dpkg status: {name}:{arch}");
        }
    }
    Ok(result)
}

/// Restrict the architecture-aware installed records to the current solver's
/// native + 'all' scope. Reject collisions rather than overwrite metadata.
#[cfg(test)]
fn native_installed_relations(
    keyed: &BTreeMap<cache_query::InstalledKey, InstalledRelations>,
    architecture: &str,
) -> Result<BTreeMap<String, InstalledRelations>> {
    let mut projected = BTreeMap::new();
    for (key, value) in keyed {
        if key.architecture != architecture && key.architecture != "all" { continue; }
        if projected.insert(key.name.clone(), value.clone()).is_some() {
            bail!("ambiguous installed relation identity for {}; native/all collision", key.name);
        }
    }
    Ok(projected)
}

#[cfg(test)]
fn read_installed_relations(status: &Path, arch: &str) -> Result<BTreeMap<String, InstalledRelations>> {
    native_installed_relations(&read_installed_relations_by_arch(status)?, arch)
}

/// Check that all installed binary identities have exactly matching dpkg
/// safety metadata. Foreign identities are retained, never overwritten.
fn validate_installed_metadata(
    installed: &BTreeMap<cache_query::InstalledKey, cache_query::Installed>,
    relations: &BTreeMap<cache_query::InstalledKey, InstalledRelations>,
) -> Result<()> {
    for (key, item) in installed {
        let identity = key.clone();
        let relation = relations.get(&identity).with_context(|| format!(
            "missing installed safety metadata for {}:{}", key.name, key.architecture
        ))?;
        if relation.version != item.version {
            bail!("inconsistent installed version for {}:{}", key.name, key.architecture);
        }
        if !matches!(relation.multi_arch.as_str(), "no" | "same" | "foreign" | "allowed") {
            bail!("unsupported installed Multi-Arch declaration for {}:{}", key.name, key.architecture);
        }
    }
    for key in relations.keys() {
        if !installed.contains_key(key) {
            bail!("installed dpkg safety metadata has unknown binary identity");
        }
    }
    // Validate existing co-installed binaries before planning any mutation.
    // A corrupt dpkg state must never be used as the basis for a new plan.
    for (left_key, left) in relations {
        for (right_key, right) in relations {
            if left_key >= right_key || left_key.name != right_key.name { continue; }
            if left_key.architecture == "all" || right_key.architecture == "all"
                || left.multi_arch != "same" || right.multi_arch != "same"
                || version::compare(&left.version, &right.version)? != Ordering::Equal {
                bail!("inconsistent installed Multi-Arch co-installation for {}", left_key.name);
            }
        }
    }
    Ok(())
}

/// A negative relationship must be a simple comma-separated set of names
/// with optional version constraints. Alternatives, arch qualifiers and
/// profiles require a fuller dpkg/APT resolver and therefore fail closed.
fn negative_relations(raw: &str) -> Result<Vec<Requirement>> {
    if raw.trim().is_empty() { return Ok(Vec::new()); }
    if raw.contains('|') { bail!("unsupported alternative in Conflicts/Breaks"); }
    raw.split(',').map(parse_atom).collect()
}

fn relations_hit(raw: &str, subject_name: &str, subject_version: &str,
    provided: &str) -> Result<bool> {
    for requirement in negative_relations(raw)? {
        if requirement.name == subject_name && satisfies(subject_version, &requirement)? {
            return Ok(true);
        }
        let advertised = PackageSummary {
            name: subject_name.into(), version: subject_version.into(),
            architecture: String::new(), multi_arch: "no".into(), description: String::new(),
            section: String::new(), depends: String::new(),
            pre_depends: String::new(), recommends: String::new(),
            suggests: String::new(), provides: provided.into(),
            conflicts: String::new(), breaks: String::new(), replaces: String::new(),
        };
        for cap in provides(&advertised)? {
            if capability_satisfies(&cap, &requirement)? { return Ok(true); }
        }
    }
    Ok(false)
}

/// Replaces may legally transfer file ownership. Until the transaction
/// engine validates dpkg's ownership database, refuse plans that would
/// overlap an installed or simultaneously staged different binary package.
fn replaces_hit(raw: &str, name: &str, version: &str) -> Result<bool> {
    for requirement in negative_relations(raw)? {
        if requirement.name == name && satisfies(version, &requirement)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Projection for the current native-only solver. The map remains keyed by
/// exact binary identity; foreign entries are kept for the outer guard.
fn scoped_installed(
    complete: &BTreeMap<cache_query::InstalledKey, cache_query::Installed>,
    architecture: &str,
) -> Result<BTreeMap<PackageKey, String>> {
    let mut scoped = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (identity, item) in complete {
        if identity.architecture != architecture && identity.architecture != "all" { continue; }
        if !seen.insert(identity.name.clone()) {
            bail!("ambiguous native/all installed binary {}", identity.name);
        }
        scoped.insert(PackageKey {
            name: identity.name.clone(), architecture: identity.architecture.clone(),
        }, item.version.clone());
    }
    Ok(scoped)
}

fn scoped_relations(
    complete: &BTreeMap<cache_query::InstalledKey, InstalledRelations>,
    architecture: &str,
) -> Result<BTreeMap<PackageKey, InstalledRelations>> {
    let mut scoped = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for (key, relation) in complete {
        if key.architecture != architecture && key.architecture != "all" { continue; }
        if !seen.insert(key.name.clone()) {
            bail!("ambiguous native/all installed relation {}", key.name);
        }
        scoped.insert(PackageKey { name: key.name.clone(), architecture: key.architecture.clone() }, relation.clone());
    }
    Ok(scoped)
}

#[derive(Clone)]
struct Solver<'a> {
    catalog: &'a BTreeMap<PackageKey, PackageSummary>,
    installed: &'a BTreeMap<PackageKey, String>,
    installed_relations: &'a BTreeMap<PackageKey, InstalledRelations>,
    visiting: BTreeSet<PackageKey>,
    done: BTreeSet<PackageKey>,
    plan: Vec<PlanStep>,
    limit: usize,
    architecture: String,
    branch_budget: Rc<Cell<usize>>,
    include_recommends: bool,
    /// Explicit concrete roots required by this entire batch. These are
    /// constraints even before the solver reaches a later root. A dependency
    /// alternative must not defeat a root the user explicitly requested.
    requested_roots: BTreeSet<PackageKey>,
}

impl Solver<'_> {
    /// Return the concrete package to install only when there is exactly one
    /// verified catalog candidate. No automatic preference between multiple
    /// virtual providers or mutually satisfiable alternatives.
    /// Resolve a concrete binary identity in the selected native scope.
    /// Foreign-architecture and same-name native/all ambiguity fail closed.
    fn key_for(&self, name: &str) -> Result<Option<PackageKey>> {
        let native = PackageKey { name: name.into(), architecture: self.architecture.clone() };
        let all = PackageKey { name: name.into(), architecture: "all".into() };
        match (self.catalog.contains_key(&native), self.catalog.contains_key(&all)) {
            (true, true) => bail!("ambiguous native/all binary identity for {name}"),
            (true, false) => Ok(Some(native)),
            (false, true) => Ok(Some(all)),
            (false, false) => Ok(None),
        }
    }

    /// Resolve installed binary identity independently of repository candidate
    /// metadata. This avoids conflating the installed native and all states.
    fn installed_for_name(&self, name: &str) -> Result<Option<(&PackageKey, &String)>> {
        let mut found = None;
        for (key, version) in self.installed {
            if key.name != name { continue; }
            if found.is_some() {
                bail!("ambiguous installed architectures for {name}");
            }
            found = Some((key, version));
        }
        Ok(found)
    }

    fn concrete(&self, required: &Requirement) -> Result<Option<PackageKey>> {
        if let Some(requested_arch) = &required.requested_arch {
            let explicit = PackageKey { name: required.name.clone(), architecture: requested_arch.clone() };
            if let Some(item) = self.catalog.get(&explicit) {
                if required.any_arch { bail!("conflicting architecture qualifiers"); }
                return Ok(if satisfies(&item.version, required)? { Some(explicit) } else { None });
            }
            if requested_arch != &self.architecture {
                return Ok(None); // Never silently substitute a native or all binary.
            }
        }
        if let Some(key) = self.key_for(&required.name)? {
            let real = &self.catalog[&key];
            if required.any_arch && real.multi_arch != "allowed" {
                return Ok(None);
            }
            return Ok(if satisfies(&real.version, required)? { Some(key) } else { None });
        }
        if required.any_arch {
            bail!("virtual Provides with :any require a full Multi-Arch resolver");
        }
        let mut candidates = Vec::new();
        for (key, item) in self.catalog {
            // A virtual provider must be installable in this dependency
            // architecture. A foreign binary must not satisfy a native
            // dependency merely because it advertises the same capability.
            if key.architecture != self.architecture && key.architecture != "all" {
                continue;
            }
            if required.any_arch { continue; }
            let mut matches = false;
            for cap in provides(item)? {
                if capability_satisfies(&cap, required)? {
                    matches = true;
                    break;
                }
            }
            if matches {
                candidates.push(key.clone());
                if candidates.len() > 1 { break; }
            }
        }
        if candidates.len() > 1 {
            bail!("ambiguous virtual package {}: multiple providers; full APT solver required", required.name);
        }
        Ok(candidates.pop())
    }

    fn resolve(&mut self, requested: &Requirement) -> Result<()> {
        let concrete = self.concrete(requested)?
            .with_context(|| format!("unresolved dependency {} (no matching real package or provider)", requested.name))?;
        if concrete.name == requested.name {
            self.resolve_concrete(&concrete, requested)
        } else {
            // The virtual capability constraint was checked against Provides
            // in concrete(); it is not a constraint on the provider's own version.
            let actual = Requirement {
                name: concrete.name.clone(), constraint: None,
                any_arch: false, requested_arch: Some(concrete.architecture.clone()),
            };
            self.resolve_concrete(&concrete, &actual)
        }
    }

    /// Debian alternatives are ordered: prefer the first viable binary,
    /// while retaining bounded backtracking for dependencies or conflicts
    /// that make an earlier choice impossible. Never leak failed branches.
    fn resolve_groups(&mut self, groups: &[Vec<Requirement>], index: usize) -> Result<()> {
        if index == groups.len() { return Ok(()); }
        let group = &groups[index];
        // An already installed satisfier wins before scheduling a new binary,
        // even when it is not the first textual alternative.
        for alternative in group {
            let selected_installed = if let Some(arch) = &alternative.requested_arch {
                let exact = PackageKey { name: alternative.name.clone(), architecture: arch.clone() };
                self.installed.get_key_value(&exact)
            } else {
                self.installed_for_name(&alternative.name)?
            };
            if let Some((installed_key, installed)) = selected_installed {
                let candidate_key = if alternative.requested_arch.is_some() {
                    Some(installed_key.clone())
                } else {
                    self.key_for(&alternative.name)?
                };
                if candidate_key.as_ref().is_some_and(|key| key != installed_key) {
                    bail!("installed and repository binary identities disagree for {}", alternative.name);
                }
                let allowed_installed = !alternative.any_arch
                    || self.key_for(&alternative.name)?.and_then(|key| self.catalog.get(&key))
                        .is_some_and(|item| item.multi_arch == "allowed" && item.version == *installed);
                if allowed_installed && satisfies(installed, alternative)? {
                    return self.resolve_groups(groups, index + 1);
                }
            }
        }

        // Preserve dependency expression order, not BTreeSet lexical order.
        // Deduplicate concrete providers without changing the first matching
        // requirement's version / architecture constraints.
        let mut seen = BTreeSet::new();
        let mut options = Vec::new();
        for requirement in group {
            if let Some(concrete) = self.concrete(requirement)? {
                if seen.insert(concrete.clone()) {
                    options.push((concrete, requirement));
                }
            }
        }
        if options.is_empty() {
            bail!("no resolvable alternatives for dependency group");
        }
        for (option, requirement) in options {
            let remaining = self.branch_budget.get();
            if remaining == 0 { bail!("dependency branch budget exhausted"); }
            self.branch_budget.set(remaining - 1);
            let mut branch = self.clone();
            // When a virtual capability maps to a unique actual provider,
            // keep the policy-verified provider identity; never weaken a
            // direct binary's requested version constraint.
            let actual = if requirement.name == option.name {
                requirement.clone()
            } else {
                Requirement {
                    name: option.name.clone(), constraint: None,
                    any_arch: false, requested_arch: Some(option.architecture.clone()),
                }
            };
            if branch.resolve_concrete(&option, &actual).is_ok()
                && branch.resolve_groups(groups, index + 1).is_ok() {
                self.visiting = branch.visiting;
                self.done = branch.done;
                self.plan = branch.plan;
                return Ok(());
            }
        }
        bail!("no viable dependency alternative after bounded ordered backtracking")
    }

    /// Validate the final effective set (installed packages overridden by
    /// staged upgrades plus all staged additions). Include reverse relations
    /// from installed dpkg status, rather than newest repository metadata.
    fn check_negative_relations(&self) -> Result<()> {
        // Exact binary identities are used for both planned and installed
        // versions. No name-only merge may override a foreign or all package.
        let staged: BTreeMap<PackageKey, &PlanStep> = self.plan.iter().map(|step| (
            PackageKey { name: step.package.clone(), architecture: step.architecture.clone() }, step
        )).collect();
        for (new_key, new_step) in &staged {
            let new_pkg = self.catalog.get(new_key)
                .context("staged package missing repository metadata")?;
            for (installed_key, installed_version) in self.installed {
                if installed_key == new_key { continue; }
                let effective_version = staged.get(installed_key)
                    .map(|item| item.version.as_str()).unwrap_or(installed_version);
                let (conflicts, breaks, replaces, provider) =
                    if let Some(pkg) = staged.get(installed_key)
                        .and_then(|_| self.catalog.get(installed_key)) {
                        (pkg.conflicts.as_str(), pkg.breaks.as_str(), pkg.replaces.as_str(), pkg.provides.as_str())
                    } else if let Some(status) = self.installed_relations.get(installed_key) {
                        if status.version != *installed_version {
                            bail!("inconsistent installed metadata for {}:{}",
                                installed_key.name, installed_key.architecture);
                        }
                        (status.conflicts.as_str(), status.breaks.as_str(), status.replaces.as_str(), status.provides.as_str())
                    } else {
                        // Only test fixtures may omit installed relationship
                        // records; real previews require exact agreement.
                        ("", "", "", "")
                    };
                if relations_hit(&new_pkg.conflicts, &installed_key.name, effective_version, provider)?
                    || relations_hit(&new_pkg.breaks, &installed_key.name, effective_version, provider)?
                    || relations_hit(conflicts, &new_key.name, &new_step.version, &new_pkg.provides)?
                    || relations_hit(breaks, &new_key.name, &new_step.version, &new_pkg.provides)? {
                    bail!("Conflicts/Breaks between planned {}:{} and installed {}:{}",
                        new_key.name, new_key.architecture, installed_key.name, installed_key.architecture);
                }
                if replaces_hit(&new_pkg.replaces, &installed_key.name, effective_version)?
                    || replaces_hit(replaces, &new_key.name, &new_step.version)? {
                    bail!("Replaces between planned {}:{} and installed {}:{} requires file ownership verification",
                        new_key.name, new_key.architecture, installed_key.name, installed_key.architecture);
                }
            }
            for (peer_key, peer_step) in &staged {
                if peer_key == new_key { continue; }
                let peer = self.catalog.get(peer_key)
                    .context("staged peer missing repository metadata")?;
                if relations_hit(&new_pkg.conflicts, &peer_key.name, &peer_step.version, &peer.provides)?
                    || relations_hit(&new_pkg.breaks, &peer_key.name, &peer_step.version, &peer.provides)?
                    || relations_hit(&peer.conflicts, &new_key.name, &new_step.version, &new_pkg.provides)?
                    || relations_hit(&peer.breaks, &new_key.name, &new_step.version, &new_pkg.provides)? {
                    bail!("Conflicts/Breaks between planned {}:{} and {}:{}",
                        new_key.name, new_key.architecture, peer_key.name, peer_key.architecture);
                }
                if replaces_hit(&new_pkg.replaces, &peer_key.name, &peer_step.version)?
                    || replaces_hit(&peer.replaces, &new_key.name, &new_step.version)? {
                    bail!("Replaces between planned {}:{} and {}:{} requires file ownership verification",
                        new_key.name, new_key.architecture, peer_key.name, peer_key.architecture);
                }
            }
        }
        Ok(())
    }

    /// Look ahead to every mandatory root in a joint install plan. APT can
    /// backtrack to another dependency alternative when the first choice
    /// conflicts with a later explicit root. Without this check the first
    /// root's dependency branch gets committed before the later root is
    /// visited, and the entire batch fails even when a safe option exists.
    ///
    /// This uses exact policy-selected candidate metadata, not a guess based
    /// on installed package names. Conflicts/Breaks and Replaces still fail
    /// closed; the engine never interprets this as permission to remove or
    /// replace a package or skip a root.
    fn check_requested_roots(&self, key: &PackageKey, candidate: &PackageSummary) -> Result<()> {
        for root_key in &self.requested_roots {
            if root_key == key { continue; }
            let root = self.catalog.get(root_key)
                .context("requested root lacks candidate metadata")?;
            let conflicts = relations_hit(&candidate.conflicts, &root.name, &root.version, &root.provides)?
                || relations_hit(&candidate.breaks, &root.name, &root.version, &root.provides)?
                || relations_hit(&root.conflicts, &candidate.name, &candidate.version, &candidate.provides)?
                || relations_hit(&root.breaks, &candidate.name, &candidate.version, &candidate.provides)?;
            if conflicts {
                bail!("Conflicts/Breaks between {} and required root {}",
                    candidate.name, root.name);
            }
            if replaces_hit(&candidate.replaces, &root.name, &root.version)?
                || replaces_hit(&root.replaces, &candidate.name, &candidate.version)? {
                bail!("Replaces between {} and required root {} needs file ownership validation",
                    candidate.name, root.name);
            }
        }
        Ok(())
    }

    fn resolve_concrete(&mut self, key: &PackageKey, requested: &Requirement) -> Result<()> {
        if self.done.contains(key) {
            let current = self.catalog.get(key)
                .context("already resolved package missing from catalog")?;
            if !satisfies(&current.version, requested)? {
                bail!("conflicting version requirements for {}", requested.name);
            }
            return Ok(());
        }
        if self.visiting.contains(key) {
            // Read-only package-set preview: a Depends cycle does not require
            // resolving the same binary twice. Never relax version constraints.
            let current = self.catalog.get(key)
                .context("visiting dependency missing verified metadata")?;
            if !satisfies(&current.version, requested)? {
                bail!("conflicting version requirements in dependency cycle for {}", requested.name);
            }
            return Ok(());
        }
        let current = self.catalog.get(key)
            .with_context(|| format!("unresolved dependency {} (virtual packages unsupported)", requested.name))?;
        if !satisfies(&current.version, requested)? {
            bail!("repository version {} does not satisfy dependency {}", current.version, requested.name);
        }
        // Reject branches incompatible with a later explicit batch root
        // *before* this branch can be committed by ordered backtracking.
        self.check_requested_roots(key, current)?;
        // Ancestors are not yet in self.plan while resolving their dependencies.
        // Reject incompatible branches here so alternative backtracking can
        // select a viable sibling rather than reporting false ambiguity.
        // Compare both directions; a visiting parent can itself forbid a child.
        for ancestor_key in &self.visiting {
            let ancestor = self.catalog.get(ancestor_key)
                .context("visiting package missing verified metadata")?;
            if relations_hit(&current.conflicts, &ancestor.name, &ancestor.version, &ancestor.provides)?
                || relations_hit(&current.breaks, &ancestor.name, &ancestor.version, &ancestor.provides)?
                || relations_hit(&ancestor.conflicts, &current.name, &current.version, &current.provides)?
                || relations_hit(&ancestor.breaks, &current.name, &current.version, &current.provides)? {
                bail!("Conflicts/Breaks between dependency {} and ancestor {}",
                    current.name, ancestor.name);
            }
        }
        // Fail closed on unsupported Conflicts/Breaks syntax even if the
        // named conflicting package is not presently installed.
        negative_relations(&current.conflicts)?;
        negative_relations(&current.breaks)?;
        negative_relations(&current.replaces)?;
        if self.plan.len() + self.visiting.len() >= self.limit {
            bail!("dependency plan exceeded maximum package count");
        }
        self.visiting.insert(key.clone());
        let dependency_arch = if current.architecture == "all" {
            self.architecture.clone()
        } else {
            current.architecture.clone()
        };
        let pre_groups = parse_groups_for_arch(&current.pre_depends, &dependency_arch)?;
        // A Pre-Depends back-edge cannot be safely represented by a simple
        // package set: configuring the target before unpacking the source
        // requires an execution-order model.
        for group in &pre_groups {
            for dependency in group {
                if let Some(target) = self.concrete(dependency)? {
                    if self.visiting.contains(&target) {
                        bail!("Pre-Depends cycle requires transaction ordering");
                    }
                }
            }
        }
        let groups = pre_groups.into_iter()
            .chain(parse_groups_for_arch(&current.depends, &dependency_arch)?).collect::<Vec<_>>();
        self.resolve_groups(&groups, 0)?;
        self.visiting.remove(key);
        self.done.insert(key.clone());
        let installed = self.installed.get(key).cloned();
        if self.installed.keys().any(|existing| {
            existing.name == key.name && existing != key
                && (existing.architecture == "all" || key.architecture == "all")
        }) {
            bail!("installed and repository binary identities disagree for {}", key.name);
        }
        if installed.as_deref() != Some(current.version.as_str()) {
            if let Some(existing) = &installed {
                if let Some(relation) = self.installed_relations.get(key) {
                    ensure_unchanged_protected(
                        &requested.name, existing, &current.version, relation,
                    )?;
                }
            }
            // Default priorities do not permit a downgrade.
            if let Some(existing) = &installed {
                if version::compare(existing, &current.version)? == Ordering::Greater {
                    bail!("would downgrade {}; full APT policy is required", current.name);
                }
            }
            self.plan.push(PlanStep {
                package: key.name.clone(), architecture: key.architecture.clone(),
                version: current.version.clone(), installed,
            });
            self.check_negative_relations()?;
            if self.include_recommends {
                // APT recommends are soft dependencies. Preserve every
                // mandatory change even if this suggestion cannot be
                // satisfied; a failed branch must not leak into the plan.
                // Parsing remains strict: unsupported metadata fails closed.
                let groups = parse_groups_for_arch(&current.recommends, &dependency_arch)?;
                for group in groups {
                    let mut trial = self.clone();
                    if trial.resolve_groups(&[group], 0).is_ok() {
                        self.visiting = trial.visiting;
                        self.done = trial.done;
                        self.plan = trial.plan;
                    }
                }
            }
        }
        Ok(())
    }
}

/// Validate both directions of Conflicts/Breaks against *foreign* installed
/// package identities, using actual dpkg status metadata (not repository latest).
/// A native-only plan is invalid even when the conflicting foreign package has
/// a different name. Never infer that a foreign package will be removed.
fn reject_foreign_negative_relations(
    catalog: &BTreeMap<PackageKey, PackageSummary>,
    installed: &BTreeMap<cache_query::InstalledKey, InstalledRelations>,
    plan: &[PlanStep],
    architecture: &str,
) -> Result<()> {
    for step in plan {
        if step.architecture != architecture && step.architecture != "all" {
            bail!("foreign-architecture transaction step requires a complete foreign resolver");
        }
        let staged_key = PackageKey {
            name: step.package.clone(), architecture: step.architecture.clone(),
        };
        let proposed = catalog.get(&staged_key).with_context(|| format!(
            "planned {}:{} missing repository metadata", step.package, step.architecture,
        ))?;
        negative_relations(&proposed.replaces)?;
        if proposed.version != step.version {
            bail!("inconsistent planned version for {}:{}", step.package, step.architecture);
        }
        for (key, foreign) in installed {
            if key.architecture == architecture || key.architecture == "all" { continue; }
            if !matches!(foreign.multi_arch.as_str(), "no" | "same" | "foreign" | "allowed") {
                bail!("invalid installed Multi-Arch declaration for {}:{}", key.name, key.architecture);
            }
            if key.name == step.package
                && (key.architecture == "all" || step.architecture == "all"
                    || proposed.multi_arch != "same" || foreign.multi_arch != "same"
                    || version::compare(&step.version, &foreign.version)? != Ordering::Equal) {
                bail!("unsafe cross-architecture co-installation of {}:{} with installed {}:{}",
                    step.package, step.architecture, key.name, key.architecture);
            }
            if relations_hit(&proposed.conflicts, &key.name, &foreign.version, &foreign.provides)?
                || relations_hit(&proposed.breaks, &key.name, &foreign.version, &foreign.provides)?
                || relations_hit(&foreign.conflicts, &step.package, &step.version, &proposed.provides)?
                || relations_hit(&foreign.breaks, &step.package, &step.version, &proposed.provides)? {
                bail!("Conflicts/Breaks between planned {}:{} and foreign installed {}:{}",
                    step.package, step.architecture, key.name, key.architecture);
            }
            if replaces_hit(&proposed.replaces, &key.name, &foreign.version)?
                || replaces_hit(&foreign.replaces, &step.package, &step.version)? {
                bail!("Replaces between planned {}:{} and foreign installed {}:{} requires file ownership verification",
                    step.package, step.architecture, key.name, key.architecture);
            }
        }
    }
    Ok(())
}

/// The current solver plans only one native architecture. A staged binary
/// with a same-name foreign installed counterpart could require Multi-Arch:
/// same version synchronization, file overlap checks or removals. The planner
/// cannot safely assume any such action, so it refuses these cases.
#[allow(dead_code)]
fn reject_foreign_identity_overlap(
    entries: &BTreeMap<cache_query::InstalledKey, cache_query::Installed>,
    steps: &[PlanStep], architecture: &str,
) -> Result<()> {
    for step in steps {
        for key in entries.keys() {
            if key.name == step.package
                && key.architecture != architecture
                && key.architecture != "all" {
                bail!("planned {} overlaps installed foreign architecture {}; full Multi-Arch solver required",
                    step.package, key.architecture);
            }
        }
    }
    Ok(())
}

/// Experimental read-only preview for an explicitly requested foreign
/// architecture. This is not a dpkg transaction or an APT policy candidate:
/// it refuses implicit architecture substitution, ambiguous dependencies,
/// installed cross-architecture conflicts, and unsupported Multi-Arch syntax.
/// The returned plan must never be used to authorize package installation.
pub fn preview_explicit_architecture(
    cache: &Path, status: &Path, requested: &str,
    native_architecture: &str, foreign_architecture: &str,
    max_packages: usize,
) -> Result<Vec<PlanStep>> {
    if max_packages == 0 || max_packages > 5000 {
        bail!("max_packages must be 1..=5000");
    }
    if foreign_architecture == native_architecture {
        bail!("explicit foreign preview requires a distinct architecture");
    }
    // Validate both architecture identifiers before using either as a solver
    // scope. An empty or synthetic "all" scope is never a concrete target.
    for arch in [native_architecture, foreign_architecture] {
        if arch.is_empty() || arch == "all"
            || !arch.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            bail!("invalid concrete preview architecture: {arch}");
        }
    }
    let parsed = parse_atom_for_arch(requested, native_architecture)?;
    if parsed.any_arch || parsed.requested_arch.as_deref() != Some(foreign_architecture) {
        bail!("foreign preview requires an explicit package:architecture request");
    }
    let catalog = metadata_query::catalog_by_architectures(
        cache, &[native_architecture, foreign_architecture], 1_000_000,
    )?;
    let complete = cache_query::installed_catalog_by_arch(status)?;
    let relations = read_installed_relations_by_arch(status)?;
    validate_installed_metadata(&complete, &relations)?;
    let installed = scoped_installed(&complete, foreign_architecture)?;
    let scoped = scoped_relations(&relations, foreign_architecture)?;
    let plan = solve_catalog(
        &catalog, &installed, &scoped, requested, max_packages, foreign_architecture,
    )?;
    // Validate against every installed identity outside the target architecture,
    // including the native host. No removals or version synchronization inferred.
    reject_foreign_negative_relations(&catalog, &relations, &plan, foreign_architecture)?;
    Ok(plan)
}

/// Resolve a foreign dependency to its real dpkg binary identity. A
/// universal Architecture: all package is valid only when the foreign binary
/// of the same name is absent; ambiguity remains fail-closed.
fn foreign_dependency_identity(
    catalog: &BTreeMap<PackageKey, PackageSummary>, name: &str, foreign: &str,
) -> Result<Option<PackageKey>> {
    let foreign_key = PackageKey { name: name.into(), architecture: foreign.into() };
    let all_key = PackageKey { name: name.into(), architecture: "all".into() };
    match (catalog.contains_key(&foreign_key), catalog.contains_key(&all_key)) {
        (true, true) => bail!("ambiguous foreign/all dependency identity for {name}"),
        (true, false) => Ok(Some(foreign_key)),
        (false, true) => {
            // In Debian a binary from Architecture: all does not satisfy a
            // foreign-architecture dependency by default. An unqualified
            // foreign Depends requires the provider to declare
            // Multi-Arch: foreign (as confirmed by apt-get -s).
            let item = &catalog[&all_key];
            if item.multi_arch != "foreign" {
                bail!("Architecture: all dependency {name} requires Multi-Arch: foreign");
            }
            Ok(Some(all_key))
        },
        (false, false) => Ok(None),
    }
}

/// Policy-checked diagnostic for an explicit foreign-architecture request.
/// Unlike the exploratory preview, this requires every staged version to be
/// the APT-policy candidate from verified metadata for that exact architecture.
/// It is still read-only and cannot authorize dpkg transactions.
pub fn preview_explicit_architecture_with_policy(
    cache: &Path, status: &Path, apt_root: &Path, requested: &str,
    native_architecture: &str, foreign_architecture: &str,
    max_packages: usize,
) -> Result<Vec<PlanStep>> {
    if max_packages == 0 || max_packages > 5000 {
        bail!("max_packages must be 1..=5000");
    }
    if foreign_architecture == native_architecture {
        bail!("foreign policy preview requires distinct native and foreign architectures");
    }
    for arch in [native_architecture, foreign_architecture] {
        if arch.is_empty() || arch == "all"
            || !arch.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-') {
            bail!("invalid concrete preview architecture: {arch}");
        }
    }
    let target_release = crate::policy::read_default_release(apt_root)?;
    let preferences = crate::apt_preferences::load(apt_root)?;
    // Resolve the explicitly requested root against its policy-selected
    // version, not the latest index stanza. This avoids rejecting a valid
    // pinned root solely because the exploratory catalog chose a newer one.
    let parsed = parse_atom_for_arch(requested, native_architecture)?;
    if parsed.any_arch || parsed.requested_arch.as_deref() != Some(foreign_architecture) {
        bail!("foreign policy preview requires an explicit package:architecture request");
    }
    let mut catalog = metadata_query::catalog_by_architectures(
        cache, &[native_architecture, foreign_architecture], 1_000_000,
    )?;
    let complete = cache_query::installed_catalog_by_arch(status)?;
    let relations = read_installed_relations_by_arch(status)?;
    validate_installed_metadata(&complete, &relations)?;
    let installed = scoped_installed(&complete, foreign_architecture)?;
    let scoped = scoped_relations(&relations, foreign_architecture)?;
    let root_key = PackageKey {
        name: parsed.name.clone(), architecture: foreign_architecture.to_owned(),
    };
    if !catalog.contains_key(&root_key) {
        bail!("foreign root {}:{} missing from repository catalog", parsed.name, foreign_architecture);
    }
    let root_origins = crate::package_provenance::verified_version_origins(
        cache, &parsed.name, foreign_architecture,
    )?.into_iter().filter(|origin| origin.architecture == foreign_architecture)
        .collect::<Vec<_>>();
    let root_candidate = crate::apt_pin_eval::preview_candidate_with_phasing(
        &parsed.name, &root_origins, &preferences,
        installed.get(&root_key).map(String::as_str), target_release.as_deref(), None,
    )?.candidate.with_context(|| format!(
        "APT policy has no foreign root candidate for {}:{}", parsed.name, foreign_architecture,
    ))?;
    let root_summary = metadata_query::verified_exact_version(
        cache, foreign_architecture, &parsed.name, &root_candidate, &root_origins,
    )?;
    if root_summary.architecture != foreign_architecture {
        bail!("foreign root policy candidate has incorrect binary architecture");
    }
    catalog.insert(root_key.clone(), root_summary);
    // Candidate-select the dependency closure before returning a plan.
    // Repository latest is only a discovery seed: every discovered package
    // gets replaced with the verified policy candidate, then re-solved.
    // A bounded fixed point prevents cycles or endlessly changing closures.
    let mut validated = BTreeSet::new();
    validated.insert(root_key.clone());
    let mut roots = BTreeSet::new();
    roots.insert(root_key);
    for _ in 0..16 {
        let mut changed = false;
        for key in &roots {
            if validated.contains(key) { continue; }
            if !catalog.contains_key(key) {
                bail!("foreign dependency {}:{} has no architecture-qualified metadata",
                    key.name, key.architecture);
            }
            let origins = crate::package_provenance::verified_version_origins(
                cache, &key.name, foreign_architecture,
            )?.into_iter().filter(|origin| origin.architecture == key.architecture)
                .collect::<Vec<_>>();
            let candidate = crate::apt_pin_eval::preview_candidate_with_phasing(
                &key.name, &origins, &preferences,
                installed.get(key).map(String::as_str), target_release.as_deref(), None,
            )?.candidate.with_context(|| format!(
                "APT policy has no candidate for foreign dependency {}:{}",
                key.name, key.architecture,
            ))?;
            let summary = metadata_query::verified_exact_version(
                cache, &key.architecture, &key.name, &candidate, &origins,
            )?;
            if summary.architecture != key.architecture {
                bail!("foreign dependency candidate has incorrect binary architecture");
            }
            if catalog.get(key) != Some(&summary) { changed = true; }
            catalog.insert(key.clone(), summary);
            validated.insert(key.clone());
        }
        // Discover mandatory concrete dependencies of the policy-selected
        // packages before solving; otherwise a newer, unpinned dependency
        // can make an otherwise valid plan fail prematurely.
        let mut discovered = false;
        let mut mandatory = BTreeSet::new();
        for key in &roots {
            if !validated.contains(key) { continue; }
            let item = catalog.get(key)
                .context("validated foreign dependency missing catalog entry")?;
            for group in parse_groups_for_arch(&item.pre_depends, foreign_architecture)?.into_iter()
                .chain(parse_groups_for_arch(&item.depends, foreign_architecture)?) {
                if group.len() == 1 {
                    let dep = &group[0];
                    if let Some(requested_arch) = &dep.requested_arch {
                        if requested_arch != foreign_architecture {
                            bail!("explicit cross-architecture dependency needs full Multi-Arch resolver");
                        }
                    }
                    if let Some(identity) = foreign_dependency_identity(
                        &catalog, &dep.name, foreign_architecture,
                    )? {
                        mandatory.insert(identity);
                    }
                }
            }
        }
        for identity in mandatory {
            if roots.insert(identity) { discovered = true; }
        }
        if discovered { continue; }
        let plan = solve_catalog(
            &catalog, &installed, &scoped, requested, max_packages, foreign_architecture,
        )?;
        for step in &plan {
            if roots.insert(PackageKey {
                name: step.package.clone(), architecture: step.architecture.clone(),
            }) { discovered = true; }
        }
        if changed || discovered { continue; }
        reject_foreign_negative_relations(&catalog, &relations, &plan, foreign_architecture)?;
        // A package recorded as Architecture: all must be verified from
        // signed metadata as all, not relabeled as an i386 package.
        for step in &plan {
            if step.architecture != foreign_architecture && step.architecture != "all" {
                bail!("foreign policy preview cannot stage a native binary");
            }
            let identity = PackageKey {
                name: step.package.clone(), architecture: step.architecture.clone(),
            };
            if step.architecture == "all"
                && catalog.get(&identity).is_none_or(|item| item.multi_arch != "foreign") {
                bail!("foreign dependency on Architecture: all requires Multi-Arch: foreign");
            }
            if !validated.contains(&identity) {
                bail!("foreign dependency was not policy-validated");
            }
        }
        return Ok(plan);
    }
    bail!("foreign policy dependency closure did not converge in 16 rounds")
}

/// Read installed packages in bulk; this deliberately does not trust the
/// resolver for actual package transactions.
pub fn preview(cache: &Path, status: &Path, package: &str, architecture: &str, max_packages: usize) -> Result<Vec<PlanStep>> {
    if max_packages == 0 || max_packages > 5000 { bail!("max_packages must be 1..=5000"); }
    let keyed = metadata_query::catalog_by_arch(cache, architecture, 1_000_000)?;
    let catalog = keyed;
    let complete = cache_query::installed_catalog_by_arch(status)?;
    let complete_relations = read_installed_relations_by_arch(status)?;
    validate_installed_metadata(&complete, &complete_relations)?;
    let installed = scoped_installed(&complete, architecture)?;
    let installed_relations = scoped_relations(&complete_relations, architecture)?;
    let plan = solve_catalog(&catalog, &installed, &installed_relations, package, max_packages, architecture)?;
    // Foreign identity compatibility is checked with installed dpkg Multi-Arch metadata below.
    reject_foreign_negative_relations(&catalog, &complete_relations, &plan, architecture)?;
    Ok(plan)
}

/// Joint multi-root plan: a single solver state, never a union of individually
/// valid plans. Candidate policy parity is not established; no execution.
pub fn preview_many(
    cache: &Path, status: &Path, packages: &[String],
    architecture: &str, max_packages: usize,
) -> Result<Vec<PlanStep>> {
    if packages.is_empty() || packages.len() > 128 {
        bail!("expected 1..=128 requested packages");
    }
    if max_packages == 0 || max_packages > 5000 {
        bail!("max_packages must be 1..=5000");
    }
    let mut roots = BTreeSet::new();
    for package in packages {
        let atom = parse_atom_for_arch(package, architecture)?;
        if atom.constraint.is_some() || atom.any_arch || atom.requested_arch.is_some() {
            bail!("batch roots must be unqualified binary package names");
        }
        if !roots.insert(package.clone()) { bail!("duplicate requested package: {package}"); }
    }
    let catalog = metadata_query::catalog_by_arch(cache, architecture, 1_000_000)?;
    let complete = cache_query::installed_catalog_by_arch(status)?;
    let complete_relations = read_installed_relations_by_arch(status)?;
    validate_installed_metadata(&complete, &complete_relations)?;
    let installed = scoped_installed(&complete, architecture)?;
    let relations = scoped_relations(&complete_relations, architecture)?;
    let plan = solve_catalog_many(&catalog, &installed, &relations, &roots.into_iter().collect::<Vec<_>>(), max_packages, architecture)?;
    reject_foreign_negative_relations(&catalog, &complete_relations, &plan, architecture)?;
    Ok(plan)
}

/// Shared read-only solver used both for ordinary previews and candidate
/// metadata selection. No transaction state or dpkg writes.
fn solve_catalog(
    catalog: &BTreeMap<PackageKey, PackageSummary>,
    installed: &BTreeMap<PackageKey, String>,
    installed_relations: &BTreeMap<PackageKey, InstalledRelations>,
    package: &str, max_packages: usize, architecture: &str,
) -> Result<Vec<PlanStep>> {
    solve_catalog_many(catalog, installed, installed_relations, &[package.to_owned()], max_packages, architecture)
}

fn solve_catalog_many(
    catalog: &BTreeMap<PackageKey, PackageSummary>,
    installed: &BTreeMap<PackageKey, String>,
    installed_relations: &BTreeMap<PackageKey, InstalledRelations>,
    packages: &[String], max_packages: usize, architecture: &str,
) -> Result<Vec<PlanStep>> {
    solve_catalog_many_options(catalog, installed, installed_relations, packages,
        max_packages, architecture, false)
}

fn solve_catalog_many_options(
    catalog: &BTreeMap<PackageKey, PackageSummary>,
    installed: &BTreeMap<PackageKey, String>,
    installed_relations: &BTreeMap<PackageKey, InstalledRelations>,
    packages: &[String], max_packages: usize, architecture: &str,
    include_recommends: bool,
) -> Result<Vec<PlanStep>> {
    let mut solver = Solver {
        catalog, installed, installed_relations, visiting: BTreeSet::new(),
        done: BTreeSet::new(), plan: Vec::new(), limit: max_packages,
        architecture: architecture.to_owned(),
        branch_budget: Rc::new(Cell::new(4096)), include_recommends,
        requested_roots: BTreeSet::new(),
    };
    for package in packages {
        if let Some(key) = solver.key_for(package)? {
            solver.requested_roots.insert(key);
        }
    }
    for package in packages {
        solver.resolve(&parse_atom_for_arch(package, architecture)?)?;
    }
    // Validate the entire staged set before exposing a read-only plan.
    // This does not authorize foreign-architecture package installation.
    validate_multiarch_plan(catalog, &solver.plan)?;
    // Validate staged binaries against installed peers as well as other staged
    // binaries. A partial plan must never silently desynchronize Multi-Arch:
    // same co-installed versions or treat Architecture: all as a peer.
    for step in &solver.plan {
        let staged_key = PackageKey {
            name: step.package.clone(), architecture: step.architecture.clone(),
        };
        let candidate = catalog.get(&staged_key)
            .context("staged binary missing verified catalog metadata")?;
        for (installed_key, installed_version) in installed {
            if installed_key.name != step.package || installed_key == &staged_key {
                continue;
            }
            if installed_key.architecture == "all" || staged_key.architecture == "all" {
                bail!("installed Architecture: all collides with staged concrete binary {}", step.package);
            }
            // The exact installed identity is needed: repository latest is not
            // evidence of the version or Multi-Arch flag currently installed.
            let relation = installed_relations.get(installed_key)
                .with_context(|| format!("missing installed Multi-Arch metadata for {}:{}",
                    installed_key.name, installed_key.architecture))?;
            if relation.version != *installed_version || relation.multi_arch != "same"
                || candidate.multi_arch != "same"
                || version::compare(&step.version, installed_version)? != Ordering::Equal {
                bail!("unsafe staged/installed Multi-Arch co-installation for {}:{} and {}:{}",
                    staged_key.name, staged_key.architecture,
                    installed_key.name, installed_key.architecture);
            }
        }
    }
    Ok(solver.plan)
}

/// Build a candidate-specific read-only catalog for the current dependency
/// closure. Start by replacing the requested package with its verified candidate,
/// then repeat with dependencies discovered from the selected metadata. Only
/// complete and stable plans may be returned; unsupported alternatives and
/// unverified/missing candidate metadata fail closed.
pub fn preview_with_policy(
    cache: &Path, status: &Path, apt_root: &Path, package: &str,
    architecture: &str, max_packages: usize,
) -> Result<Vec<PlanStep>> {
    preview_many_with_policy(cache, status, apt_root, &[package.to_owned()], architecture, max_packages)
}

/// Resolve all requested roots in one candidate-verified policy closure.
/// Unsupported policy/architecture cases fail closed; never runs dpkg.
pub fn preview_many_with_policy(
    cache: &Path, status: &Path, apt_root: &Path, packages: &[String],
    architecture: &str, max_packages: usize,
) -> Result<Vec<PlanStep>> {
    preview_many_with_policy_options(
        cache, status, apt_root, packages, architecture, max_packages, false,
    )
}

/// Verified candidate policy and an explicitly opted-in soft dependency mode.
/// Default remains mandatory dependencies only until APT parity is established.
pub fn preview_many_with_policy_options(
    cache: &Path, status: &Path, apt_root: &Path, packages: &[String],
    architecture: &str, max_packages: usize, include_recommends: bool,
) -> Result<Vec<PlanStep>> {
    if packages.is_empty() || packages.len() > 128 {
        bail!("expected 1..=128 requested packages");
    }
    let mut requested = BTreeSet::new();
    for package in packages {
        let atom = parse_atom_for_arch(package, architecture)?;
        if atom.constraint.is_some() || atom.any_arch || atom.requested_arch.is_some() {
            bail!("policy batch roots must be unqualified binary names");
        }
        if !requested.insert(package.clone()) { bail!("duplicate requested package: {package}"); }
    }
    if max_packages == 0 || max_packages > 5000 {
        bail!("max_packages must be 1..=5000");
    }
    let target_release = crate::policy::read_default_release(apt_root)?;
    let preferences = crate::apt_preferences::load(apt_root)?;
    for package in packages {
        crate::apt_pin_eval::evaluate_with_phasing(
            package, &[], &preferences, target_release.as_deref(), None,
        )?;
    }
    let complete = cache_query::installed_catalog_by_arch(status)?;
    let complete_relations = read_installed_relations_by_arch(status)?;
    validate_installed_metadata(&complete, &complete_relations)?;
    let installed = scoped_installed(&complete, architecture)?;
    let installed_relations = scoped_relations(&complete_relations, architecture)?;
    let mut catalog = metadata_query::catalog_by_arch(cache, architecture, 1_000_000)?;
    // A package's installed version is not sufficient evidence that its
    // dependency metadata is available from a signed repository occurrence.
    let mut validated = BTreeSet::<String>::new();
    let mut optional_names = BTreeSet::<String>::new();
    let mut roots = BTreeSet::new();
    roots.extend(requested.iter().cloned());
    for _ in 0..16 {
        let mut changed = false;
        for name in &roots {
            if validated.contains(name) { continue; }
            let origins = crate::package_provenance::verified_version_origins(cache, name, architecture)?;
            let chosen = crate::apt_pin_eval::preview_candidate_with_phasing(
                name, &origins, &preferences, installed.iter().find(|(key, _)| key.name == *name).map(|(_, version)| version.as_str()),
                target_release.as_deref(), None,
            )?.candidate;
            let chosen = match chosen {
                Some(candidate) => candidate,
                None if optional_names.contains(name) => {
                    // A soft dependency pinned out of the candidate set does
                    // not invalidate the whole mandatory install plan.
                    let key = metadata_query::native_key_for(&catalog, name, architecture)?;
                    catalog.remove(&key);
                    validated.insert(name.clone());
                    changed = true;
                    continue;
                }
                None => bail!("APT policy has no candidate for {name}"),
            };
            if !origins.iter().any(|origin| origin.version == chosen) {
                bail!("candidate {name} {chosen} is not present in verified repository metadata");
            }
            let summary = metadata_query::verified_exact_version(cache, architecture, name, &chosen, &origins)?;
            // Candidate metadata may not silently switch from a native binary
            // to Architecture: all, or pick a foreign architecture stanza.
            // Updates are keyed by the exact binary identity first.
            let key = metadata_query::native_key_for(&catalog, name, architecture)?;
            let old = catalog.get(&key).context("missing preexisting binary metadata")?;
            if old != &summary { changed = true; }
            metadata_query::insert_native_candidate(&mut catalog, summary, architecture)?;
            validated.insert(name.clone());
        }
        // Select metadata for mandatory concrete dependencies *before* trying
        // the solver. Otherwise latest-version dependencies could fail
        // prematurely even though an older pinned candidate is satisfiable.
        let mut mandatory = BTreeSet::new();
        for name in &roots {
            if !validated.contains(name) { continue; }
            let key = match metadata_query::native_key_for(&catalog, name, architecture) {
                Ok(key) => key,
                Err(_) if optional_names.contains(name) => continue,
                Err(err) => return Err(err),
            };
            let summary = catalog.get(&key).context("validated candidate missing catalog metadata")?;
            for group in parse_groups_for_arch(&summary.pre_depends, architecture)?.into_iter()
                .chain(parse_groups_for_arch(&summary.depends, architecture)?) {
                if group.len() == 1 && (catalog.contains_key(&PackageKey { name: group[0].name.clone(), architecture: architecture.into() })
                    || catalog.contains_key(&PackageKey { name: group[0].name.clone(), architecture: "all".into() })) {
                    mandatory.insert(group[0].name.clone());
                }
            }
        }
        let mut discovered = false;
        for name in mandatory {
            if roots.insert(name.clone()) { discovered = true; }
            // A mandatory relationship upgrades this identity from optional.
            optional_names.remove(&name);
        }
        if include_recommends {
            let mut recommended = BTreeSet::new();
            for name in &roots {
                if !validated.contains(name) { continue; }
                let key = match metadata_query::native_key_for(&catalog, name, architecture) {
                    Ok(key) => key,
                    Err(_) => continue,
                };
                let summary = &catalog[&key];
                let effective_arch = if summary.architecture == "all" {
                    architecture
                } else {
                    &summary.architecture
                };
                for group in parse_groups_for_arch(&summary.recommends, effective_arch)? {
                    for req in group {
                        if req.requested_arch.as_deref().is_some_and(|arch|
                            arch != architecture && arch != "all") {
                            continue;
                        }
                        if metadata_query::native_key_for(&catalog, &req.name, architecture).is_ok()
                            && !roots.contains(&req.name) {
                            recommended.insert(req.name);
                        }
                    }
                }
            }
            for name in recommended {
                if roots.insert(name.clone()) {
                    optional_names.insert(name);
                    discovered = true;
                }
            }
        }
        if discovered { continue; }
        let plan = solve_catalog_many_options(
            &catalog, &installed, &installed_relations, packages,
            max_packages, architecture, include_recommends,
        )?;
        for step in &plan {
            if roots.insert(step.package.clone()) {
                discovered = true;
            }
        }
        if !changed && !discovered {
            check_plan_candidates(&plan, |name, current| {
                let origins = crate::package_provenance::verified_version_origins(cache, name, architecture)?;
                Ok(crate::apt_pin_eval::preview_candidate_with_phasing(
                    name, &origins, &preferences, current,
                    target_release.as_deref(), None,
                )?.candidate)
            })?;
            // Foreign identity compatibility is checked with installed dpkg Multi-Arch metadata below.
            reject_foreign_negative_relations(&catalog, &complete_relations, &plan, architecture)?;
            return Ok(plan);
        }
    }
    bail!("candidate metadata dependency closure did not converge in 16 rounds")
}

/// Separate the candidate comparison so mismatch and candidate absence are
/// exercised independently of GPG test fixtures.
fn check_plan_candidates(
    steps: &[PlanStep],
    mut candidate_for: impl FnMut(&str, Option<&str>) -> Result<Option<String>>,
) -> Result<()> {
    for step in steps {
        let candidate = candidate_for(&step.package, step.installed.as_deref())?
            .with_context(|| format!("APT policy has no candidate for {}", step.package))?;
        if candidate != step.version {
            bail!("dependency plan selected {} {} but APT policy candidate is {}; refusing latest-version metadata mismatch",
                step.package, step.version, candidate);
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(name: &str, version: &str, depends: &str) -> PackageSummary {
        PackageSummary {
            name: name.into(), version: version.into(), architecture: "amd64".into(), multi_arch: "no".into(),
            description: String::new(), section: String::new(), depends: depends.into(),
            pre_depends: String::new(), recommends: String::new(), suggests: String::new(),
            provides: String::new(), conflicts: String::new(), breaks: String::new(), replaces: String::new(),
        }
    }
    fn test_keyed(catalog: &BTreeMap<String, PackageSummary>) -> BTreeMap<PackageKey, PackageSummary> {
        catalog.values().map(|item| (PackageKey {
            name: item.name.clone(), architecture: item.architecture.clone(),
        }, item.clone())).collect()
    }
    fn test_keyed_versions(
        catalog: &BTreeMap<PackageKey, PackageSummary>,
        installed: &BTreeMap<String, String>, arch: &str,
    ) -> BTreeMap<PackageKey, String> {
        installed.iter().map(|(name, value)| {
            let key = metadata_query::native_key_for(catalog, name, arch).unwrap_or(
                PackageKey { name: name.clone(), architecture: arch.into() }
            );
            (key, value.clone())
        }).collect()
    }
    fn test_keyed_relations(
        catalog: &BTreeMap<PackageKey, PackageSummary>,
        relations: &BTreeMap<String, InstalledRelations>, arch: &str,
    ) -> BTreeMap<PackageKey, InstalledRelations> {
        relations.iter().map(|(name, value)| {
            let key = metadata_query::native_key_for(catalog, name, arch).unwrap_or(
                PackageKey { name: name.clone(), architecture: arch.into() }
            );
            (key, value.clone())
        }).collect()
    }
    fn solve_catalog_test(
        catalog: &BTreeMap<PackageKey, PackageSummary>,
        installed: &BTreeMap<String, String>,
        relations: &BTreeMap<String, InstalledRelations>,
        package: &str, limit: usize, arch: &str,
    ) -> Result<Vec<PlanStep>> {
        let binary_installed = test_keyed_versions(catalog, installed, arch);
        let binary_relations = test_keyed_relations(catalog, relations, arch);
        solve_catalog(catalog, &binary_installed, &binary_relations, package, limit, arch)
    }
    fn solve(catalog: &BTreeMap<String, PackageSummary>, installed: &BTreeMap<String,String>, name: &str) -> Result<Vec<PlanStep>> {
        let keyed = test_keyed(catalog);
        let keyed_installed = test_keyed_versions(&keyed, installed, "amd64");
        let mut s = Solver {
            catalog: &keyed, installed: &keyed_installed, installed_relations: &BTreeMap::new(),
            visiting: BTreeSet::new(), done: BTreeSet::new(),
            plan: Vec::new(), limit: 20, architecture: "amd64".into(), branch_budget: Rc::new(Cell::new(4096)), include_recommends: false,
            requested_roots: BTreeSet::new(),
        };
        s.resolve(&parse_atom_for_arch(name, "amd64")?)?;
        Ok(s.plan)
    }
    #[test]
    fn replaces_declared_against_installed_binary_blocks_plan() {
        let mut app = item("app", "1", "");
        app.replaces = "old-lib (<< 3)".into();
        let catalog = test_keyed(&BTreeMap::from([("app".into(), app)]));
        let installed = BTreeMap::from([("old-lib".into(), "2".into())]);
        let relations = BTreeMap::from([("old-lib".into(), InstalledRelations {
            version: "2".into(), ..Default::default()
        })]);
        assert!(solve_catalog_test(&catalog, &installed, &relations, "app", 20, "amd64")
            .unwrap_err().to_string().contains("Replaces"));
        // Outside the versioned Replaces range: no known ownership transfer.
        let installed = BTreeMap::from([("old-lib".into(), "3".into())]);
        let relations = BTreeMap::from([("old-lib".into(), InstalledRelations {
            version: "3".into(), ..Default::default()
        })]);
        assert!(solve_catalog_test(&catalog, &installed, &relations, "app", 20, "amd64").is_ok());
    }

    #[test]
    fn reverse_installed_replaces_blocks_plan() {
        let catalog = test_keyed(&BTreeMap::from([("app".into(), item("app", "1", ""))]));
        let installed = BTreeMap::from([("old-lib".into(), "2".into())]);
        let relations = BTreeMap::from([("old-lib".into(), InstalledRelations {
            version: "2".into(), replaces: "app".into(), ..Default::default()
        })]);
        assert!(solve_catalog_test(&catalog, &installed, &relations, "app", 20, "amd64")
            .unwrap_err().to_string().contains("Replaces"));
    }

    #[test]
    fn staged_replaces_and_foreign_installed_replaces_fail_closed() {
        let mut app = item("app", "1", "lib");
        app.replaces = "lib".into();
        let catalog = test_keyed(&BTreeMap::from([
            ("app".into(), app.clone()), ("lib".into(), item("lib", "1", "")),
        ]));
        assert!(solve_catalog_test(&catalog, &BTreeMap::new(), &BTreeMap::new(),
            "app", 20, "amd64").is_err());
        app.depends.clear();
        let catalog = test_keyed(&BTreeMap::from([("app".into(), app)]));
        let foreign = BTreeMap::from([(
            cache_query::InstalledKey { name: "lib".into(), architecture: "i386".into() },
            InstalledRelations { version: "1".into(), multi_arch: "no".into(), ..Default::default() },
        )]);
        let plan = [PlanStep { package: "app".into(), architecture: "amd64".into(),
            version: "1".into(), installed: None }];
        assert!(reject_foreign_negative_relations(&catalog, &foreign, &plan, "amd64")
            .unwrap_err().to_string().contains("Replaces"));
        assert!(replaces_hit("lib [amd64]", "lib", "1").is_err());
    }

    #[test]
    fn installed_keyed_relations_reject_native_all_mismatch_even_same_version() {
        let catalog = BTreeMap::from([(
            PackageKey { name: "lib".into(), architecture: "amd64".into() },
            item("lib", "1", ""),
        )]);
        let installed = BTreeMap::from([(
            PackageKey { name: "lib".into(), architecture: "all".into() },
            "1".into(),
        )]);
        assert!(solve_catalog(&catalog, &installed, &BTreeMap::new(), "lib", 20, "amd64").is_err());
        let root = BTreeMap::from([
            (PackageKey { name: "app".into(), architecture: "amd64".into() },
                item("app", "1", "lib")),
            (PackageKey { name: "lib".into(), architecture: "amd64".into() },
                item("lib", "1", "")),
        ]);
        assert!(solve_catalog(&root, &installed, &BTreeMap::new(), "app", 20, "amd64").is_err());
    }

    #[test]
    fn foreign_dependency_retains_all_identity_and_rejects_same_name_ambiguity() {
        let mut all = item("toolkit", "1.0", "");
        all.architecture = "all".into();
        all.multi_arch = "foreign".into();
        let mut entries = BTreeMap::from([(
            PackageKey { name: "toolkit".into(), architecture: "all".into() }, all,
        )]);
        let key = foreign_dependency_identity(&entries, "toolkit", "i386").unwrap().unwrap();
        assert_eq!(key.architecture, "all");
        let mut unqualified = entries.clone();
        unqualified.get_mut(&key).unwrap().multi_arch = "no".into();
        assert!(foreign_dependency_identity(&unqualified, "toolkit", "i386").is_err());
        assert!(foreign_dependency_identity(&entries, "missing", "i386").unwrap().is_none());
        let foreign = item("toolkit", "1.0", "");
        entries.insert(
            PackageKey {name: "toolkit".into(), architecture: "amd64".into()}, foreign.clone(),
        );
        assert_eq!(foreign_dependency_identity(&entries, "toolkit", "i386").unwrap().unwrap().architecture, "all");
        let mut foreign = foreign;
        foreign.architecture = "i386".into();
        entries.insert(PackageKey { name: "toolkit".into(), architecture: "i386".into() }, foreign);
        assert!(foreign_dependency_identity(&entries, "toolkit", "i386").is_err());
    }

    #[test]
    fn foreign_policy_validation_rejects_invalid_scope_before_metadata_io() {
        let invalid_cache = Path::new("/nonexistent/rpt-foreign-cache");
        let status = Path::new("/nonexistent/rpt-foreign-status");
        let apt_root = Path::new("/nonexistent/rpt-foreign-apt");
        for (native, foreign, max) in [
            ("amd64", "amd64", 20),
            ("amd64", "all", 20),
            ("amd64", "../i386", 20),
            ("", "i386", 20),
            ("amd64", "i386", 0),
            ("amd64", "i386", 5001),
        ] {
            assert!(preview_explicit_architecture_with_policy(
                invalid_cache, status, apt_root, "sample:i386", native, foreign, max,
            ).is_err());
        }
    }
    #[test]
    fn explicit_foreign_preview_requires_qualified_request_and_safe_catalog() {
        let root = std::env::temp_dir().join(format!("rpt-foreign-preview-{}", std::process::id()));
        let dir = root.join("0123456789abcdef01234567");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("InRelease"), "fixture").unwrap();
        std::fs::write(dir.join("index-0.raw"),
            "Package: lib\nVersion: 1\nArchitecture: amd64\nMulti-Arch: same\n\n\
Package: lib\nVersion: 1\nArchitecture: i386\nMulti-Arch: same\n\n").unwrap();
        let status = root.join("status");
        std::fs::write(&status, "").unwrap();
        let plan = preview_explicit_architecture(&root, &status, "lib:i386", "amd64", "i386", 20).unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].architecture, "i386");
        assert!(preview_explicit_architecture(&root, &status, "lib", "amd64", "i386", 20).is_err());
        assert!(preview_explicit_architecture(&root, &status, "lib:amd64", "amd64", "i386", 20).is_err());
        assert!(preview_explicit_architecture(&root, &status, "lib:i386", "amd64", "amd64", 20).is_err());
        std::fs::write(&status,
            "Package: lib\nArchitecture: amd64\nVersion: 2\nMulti-Arch: same\nStatus: install ok installed\n\n").unwrap();
        assert!(preview_explicit_architecture(&root, &status, "lib:i386", "amd64", "i386", 20).is_err());
        // Foreign preview must not silently stage a native dependency. A
        // mixed-architecture transaction needs a complete APT resolver.
        std::fs::write(&status, "").unwrap();
        std::fs::write(dir.join("index-0.raw"),
            "Package: app\nVersion: 1\nArchitecture: i386\nDepends: native-lib:amd64\n\n\
Package: native-lib\nVersion: 1\nArchitecture: amd64\n\n").unwrap();
        assert!(preview_explicit_architecture(&root, &status, "app:i386", "amd64", "i386", 20).is_err());
        // A dependency resolved entirely within the foreign architecture is
        // still a valid read-only plan.
        std::fs::write(dir.join("index-0.raw"),
            "Package: app\nVersion: 1\nArchitecture: i386\nDepends: foreign-lib\n\n\
Package: foreign-lib\nVersion: 1\nArchitecture: i386\n\n").unwrap();
        let foreign_plan = preview_explicit_architecture(&root, &status, "app:i386", "amd64", "i386", 20).unwrap();
        assert_eq!(foreign_plan.len(), 2);
        assert!(foreign_plan.iter().all(|step| step.architecture == "i386"));
        assert!(preview_explicit_architecture(&root, &status, "app:i386", "all", "i386", 20).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn staged_multiarch_peer_requires_installed_status_and_matching_version() {
        let mut native = item("lib", "2", "");
        native.multi_arch = "same".into();
        let catalog = BTreeMap::from([(
            PackageKey { name: "lib".into(), architecture: "amd64".into() }, native,
        )]);
        let foreign = PackageKey { name: "lib".into(), architecture: "i386".into() };
        let installed = BTreeMap::from([(foreign.clone(), "2".into())]);
        let mut relations = BTreeMap::from([(foreign.clone(), InstalledRelations {
            version: "2".into(), multi_arch: "same".into(), ..Default::default()
        })]);
        assert!(solve_catalog(&catalog, &installed, &relations, "lib", 20, "amd64").is_ok());
        relations.get_mut(&foreign).unwrap().version = "1".into();
        assert!(solve_catalog(&catalog, &installed, &relations, "lib", 20, "amd64").is_err());
        relations.get_mut(&foreign).unwrap().version = "2".into();
        relations.get_mut(&foreign).unwrap().multi_arch = "foreign".into();
        assert!(solve_catalog(&catalog, &installed, &relations, "lib", 20, "amd64").is_err());
        assert!(solve_catalog(&catalog, &installed, &BTreeMap::new(), "lib", 20, "amd64").is_err());
        relations.get_mut(&foreign).unwrap().multi_arch = "same".into();
        let older = BTreeMap::from([(foreign, "1".into())]);
        assert!(solve_catalog(&catalog, &older, &relations, "lib", 20, "amd64").is_err());
    }

    #[test]
    fn installed_all_reverse_conflict_checked_by_exact_identity() {
        let catalog = BTreeMap::from([(
            PackageKey { name: "app".into(), architecture: "amd64".into() },
            item("app", "1", ""),
        )]);
        let installed = BTreeMap::from([(
            PackageKey { name: "blocker".into(), architecture: "all".into() },
            "3".into(),
        )]);
        let relations = BTreeMap::from([(
            PackageKey { name: "blocker".into(), architecture: "all".into() },
            InstalledRelations {
                version: "3".into(), breaks: "app (>= 1)".into(), ..Default::default()
            },
        )]);
        assert!(solve_catalog(&catalog, &installed, &relations, "app", 20, "amd64").is_err());
    }

    #[test]
    fn installed_status_safety_metadata_reconciles_foreign_identities() {
        let file = std::env::temp_dir().join(format!("rpt-keyed-relations-{}", std::process::id()));
        std::fs::write(&file, "\
Package: lib\nArchitecture: amd64\nVersion: 1\nMulti-Arch: same\nStatus: install ok installed\n\n\
Package: lib\nArchitecture: i386\nVersion: 1\nMulti-Arch: same\nStatus: hold ok installed\nEssential: yes\n\n").unwrap();
        let complete = cache_query::installed_catalog_by_arch(&file).unwrap();
        let relations = read_installed_relations_by_arch(&file).unwrap();
        validate_installed_metadata(&complete, &relations).unwrap();
        let selected = scoped_relations(&relations, "amd64").unwrap();
        assert_eq!(selected.len(), 1);
        let foreign = &relations[&cache_query::InstalledKey {
            name: "lib".into(), architecture: "i386".into(),
        }];
        assert!(foreign.held && foreign.essential);
        std::fs::remove_file(file).unwrap();
    }

    #[test]
    fn planned_foreign_coinstalled_identity_fails_closed() {
        let entries = BTreeMap::from([
            (cache_query::InstalledKey { name: "lib".into(), architecture: "amd64".into() },
                cache_query::Installed { version: "1".into(), architecture: "amd64".into(), status: "install ok installed".into() }),
            (cache_query::InstalledKey { name: "lib".into(), architecture: "i386".into() },
                cache_query::Installed { version: "1".into(), architecture: "i386".into(), status: "install ok installed".into() }),
        ]);
        let plan = [PlanStep { architecture: "amd64".into(), package: "lib".into(), version: "2".into(), installed: Some("1".into()) }];
        assert!(reject_foreign_identity_overlap(&entries, &plan, "amd64").is_err());
        let unrelated = [PlanStep { architecture: "amd64".into(), package: "app".into(), version: "1".into(), installed: None }];
        assert!(reject_foreign_identity_overlap(&entries, &unrelated, "amd64").is_ok());
    }

    #[test]
    fn candidate_projection_rejects_same_name_native_all_identity() {
        let native = item("pkg", "2", "current");
        let mut common = item("pkg", "1", "old");
        common.architecture = "all".into();
        let keyed = BTreeMap::from([
            (metadata_query::PackageKey { name: "pkg".into(), architecture: "amd64".into() }, native),
            (metadata_query::PackageKey { name: "pkg".into(), architecture: "all".into() }, common),
        ]);
        assert!(metadata_query::native_catalog_projection(keyed).is_err());
    }
    #[test]
    fn keyed_solver_tracks_architecture_all_in_dependency_plan() {
        let mut docs = item("docs", "1", "");
        docs.architecture = "all".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "docs (>= 1)")),
            ("docs".into(), docs),
        ]);
        let plan = solve_catalog_test(&test_keyed(&catalog), &BTreeMap::new(),
            &BTreeMap::new(), "app", 20, "amd64").unwrap();
        assert_eq!(plan.iter().map(|p| (p.package.as_str(), p.architecture.as_str()))
            .collect::<Vec<_>>(), [("docs", "all"), ("app", "amd64")]);
    }

    #[test]
    fn keyed_solver_rejects_ambiguous_native_and_all_dependency() {
        let app = item("app", "1", "lib");
        let lib = item("lib", "2", "");
        let mut common = item("lib", "1", "");
        common.architecture = "all".into();
        let keyed = BTreeMap::from([
            (PackageKey { name: "app".into(), architecture: "amd64".into() }, app),
            (PackageKey { name: "lib".into(), architecture: "amd64".into() }, lib),
            (PackageKey { name: "lib".into(), architecture: "all".into() }, common),
        ]);
        assert!(solve_catalog_test(&keyed, &BTreeMap::new(),
            &BTreeMap::new(), "app", 20, "amd64").is_err());
    }

    #[test]
    fn candidate_catalog_can_resolve_older_pinned_dependency_metadata() {
        let mut latest = BTreeMap::from([
            ("app".into(), item("app", "2", "impossible")),
            ("lib".into(), item("lib", "2", "impossible")),
        ]);
        let installed = BTreeMap::new();
        let relations = BTreeMap::new();
        // The real candidate selected by policy may carry a completely
        // different dependency graph from the latest published version.
        latest.insert("app".into(), item("app", "1", "lib (>= 1)"));
        latest.insert("lib".into(), item("lib", "1", ""));
        let plan = solve_catalog_test(&test_keyed(&latest), &installed, &relations, "app", 20, "amd64").unwrap();
        assert_eq!(plan.iter().map(|x| (x.package.as_str(), x.version.as_str()))
            .collect::<Vec<_>>(), [("lib", "1"), ("app", "1")]);
    }
    #[test]
    fn plan_candidate_gate_rejects_pinned_or_held_back_versions() {
        let steps = vec![PlanStep { architecture: "amd64".into(), package: "lib".into(), version: "2.0".into(), installed: None }];
        assert!(check_plan_candidates(&steps, |_, _| Ok(Some("1.0".into()))).is_err());
        assert!(check_plan_candidates(&steps, |_, _| Ok(None)).is_err());
        assert!(check_plan_candidates(&steps, |_, _| Ok(Some("2.0".into()))).is_ok());
    }
    #[test]
    fn gate_validates_each_dependency_not_only_requested_package() {
        let steps = vec![
            PlanStep { architecture: "amd64".into(), package: "lib".into(), version: "2".into(), installed: Some("1".into()) },
            PlanStep { architecture: "amd64".into(), package: "app".into(), version: "1".into(), installed: None },
        ];
        let mut seen = Vec::new();
        assert!(check_plan_candidates(&steps, |name, installed| {
            seen.push((name.to_owned(), installed.map(str::to_owned)));
            Ok(Some(if name == "lib" { "2" } else { "0" }.into()))
        }).is_err());
        assert_eq!(seen, vec![("lib".into(), Some("1".into())), ("app".into(), None)]);
    }
    #[test]
    fn policy_gate_uses_pinning_candidate_instead_of_latest() {
        use crate::package_provenance::{ReleaseIdentity, VerifiedVersionOrigin};
        let origin = |version: &str, archive: &str| VerifiedVersionOrigin {
            version: version.into(), architecture: "amd64".into(),
            repository_uri: "https://example.org".into(), suite: archive.into(),
            index_path: "main/binary-amd64/Packages.xz".into(),
            component: "main".into(), phased_update_percentage: None,
            source_package: "lib".into(), source_version: version.into(),
            release: ReleaseIdentity { archive: Some(archive.into()), ..Default::default() },
        };
        let origins = [origin("1.0", "stable"), origin("2.0", "testing")];
        let pins = [crate::apt_preferences::PinRecord {
            package: "lib".into(), pin: "release a=stable".into(), priority: 900,
        }];
        let plan = [PlanStep { architecture: "amd64".into(), package: "lib".into(), version: "2.0".into(), installed: None }];
        assert!(check_plan_candidates(&plan, |name, installed| {
            Ok(crate::apt_pin_eval::preview_candidate_with_phasing(
                name, &origins, &pins, installed, None, None,
            )?.candidate)
        }).is_err());
    }
    #[test]
    fn any_suffix_requires_multiarch_allowed_on_selected_native_package() {
        for arch in ["amd64", "arm64"] {
            let mut interpreter = item("interpreter", "3", "");
            interpreter.architecture = arch.into();
            interpreter.multi_arch = "allowed".into();
            let mut app = item("app", "1", "interpreter:any (>= 2)");
            app.architecture = arch.into();
            let catalog = BTreeMap::from([
                ("app".into(), app.clone()),
                ("interpreter".into(), interpreter.clone()),
            ]);
            let result = solve_catalog_test(&test_keyed(&catalog), &BTreeMap::new(),
                &BTreeMap::new(), "app", 20, arch).unwrap();
            assert_eq!(result.iter().map(|s| s.package.as_str()).collect::<Vec<_>>(),
                ["interpreter", "app"]);
            for value in ["no", "same", "foreign"] {
                let mut denied = interpreter.clone();
                denied.multi_arch = value.into();
                let catalog = BTreeMap::from([
                    ("app".into(), app.clone()), ("interpreter".into(), denied),
                ]);
                assert!(solve_catalog_test(&test_keyed(&catalog), &BTreeMap::new(),
                    &BTreeMap::new(), "app", 20, arch).is_err());
            }
            let foreign = if arch == "arm64" { "amd64" } else { "arm64" };
            assert_eq!(parse_atom_for_arch(&format!("interpreter:{foreign}"), arch).unwrap().requested_arch.as_deref(), Some(foreign));
        }
    }
    #[test]
    fn native_qualifiers_accept_current_arch_and_reject_foreign_or_any() {
        for arch in ["amd64", "arm64"] {
            assert_eq!(parse_atom_for_arch("lib:native (>= 1:2.0)", arch).unwrap().name, "lib");
            assert_eq!(parse_atom_for_arch(&format!("lib:{arch}"), arch).unwrap().name, "lib");
            assert!(parse_atom_for_arch("lib:any", arch).unwrap().any_arch);
            let foreign = if arch == "arm64" { "amd64" } else { "arm64" };
            assert_eq!(parse_atom_for_arch(&format!("lib:{foreign}"), arch).unwrap().requested_arch.as_deref(), Some(foreign));
            assert!(parse_groups_for_arch("pkg-a:native | pkg-b:native, pkg-c (>= 2)", arch).is_ok());
        }
    }
    #[test]
    fn installed_status_guards_held_essential_and_protected() {
        for (status, flag, error_fragment) in [
            ("hold ok installed", "", "held"),
            ("install ok installed", "Essential: yes\n", "Essential"),
            ("install ok installed", "Protected: yes\n", "Protected"),
        ] {
            let raw = format!(
                "Package: protected-pkg\nVersion: 1\nArchitecture: amd64\nStatus: {status}\n{flag}\n"
            );
            let file = std::env::temp_dir().join(format!(
                "rpt-installed-safety-{}-{}", std::process::id(), error_fragment,
            ));
            std::fs::write(&file, raw).unwrap();
            let relations = read_installed_relations(&file, "amd64").unwrap();
            std::fs::remove_file(file).unwrap();
            let pkg = &relations["protected-pkg"];
            assert!(ensure_unchanged_protected("protected-pkg", "1", "2", pkg)
                .unwrap_err().to_string().contains(error_fragment));
            assert!(ensure_unchanged_protected("protected-pkg", "1", "1", pkg).is_ok());
            assert!(ensure_unchanged_protected("protected-pkg", "9", "9", pkg).is_err());
        }
        assert!(parse_critical_flag(Some("maybe"), "Essential").is_err());
        assert!(!parse_critical_flag(None, "Protected").unwrap());
    }

    #[test]
    fn backtracking_rejects_held_alternative_and_selects_viable_option() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "held-dep (>= 2) | other")),
            ("held-dep".into(), item("held-dep", "2", "")),
            ("other".into(), item("other", "1", "")),
        ]);
        let installed = BTreeMap::from([("held-dep".into(), "1".into())]);
        let relations = BTreeMap::from([("held-dep".into(), InstalledRelations {
            version: "1".into(), held: true, ..Default::default()
        })]);
        let plan = solve_catalog_test(&test_keyed(&catalog), &installed, &relations, "app", 20, "amd64").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(), ["other", "app"]);
    }

    #[test]
    fn critical_flag_blocks_upgrade_in_full_plan() {
        let catalog = BTreeMap::from([("core".into(), item("core", "2", ""))]);
        let installed = BTreeMap::from([("core".into(), "1".into())]);
        for name in ["essential", "protected"] {
            let mut state = InstalledRelations { version: "1".into(), ..Default::default() };
            if name == "essential" { state.essential = true; }
            else { state.protected = true; }
            let relations = BTreeMap::from([("core".into(), state)]);
            assert!(solve_catalog_test(&test_keyed(&catalog), &installed, &relations, "core", 20, "amd64").is_err());
        }
    }
    #[test]
    fn held_package_cannot_be_upgraded_in_dependency_plan() {
        let mut installed_relations = BTreeMap::new();
        installed_relations.insert("lib".to_owned(), InstalledRelations {
            version: "1".into(), held: true, ..Default::default()
        });
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "lib (>= 2)")),
            ("lib".into(), item("lib", "2", "")),
        ]);
        let installed = BTreeMap::from([("lib".into(), "1".into())]);
        assert!(solve_catalog_test(
            &test_keyed(&catalog), &installed, &installed_relations, "app", 20, "amd64",
        ).is_err());
        installed_relations.get_mut("lib").unwrap().held = false;
        assert!(solve_catalog_test(
            &test_keyed(&catalog), &installed, &installed_relations, "app", 20, "amd64",
        ).is_ok());
    }
    #[test]
    fn dependency_order_and_installed_satisfaction() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "2.0", "lib (>= 1.0)")),
            ("lib".into(), item("lib", "1.1", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p|p.package.as_str()).collect::<Vec<_>>(), ["lib", "app"]);
        let installed = BTreeMap::from([("lib".into(), "1.0".into())]);
        let plan = solve(&catalog, &installed, "app").unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].package, "app");
    }
    #[test]
    fn cycles_and_alternatives_fail_closed() {
        let cycles = BTreeMap::from([
            ("a".into(), item("a", "1", "b")),
            ("b".into(), item("b", "1", "a")),
        ]);
        assert!(solve(&cycles, &BTreeMap::new(), "a").is_err());
        let alt = BTreeMap::from([("a".into(), item("a", "1", "missing | b"))]);
        assert!(solve(&alt, &BTreeMap::new(), "a").is_err());
        assert!(parse_atom("libc6:any").is_err());
    }
    #[test]
    fn unique_available_alternative_is_planned_without_guessing() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "missing | lib")),
            ("lib".into(), item("lib", "2", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(), ["lib", "app"]);
        let ambiguous = BTreeMap::from([
            ("app".into(), item("app", "1", "lib | other")),
            ("lib".into(), item("lib", "2", "")),
            ("other".into(), item("other", "1", "")),
        ]);
        let chosen = solve(&ambiguous, &BTreeMap::new(), "app").unwrap();
        assert_eq!(chosen.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(), ["lib", "app"]);
    }
    #[test]
    fn alternative_selection_respects_debian_order_not_lexical_order() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "zed | alpha")),
            ("alpha".into(), item("alpha", "1", "")),
            ("zed".into(), item("zed", "1", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            ["zed", "app"]);
        // A currently installed alternative wins without forcing zed.
        let installed = BTreeMap::from([("alpha".into(), "1".into())]);
        let plan = solve(&catalog, &installed, "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(), ["app"]);
    }

    #[test]
    fn multi_root_later_dependency_can_force_fallback_choice() {
        let mut first = item("first", "1", "");
        first.conflicts = "second".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "first | fallback")),
            ("first".into(), first),
            ("fallback".into(), item("fallback", "1", "")),
            ("second".into(), item("second", "1", "")),
        ]);
        // Order roots with second first to make the preference conflict known.
        let keyed = test_keyed(&catalog);
        let plan = solve_catalog_many(&keyed, &BTreeMap::new(), &BTreeMap::new(),
            &["second".into(), "app".into()], 20, "amd64").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            ["second", "fallback", "app"]);
    }
    #[test]
    fn future_explicit_root_forces_fallback_even_when_requested_last() {
        let mut first = item("first", "1", "");
        first.conflicts = "blocker".into();
        let catalog = test_keyed(&BTreeMap::from([
            ("app".into(), item("app", "1", "first | fallback")),
            ("first".into(), first),
            ("fallback".into(), item("fallback", "1", "")),
            ("blocker".into(), item("blocker", "1", "")),
        ]));
        for roots in [
            vec!["app".into(), "blocker".into()],
            vec!["blocker".into(), "app".into()],
        ] {
            let plan = solve_catalog_many(&catalog, &BTreeMap::new(), &BTreeMap::new(),
                &roots, 20, "amd64").unwrap();
            let names = plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>();
            assert!(names.contains(&"app") && names.contains(&"blocker"));
            assert!(names.contains(&"fallback") && !names.contains(&"first"));
            assert_eq!(names.len(), 3);
        }
        // A single root still prefers its first declared dependency.
        let plan = solve_catalog_many(&catalog, &BTreeMap::new(), &BTreeMap::new(),
            &["app".into()], 20, "amd64").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            ["first", "app"]);
    }

    #[test]
    fn joint_root_lookahead_checks_reverse_breaks_and_version_constraints() {
        let mut blocker = item("blocker", "3", "");
        blocker.breaks = "first (>= 2)".into();
        let catalog = test_keyed(&BTreeMap::from([
            ("app".into(), item("app", "1", "first | fallback")),
            ("first".into(), item("first", "2", "")),
            ("fallback".into(), item("fallback", "1", "")),
            ("blocker".into(), blocker.clone()),
        ]));
        let roots = ["app".into(), "blocker".into()];
        let plan = solve_catalog_many(&catalog, &BTreeMap::new(), &BTreeMap::new(),
            &roots, 20, "amd64").unwrap();
        assert!(plan.iter().any(|step| step.package == "fallback"));
        blocker.breaks = "first (>> 2)".into();
        let mut harmless = catalog;
        harmless.insert(PackageKey { name: "blocker".into(), architecture: "amd64".into() }, blocker);
        let plan = solve_catalog_many(&harmless, &BTreeMap::new(), &BTreeMap::new(),
            &roots, 20, "amd64").unwrap();
        assert!(plan.iter().any(|step| step.package == "first"));
    }

    #[test]
    fn joint_root_lookahead_does_not_discard_roots_or_authorize_replaces() {
        let mut first = item("first", "1", "");
        first.replaces = "blocker".into();
        let mut fallback = item("fallback", "1", "");
        fallback.conflicts = "blocker".into();
        let catalog = test_keyed(&BTreeMap::from([
            ("app".into(), item("app", "1", "first | fallback")),
            ("first".into(), first),
            ("fallback".into(), fallback),
            ("blocker".into(), item("blocker", "1", "")),
        ]));
        assert!(solve_catalog_many(&catalog, &BTreeMap::new(), &BTreeMap::new(),
            &["app".into(), "blocker".into()], 20, "amd64").is_err());
    }

    #[test]
    fn backtracks_failed_branch_and_restores_full_partial_plan() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "bad | good")),
            ("bad".into(), item("bad", "1", "helper, missing")),
            ("helper".into(), item("helper", "1", "")),
            ("good".into(), item("good", "1", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(), ["good", "app"]);
    }
    #[test]
    fn recommendations_can_be_selected_without_changing_mandatory_default() {
        let mut app = item("app", "1", "lib");
        app.recommends = "extra".into();
        let mut catalog = test_keyed(&BTreeMap::from([
            ("app".into(), app),
            ("lib".into(), item("lib", "1", "")),
            ("extra".into(), item("extra", "1", "")),
        ]));
        let roots = &["app".to_owned()];
        let ordinary = solve_catalog_many_options(
            &catalog, &BTreeMap::new(), &BTreeMap::new(), roots, 30, "amd64", false,
        ).unwrap();
        assert_eq!(ordinary.iter().map(|s| s.package.as_str()).collect::<Vec<_>>(),
            ["lib", "app"]);
        let with_recommends = solve_catalog_many_options(
            &catalog, &BTreeMap::new(), &BTreeMap::new(), roots, 30, "amd64", true,
        ).unwrap();
        assert_eq!(with_recommends.iter().map(|s| s.package.as_str()).collect::<Vec<_>>(),
            ["lib", "app", "extra"]);
        catalog.remove(&PackageKey {name:"extra".into(),architecture:"amd64".into()});
        let absent = solve_catalog_many_options(
            &catalog, &BTreeMap::new(), &BTreeMap::new(), roots, 30, "amd64", true,
        ).unwrap();
        assert_eq!(absent.iter().map(|s| s.package.as_str()).collect::<Vec<_>>(),
            ["lib", "app"]);
    }

    #[test]
    fn recommendations_do_not_force_conflicting_or_broken_packages() {
        let mut app = item("app", "1", "");
        app.recommends = "badpkg | goodpkg, absentpkg".into();
        let mut bad = item("badpkg", "1", "");
        bad.conflicts = "app".into();
        let catalog = test_keyed(&BTreeMap::from([
            ("app".into(), app),
            ("badpkg".into(), bad),
            ("goodpkg".into(), item("goodpkg", "1", "")),
        ]));
        let result = solve_catalog_many_options(
            &catalog, &BTreeMap::new(), &BTreeMap::new(),
            &["app".into()], 40, "amd64", true,
        ).unwrap();
        assert_eq!(result.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            ["app", "goodpkg"]);
    }

    #[test]
    fn ordered_alternatives_choose_first_fully_viable_branch() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "alpha | beta")),
            ("alpha".into(), item("alpha", "1", "helper")),
            ("helper".into(), item("helper", "1", "")),
            ("beta".into(), item("beta", "1", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            ["helper", "alpha", "app"]);
    }
    #[test]
    fn backtracking_considers_remaining_dependency_groups() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "bad | good, helper")),
            ("bad".into(), item("bad", "1", "missing")),
            ("good".into(), item("good", "1", "")),
            ("helper".into(), item("helper", "1", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(), ["good", "helper", "app"]);
    }
    #[test]
    fn single_virtual_provider_with_explicit_version() {
        let mut provider = item("provider", "9.0", "");
        provider.provides = "virtual-api (= 2.1)".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "virtual-api (>= 2.0)")),
            ("provider".into(), provider),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(), ["provider", "app"]);
        let bad = BTreeMap::from([
            ("app".into(), item("app", "1", "virtual-api (>= 2.2)")),
            ("provider".into(), catalog["provider"].clone()),
        ]);
        assert!(solve(&bad, &BTreeMap::new(), "app").is_err());
        let mut unversioned = catalog["provider"].clone();
        unversioned.provides = "virtual-api".into();
        let bad = BTreeMap::from([
            ("app".into(), catalog["app"].clone()), ("provider".into(), unversioned),
        ]);
        assert!(solve(&bad, &BTreeMap::new(), "app").is_err());
    }
    #[test]
    fn ambiguous_virtual_providers_and_conflicting_provider_fail_closed() {
        let mut first = item("first", "1", "");
        first.provides = "service-api".into();
        let mut second = item("second", "1", "");
        second.provides = "service-api".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "service-api")),
            ("first".into(), first.clone()),
            ("second".into(), second),
        ]);
        assert!(solve(&catalog, &BTreeMap::new(), "app").is_err());
        first.conflicts = "old".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "service-api")),
            ("first".into(), first),
        ]);
        assert!(solve(&catalog, &BTreeMap::from([("old".into(), "1".into())]), "app").is_err());
    }
    #[test]
    fn allows_debian_epoch_in_versions_not_architecture_suffixes() {
        let requirement = parse_atom("old-lib (<< 2:3.0)").unwrap();
        assert!(satisfies("1:9.0", &requirement).unwrap());
        assert!(!satisfies("2:3.0", &requirement).unwrap());
        assert!(parse_atom("old-lib:any").is_err());
    }
    #[test]
    fn versioned_conflicts_and_breaks_match_only_existing_versions() {
        let mut app = item("app", "1", "");
        app.conflicts = "old-lib (<< 2.0)".into();
        let catalog = BTreeMap::from([("app".into(), app.clone())]);
        assert!(solve(&catalog, &BTreeMap::from([("old-lib".into(), "1.9".into())]), "app").is_err());
        assert!(solve(&catalog, &BTreeMap::from([("old-lib".into(), "2.1".into())]), "app").is_ok());
        app.breaks = "other (>= 3.0)".into();
        let catalog = BTreeMap::from([("app".into(), app)]);
        assert!(solve(&catalog, &BTreeMap::from([("other".into(), "3.0".into())]), "app").is_err());
    }
    #[test]
    fn conflict_with_previous_planned_dependency_is_rejected() {
        let mut app = item("app", "1", "lib");
        app.breaks = "lib".into();
        let catalog = BTreeMap::from([
            ("app".into(), app), ("lib".into(), item("lib", "1", "")),
        ]);
        assert!(solve(&catalog, &BTreeMap::new(), "app").is_err());
    }
    #[test]
    fn backtracking_skips_conflicting_alternative() {
        let mut bad = item("bad", "1", "");
        bad.conflicts = "already".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "bad | good")),
            ("bad".into(), bad), ("good".into(), item("good", "1", "")),
        ]);
        let installed = BTreeMap::from([("already".into(), "1".into())]);
        let plan = solve(&catalog, &installed, "app").unwrap();
        assert_eq!(plan.iter().map(|p|p.package.as_str()).collect::<Vec<_>>(), ["good", "app"]);
    }
    #[test]
    fn reverse_installed_breaks_and_conflicts_use_status_metadata() {
        let raw = "Package: legacy\nStatus: install ok installed\nArchitecture: amd64\nVersion: 3\nBreaks: app (>= 2)\n\n";
        let path = std::env::temp_dir().join(format!("rpt-conflicts-{}.status", std::process::id()));
        std::fs::write(&path, raw).unwrap();
        let installed_relations = read_installed_relations(&path, "amd64").unwrap();
        std::fs::remove_file(path).unwrap();
        let catalog = BTreeMap::from([("app".into(), item("app", "2", ""))]);
        let installed = BTreeMap::from([("legacy".into(), "3".into())]);
        let keyed = test_keyed(&catalog);
        let binary_installed = test_keyed_versions(&keyed, &installed, "amd64");
        let binary_relations = test_keyed_relations(&keyed, &installed_relations, "amd64");
        let mut solver = Solver {
            catalog: &keyed, installed: &binary_installed, installed_relations: &binary_relations,
            visiting: BTreeSet::new(), done: BTreeSet::new(),
            plan: Vec::new(), limit: 20, architecture: "amd64".into(), branch_budget: Rc::new(Cell::new(100)), include_recommends: false,
            requested_roots: BTreeSet::new(),
        };
        assert!(solver.resolve(&parse_atom("app").unwrap()).is_err());
    }
    #[test]
    fn invalid_conflict_syntax_is_not_ignored() {
        let mut app = item("app", "1", "");
        app.conflicts = "other:any".into();
        assert!(solve(&BTreeMap::from([("app".into(), app)]), &BTreeMap::new(), "app").is_err());
    }
    #[test]
    fn conflict_and_downgrade_fail_closed() {
        let mut p = item("pkg", "1", "");
        p.conflicts = "other".into();
        // A simple absent conflict does not need a package removal.
        assert!(solve(&BTreeMap::from([("pkg".into(), p.clone())]), &BTreeMap::new(), "pkg").is_ok());
        assert!(solve(&BTreeMap::from([("pkg".into(), p)]),
            &BTreeMap::from([("other".into(), "1".into())]), "pkg").is_err());
        let p = item("pkg", "1", "");
        assert!(solve(&BTreeMap::from([("pkg".into(), p)]),
            &BTreeMap::from([("pkg".into(), "2".into())]), "pkg").is_err());
    }
    #[test]
    fn installed_relationships_keep_foreign_and_native_separate() {
        let path = std::env::temp_dir().join(format!("rpt-relations-identity-{}", std::process::id()));
        std::fs::write(&path, "\
Package: libx\nVersion: 2\nArchitecture: amd64\nStatus: install ok installed\nConflicts: oldnative\n\n\
Package: libx\nVersion: 1\nArchitecture: i386\nStatus: hold ok installed\nConflicts: oldforeign\n\n\
Package: docs\nVersion: 3\nArchitecture: all\nStatus: install ok installed\n\n").unwrap();
        let all = read_installed_relations_by_arch(&path).unwrap();
        assert_eq!(all.len(), 3);
        assert_eq!(all[&cache_query::InstalledKey { name: "libx".into(), architecture: "amd64".into() }].conflicts, "oldnative");
        assert_eq!(all[&cache_query::InstalledKey { name: "libx".into(), architecture: "i386".into() }].conflicts, "oldforeign");
        assert!(all[&cache_query::InstalledKey { name: "libx".into(), architecture: "i386".into() }].held);
        let native = native_installed_relations(&all, "amd64").unwrap();
        assert_eq!(native.len(), 2);
        assert_eq!(native["libx"].version, "2");
        assert_eq!(native["docs"].version, "3");
        std::fs::remove_file(path).unwrap();
    }


    #[test]
    fn installed_relationships_reject_native_all_collision_and_duplicate_identity() {
        let path = std::env::temp_dir().join(format!("rpt-relations-duplicate-{}", std::process::id()));
        std::fs::write(&path, "\
Package: libx\nVersion: 2\nArchitecture: amd64\nStatus: install ok installed\n\n\
Package: libx\nVersion: 2\nArchitecture: all\nStatus: install ok installed\n\n").unwrap();
        let all = read_installed_relations_by_arch(&path).unwrap();
        assert_eq!(all.len(), 2);
        assert!(native_installed_relations(&all, "amd64").is_err());
        std::fs::write(&path, "\
Package: libx\nVersion: 2\nArchitecture: amd64\nStatus: install ok installed\n\n\
Package: libx\nVersion: 3\nArchitecture: amd64\nStatus: install ok installed\n\n").unwrap();
        assert!(read_installed_relations_by_arch(&path).is_err());
        std::fs::remove_file(path).unwrap();
    }


    #[test]
    fn foreign_reverse_and_forward_conflicts_breaks_are_checked() {
        let arch = "amd64";
        let app = item("app", "2", "");
        let key = PackageKey { name: "app".into(), architecture: arch.into() };
        let plan = [PlanStep { package: "app".into(), architecture: arch.into(),
            version: "2".into(), installed: None }];
        let mut catalog = BTreeMap::from([(key, app.clone())]);
        let other = PackageKey { name: "foreign-lib".into(), architecture: "i386".into() };
        let mut installed = BTreeMap::from([(cache_query::InstalledKey { name: other.name.clone(), architecture: other.architecture.clone() }, InstalledRelations {
            version: "1".into(), multi_arch: "no".into(), ..Default::default()
        })]);
        assert!(reject_foreign_negative_relations(&catalog, &installed, &plan, arch).is_ok());
        // A foreign package's installed reverse relationship is authoritative.
        installed.get_mut(&cache_query::InstalledKey { name: other.name.clone(), architecture: other.architecture.clone() }).unwrap().breaks = "app (>= 2)".into();
        assert!(reject_foreign_negative_relations(&catalog, &installed, &plan, arch).is_err());
        installed.get_mut(&cache_query::InstalledKey { name: other.name.clone(), architecture: other.architecture.clone() }).unwrap().breaks.clear();
        installed.get_mut(&cache_query::InstalledKey { name: other.name.clone(), architecture: other.architecture.clone() }).unwrap().conflicts = "app".into();
        assert!(reject_foreign_negative_relations(&catalog, &installed, &plan, arch).is_err());
        installed.get_mut(&cache_query::InstalledKey { name: other.name.clone(), architecture: other.architecture.clone() }).unwrap().conflicts.clear();

        // The staged native binary can itself conflict with an installed foreign name.
        catalog.get_mut(&PackageKey { name: "app".into(), architecture: arch.into() })
            .unwrap().conflicts = "foreign-lib (>= 1)".into();
        assert!(reject_foreign_negative_relations(&catalog, &installed, &plan, arch).is_err());
        catalog.get_mut(&PackageKey { name: "app".into(), architecture: arch.into() })
            .unwrap().conflicts.clear();
        catalog.get_mut(&PackageKey { name: "app".into(), architecture: arch.into() })
            .unwrap().breaks = "foreign-lib".into();
        assert!(reject_foreign_negative_relations(&catalog, &installed, &plan, arch).is_err());
        catalog.get_mut(&PackageKey { name: "app".into(), architecture: arch.into() })
            .unwrap().breaks.clear();

        // Versioned virtual Provides on the installed foreign binary are checked.
        installed.get_mut(&cache_query::InstalledKey { name: other.name.clone(), architecture: other.architecture.clone() }).unwrap().provides = "foreign-virtual (= 2)".into();
        catalog.get_mut(&PackageKey { name: "app".into(), architecture: arch.into() })
            .unwrap().conflicts = "foreign-virtual (>= 2)".into();
        assert!(reject_foreign_negative_relations(&catalog, &installed, &plan, arch).is_err());
    }


    #[test]
    fn installed_foreign_relations_remain_distinct_from_native() {
        let path = std::env::temp_dir().join(format!("rpt-foreign-relations-{}", std::process::id()));
        std::fs::write(&path, "\
Package: libdemo\nArchitecture: amd64\nVersion: 2\nStatus: install ok installed\n\n\
Package: libdemo\nArchitecture: i386\nVersion: 1\nStatus: hold ok installed\nConflicts: app (>= 3)\n\n\
Package: docs\nArchitecture: all\nVersion: 4\nStatus: install ok installed\n\n").unwrap();
        let keyed = read_installed_relations_by_arch(&path).unwrap();
        assert_eq!(keyed.len(), 3);
        let foreign = &keyed[&cache_query::InstalledKey { name: "libdemo".into(), architecture: "i386".into() }];
        assert!(foreign.held);
        assert_eq!(foreign.conflicts, "app (>= 3)");
        let native = read_installed_relations(&path, "amd64").unwrap();
        assert_eq!(native.len(), 2);
        assert_eq!(native["libdemo"].version, "2");
        std::fs::remove_file(path).unwrap();
    }


    #[test]
    fn multiarch_same_coinstallation_preflight_requires_matching_versions_and_metadata() {
        let mut native = item("libdemo", "2:1.0-1", "");
        native.multi_arch = "same".into();
        let mut foreign = native.clone();
        foreign.architecture = "i386".into();
        assert!(validate_multiarch_same_pair(&native, &foreign).is_ok());

        foreign.version = "2:1.0-2".into();
        assert!(validate_multiarch_same_pair(&native, &foreign).is_err());
        foreign.version = native.version.clone();

        foreign.multi_arch = "foreign".into();
        assert!(validate_multiarch_same_pair(&native, &foreign).is_err());
        foreign.multi_arch = "same".into();

        native.multi_arch = "allowed".into();
        assert!(validate_multiarch_same_pair(&native, &foreign).is_err());
        native.multi_arch = "same".into();

        foreign.architecture = "all".into();
        assert!(validate_multiarch_same_pair(&native, &foreign).is_err());
        foreign.architecture = "amd64".into();
        assert!(validate_multiarch_same_pair(&native, &foreign).is_err());
        foreign.architecture = "i386".into();
        foreign.name = "another".into();
        assert!(validate_multiarch_same_pair(&native, &foreign).is_err());
    }


    #[test]
    fn foreign_partner_lookup_uses_exact_architecture_and_matching_multiarch_version() {
        let mut native = item("libdemo", "1.0-1", "");
        native.multi_arch = "same".into();
        let mut foreign = native.clone();
        foreign.architecture = "i386".into();
        let key = PackageKey { name: "libdemo".into(), architecture: "i386".into() };
        let mut catalog = BTreeMap::from([(key.clone(), foreign.clone())]);
        assert_eq!(validated_foreign_partner(&catalog, &native, "i386").unwrap().architecture, "i386");
        assert!(validated_foreign_partner(&catalog, &native, "arm64").is_err());
        foreign.version = "1.0-2".into();
        catalog.insert(key.clone(), foreign.clone());
        assert!(validated_foreign_partner(&catalog, &native, "i386").is_err());
        foreign.version = native.version.clone();
        foreign.multi_arch = "foreign".into();
        catalog.insert(key, foreign);
        assert!(validated_foreign_partner(&catalog, &native, "i386").is_err());
    }


    #[test]
    fn multiarch_plan_gate_checks_all_staged_binary_identities() {
        let mut native = item("libdemo", "1", "");
        native.multi_arch = "same".into();
        let mut foreign = native.clone();
        foreign.architecture = "i386".into();
        let nk = PackageKey { name: "libdemo".into(), architecture: "amd64".into() };
        let fk = PackageKey { name: "libdemo".into(), architecture: "i386".into() };
        let mut catalog = BTreeMap::from([(nk, native), (fk.clone(), foreign)]);
        let steps = vec![
            PlanStep { package: "libdemo".into(), architecture: "amd64".into(), version: "1".into(), installed: None },
            PlanStep { package: "libdemo".into(), architecture: "i386".into(), version: "1".into(), installed: None },
        ];
        assert!(validate_multiarch_plan(&catalog, &steps).is_ok());
        let mut wrong = steps.clone();
        wrong[1].version = "2".into();
        assert!(validate_multiarch_plan(&catalog, &wrong).is_err());
        let mut duplicate = steps.clone();
        duplicate.push(steps[0].clone());
        assert!(validate_multiarch_plan(&catalog, &duplicate).is_err());
        catalog.get_mut(&fk).unwrap().multi_arch = "foreign".into();
        assert!(validate_multiarch_plan(&catalog, &steps).is_err());
        catalog.remove(&fk);
        assert!(validate_multiarch_plan(&catalog, &steps).is_err());
    }


    #[test]
    fn multiarch_plan_rejects_all_and_concrete_same_name_pair() {
        let native = item("libdemo", "1", "");
        let mut common = native.clone();
        common.architecture = "all".into();
        let catalog = BTreeMap::from([
            (PackageKey { name: "libdemo".into(), architecture: "amd64".into() }, native),
            (PackageKey { name: "libdemo".into(), architecture: "all".into() }, common),
        ]);
        let plan = vec![
            PlanStep { package: "libdemo".into(), architecture: "amd64".into(), version: "1".into(), installed: None },
            PlanStep { package: "libdemo".into(), architecture: "all".into(), version: "1".into(), installed: None },
        ];
        assert!(validate_multiarch_plan(&catalog, &plan).is_err());
    }


    #[test]
    fn multiarch_plan_rejects_unknown_declarations_before_returning_plan() {
        let mut pkg = item("libdemo", "1", "");
        pkg.multi_arch = "unexpected".into();
        let key = PackageKey { name: "libdemo".into(), architecture: "amd64".into() };
        let mut catalog = BTreeMap::from([(key.clone(), pkg)]);
        let plan = vec![PlanStep {
            package: "libdemo".into(), architecture: "amd64".into(),
            version: "1".into(), installed: None,
        }];
        assert!(validate_multiarch_plan(&catalog, &plan).is_err());
        for accepted in ["no", "same", "foreign", "allowed"] {
            catalog.get_mut(&key).unwrap().multi_arch = accepted.into();
            assert!(validate_multiarch_plan(&catalog, &plan).is_ok());
        }
    }


    #[test]
    fn foreign_installed_same_version_coinstallation_gate() {
        let mut native = item("libdemo", "1", "");
        native.multi_arch = "same".into();
        let key = PackageKey { name: "libdemo".into(), architecture: "amd64".into() };
        let mut catalog = BTreeMap::from([(key.clone(), native)]);
        let foreign_key = cache_query::InstalledKey {
            name: "libdemo".into(), architecture: "i386".into(),
        };
        let mut foreign = BTreeMap::from([(foreign_key.clone(), InstalledRelations {
            version: "1".into(), multi_arch: "same".into(), ..Default::default()
        })]);
        let plan = [PlanStep {
            package: "libdemo".into(), architecture: "amd64".into(),
            version: "1".into(), installed: None,
        }];
        assert!(reject_foreign_negative_relations(&catalog, &foreign, &plan, "amd64").is_ok());
        foreign.get_mut(&foreign_key).unwrap().version = "2".into();
        assert!(reject_foreign_negative_relations(&catalog, &foreign, &plan, "amd64").is_err());
        foreign.get_mut(&foreign_key).unwrap().version = "1".into();
        foreign.get_mut(&foreign_key).unwrap().multi_arch = "no".into();
        assert!(reject_foreign_negative_relations(&catalog, &foreign, &plan, "amd64").is_err());
        foreign.get_mut(&foreign_key).unwrap().multi_arch = "same".into();
        catalog.get_mut(&key).unwrap().multi_arch = "allowed".into();
        assert!(reject_foreign_negative_relations(&catalog, &foreign, &plan, "amd64").is_err());
    }


    #[test]
    fn installed_multiarch_metadata_rejects_inconsistent_coinstalled_pairs() {
        let a = cache_query::InstalledKey { name: "libdemo".into(), architecture: "amd64".into() };
        let b = cache_query::InstalledKey { name: "libdemo".into(), architecture: "i386".into() };
        let installed = BTreeMap::from([
            (a.clone(), cache_query::Installed { version: "1".into(), architecture: "amd64".into(), status: "install ok installed".into() }),
            (b.clone(), cache_query::Installed { version: "1".into(), architecture: "i386".into(), status: "install ok installed".into() }),
        ]);
        let mut relations = BTreeMap::from([
            (a.clone(), InstalledRelations { version: "1".into(), multi_arch: "same".into(), ..Default::default() }),
            (b.clone(), InstalledRelations { version: "1".into(), multi_arch: "same".into(), ..Default::default() }),
        ]);
        assert!(validate_installed_metadata(&installed, &relations).is_ok());
        relations.get_mut(&b).unwrap().multi_arch = "foreign".into();
        assert!(validate_installed_metadata(&installed, &relations).is_err());
        relations.get_mut(&b).unwrap().multi_arch = "same".into();
        relations.get_mut(&b).unwrap().version = "2".into();
        assert!(validate_installed_metadata(&installed, &relations).is_err());
        relations.get_mut(&b).unwrap().version = "1".into();
        relations.get_mut(&b).unwrap().multi_arch = "unknown".into();
        assert!(validate_installed_metadata(&installed, &relations).is_err());
    }


    #[test]
    fn explicit_foreign_dependency_selects_exact_binary_identity() {
        let app = item("app", "1", "libdemo:i386 (>= 2)");
        let mut foreign = item("libdemo", "2", "");
        foreign.architecture = "i386".into();
        foreign.multi_arch = "same".into();
        let catalog = BTreeMap::from([
            (PackageKey { name: "app".into(), architecture: "amd64".into() }, app),
            (PackageKey { name: "libdemo".into(), architecture: "i386".into() }, foreign),
        ]);
        let plan = solve_catalog(&catalog, &BTreeMap::new(), &BTreeMap::new(), "app", 20, "amd64").unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].architecture, "i386");
        assert_eq!(plan[0].package, "libdemo");
        let mut mismatched = catalog.clone();
        let foreign_key = PackageKey { name: "libdemo".into(), architecture: "i386".into() };
        mismatched.get_mut(&foreign_key).unwrap().version = "1".into();
        assert!(solve_catalog(&mismatched, &BTreeMap::new(), &BTreeMap::new(),
            "app", 20, "amd64").is_err());
        assert!(solve_catalog(&catalog, &BTreeMap::new(), &BTreeMap::new(),
            "libdemo:armhf", 20, "amd64").is_err());
    }

    #[test]
    fn architecture_qualifiers_reject_invalid_debian_architecture_names() {
        for valid in ["pkg:amd64", "pkg:arm64", "pkg:i386", "pkg:hurd-i386", "pkg:native", "pkg:any"] {
            assert!(parse_atom_for_arch(valid, "amd64").is_ok(), "expected valid: {valid}");
        }
        for invalid in ["pkg:", "pkg:-amd64", "pkg:AMD64", "pkg:_amd64", "pkg:arm_64",
                        "pkg:arm.64", "pkg:arm/64", "pkg:arm64:amd64"] {
            assert!(parse_atom_for_arch(invalid, "amd64").is_err(), "expected invalid: {invalid}");
        }
    }

    #[test]
    fn dependency_names_follow_debian_binary_package_rules() {
        for valid in ["libc6", "python3-apt", "g++", "a.b", "0ad"] {
            assert!(parse_atom(valid).is_ok(), "expected valid: {valid}");
        }
        for invalid in ["a", "Apt", "-apt", ".apt", "+apt", "apt_name", "apt/name", "apt name"] {
            assert!(parse_atom(invalid).is_err(), "expected invalid: {invalid}");
            assert!(parse_atom_for_arch(&format!("{invalid}:native"), "amd64").is_err());
        }
    }



    #[test]
    fn alternatives_reject_unsatisfied_version_constraints() {
        let catalog = BTreeMap::from([
            (PackageKey { name: "app".into(), architecture: "amd64".into() },
                item("app", "1", "libfirst (>= 3) | libsecond (>= 4)")),
            (PackageKey { name: "libfirst".into(), architecture: "amd64".into() },
                item("libfirst", "2", "")),
            (PackageKey { name: "libsecond".into(), architecture: "amd64".into() },
                item("libsecond", "3", "")),
        ]);
        assert!(solve_catalog(&catalog, &BTreeMap::new(), &BTreeMap::new(),
            "app", 20, "amd64").is_err());
    }

    #[test]
    fn alternatives_reject_missing_providers_without_partial_plan() {
        let catalog = BTreeMap::from([(
            PackageKey { name: "app".into(), architecture: "amd64".into() },
            item("app", "1", "missing-one | missing-two"),
        )]);
        assert!(solve_catalog(&catalog, &BTreeMap::new(), &BTreeMap::new(),
            "app", 20, "amd64").is_err());
    }

    #[test]
    fn alternatives_preserve_versioned_installed_satisfaction() {
        let catalog = BTreeMap::from([
            (PackageKey { name: "app".into(), architecture: "amd64".into() },
                item("app", "1", "libfirst (>= 2) | libsecond")),
            (PackageKey { name: "libfirst".into(), architecture: "amd64".into() },
                item("libfirst", "2", "")),
            (PackageKey { name: "libsecond".into(), architecture: "amd64".into() },
                item("libsecond", "1", "")),
        ]);
        let installed = BTreeMap::from([(
            PackageKey { name: "libfirst".into(), architecture: "amd64".into() },
            "2".into(),
        )]);
        let plan = solve_catalog(&catalog, &installed, &BTreeMap::new(),
            "app", 20, "amd64").unwrap();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].package, "app");
    }

    #[test]
    fn nested_backtracking_discards_transitive_failed_branch() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "bad | good")),
            ("bad".into(), item("bad", "1", "transitive")),
            ("transitive".into(), item("transitive", "1", "missing")),
            ("good".into(), item("good", "1", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            ["good", "app"]);
    }

    #[test]
    fn backtracking_does_not_resurrect_failed_branch_with_shared_dependency() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "bad | good, shared")),
            ("bad".into(), item("bad", "1", "shared, missing")),
            ("good".into(), item("good", "1", "shared")),
            ("shared".into(), item("shared", "1", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        let names = plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>();
        assert_eq!(names, ["shared", "good", "app"]);
    }

    #[test]
    fn virtual_provider_version_is_not_binary_package_version() {
        let mut provider = item("provider", "99", "");
        provider.provides = "virtual-api (= 2)".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "virtual-api (>= 3)")),
            ("provider".into(), provider),
        ]);
        assert!(solve(&catalog, &BTreeMap::new(), "app").is_err());
    }

    #[test]
    fn virtual_provider_version_constraint_accepts_matching_provided_version() {
        let mut provider = item("provider", "1", "");
        provider.provides = "virtual-api (= 3)".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "virtual-api (>= 2)")),
            ("provider".into(), provider),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            ["provider", "app"]);
    }

    #[test]
    fn resolver_alternative_and_conflict_matrix() {
        struct Case {
            dependency: &'static str,
            bad_conflict: &'static str,
            good_version: &'static str,
            expected: &'static [&'static str],
        }
        for case in [
            Case { dependency: "bad | good", bad_conflict: "legacy", good_version: "1", expected: &["good", "app"] },
            Case { dependency: "bad (>= 2) | good", bad_conflict: "", good_version: "1", expected: &["good", "app"] },
            Case { dependency: "bad | good (>= 2)", bad_conflict: "legacy", good_version: "1", expected: &[] },
            Case { dependency: "bad | good (>= 2)", bad_conflict: "legacy", good_version: "2", expected: &["good", "app"] },
        ] {
            let mut bad = item("bad", "1", "");
            bad.conflicts = case.bad_conflict.into();
            let catalog = BTreeMap::from([
                ("app".into(), item("app", "1", case.dependency)),
                ("bad".into(), bad),
                ("good".into(), item("good", case.good_version, "")),
            ]);
            let installed = BTreeMap::from([("legacy".into(), "1".into())]);
            let result = solve(&catalog, &installed, "app");
            if case.expected.is_empty() {
                assert!(result.is_err(), "expected refusal for {}", case.dependency);
            } else {
                let plan = result.unwrap();
                assert_eq!(
                    plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
                    case.expected,
                    "dependency: {}", case.dependency,
                );
            }
        }
    }

    #[test]
    fn resolver_versioned_provides_matrix() {
        for (provided, required, accepted) in [
            ("virtual-api (= 3)", "virtual-api (>= 2)", true),
            ("virtual-api (= 2)", "virtual-api (>= 3)", false),
            ("virtual-api (= 3)", "virtual-api (= 3)", true),
            ("virtual-api (= 3)", "virtual-api (>> 3)", false),
            ("virtual-api (= 3)", "virtual-api (<< 4)", true),
        ] {
            let mut provider = item("provider", "99", "");
            provider.provides = provided.into();
            let catalog = BTreeMap::from([
                ("app".into(), item("app", "1", required)),
                ("provider".into(), provider),
            ]);
            let result = solve(&catalog, &BTreeMap::new(), "app");
            assert_eq!(result.is_ok(), accepted, "provided={provided}, required={required}");
            if accepted {
                assert_eq!(
                    result.unwrap().iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
                    ["provider", "app"],
                );
            }
        }
    }

    #[test]
    fn resolver_predepends_and_cycles_fail_closed_or_ordered() {
        let mut app = item("app", "1", "");
        app.pre_depends = "foundation (>= 2)".into();
        let catalog = BTreeMap::from([
            ("app".into(), app.clone()),
            ("foundation".into(), item("foundation", "2", "")),
        ]);
        let plan = solve(&catalog, &BTreeMap::new(), "app").unwrap();
        assert_eq!(
            plan.iter().map(|p| p.package.as_str()).collect::<Vec<_>>(),
            ["foundation", "app"],
        );
        let mut too_old = catalog.clone();
        too_old.insert("foundation".into(), item("foundation", "1", ""));
        assert!(solve(&too_old, &BTreeMap::new(), "app").is_err());
        // A Depends-only cycle has a finite read-only package set.
        let depends_cycle = BTreeMap::from([
            ("app".into(), item("app", "1", "foundation")),
            ("foundation".into(), item("foundation", "2", "app")),
        ]);
        let cycle_plan = solve(&depends_cycle, &BTreeMap::new(), "app").unwrap();
        assert_eq!(cycle_plan.len(), 2);
        // A direct Pre-Depends back-edge still fails closed: unpack/configure
        // ordering cannot be inferred from a package-set preview.
        let mut pre_cycle = catalog;
        let mut foundation = item("foundation", "2", "");
        foundation.pre_depends = "app".into();
        pre_cycle.insert("foundation".into(), foundation);
        assert!(solve(&pre_cycle, &BTreeMap::new(), "app").is_err());
    }

    #[test]
    fn multiple_equally_valid_versioned_virtual_providers_fail_closed() {
        let mut first = item("first", "1", "");
        first.provides = "virtual-api (= 3)".into();
        let mut second = item("second", "2", "");
        second.provides = "virtual-api (= 4)".into();
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "virtual-api (>= 2)")),
            ("first".into(), first),
            ("second".into(), second),
        ]);
        assert!(solve(&catalog, &BTreeMap::new(), "app").is_err());
    }
    #[test]
    fn joint_roots_share_dependencies_without_duplicates() {
        let catalog = BTreeMap::from([
            ("appone".into(), item("appone", "1", "shared")),
            ("apptwo".into(), item("apptwo", "1", "shared")),
            ("shared".into(), item("shared", "1", "")),
        ]);
        let keyed = catalog.into_iter().map(|(name, value)| (
            PackageKey { name, architecture: "amd64".into() }, value
        )).collect();
        let roots = vec!["appone".into(), "apptwo".into()];
        let plan = solve_catalog_many(&keyed, &BTreeMap::new(), &BTreeMap::new(),
            &roots, 10, "amd64").unwrap();
        assert_eq!(plan.iter().map(|s| s.package.as_str()).collect::<Vec<_>>(),
            ["shared", "appone", "apptwo"]);
    }

    #[test]
    fn joint_roots_reject_mutually_conflicting_packages() {
        let mut left = item("leftpkg", "1", "");
        left.conflicts = "rightpkg".into();
        let keyed = BTreeMap::from([
            (PackageKey { name: "leftpkg".into(), architecture: "amd64".into() }, left),
            (PackageKey { name: "rightpkg".into(), architecture: "amd64".into() }, item("rightpkg", "1", "")),
        ]);
        let roots = vec!["leftpkg".into(), "rightpkg".into()];
        assert!(solve_catalog_many(&keyed, &BTreeMap::new(), &BTreeMap::new(),
            &roots, 10, "amd64").is_err());
    }

}

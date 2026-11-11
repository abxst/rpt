//! Fail-closed dependency planning preview. NEVER executes dpkg or mutates packages.
//! This is not APT's full dependency resolver: only uniquely resolvable
//! alternatives and virtual Provides, plus bounded alternative backtracking;
//! no architecture qualifiers, pinning, package holds, triggers or removals.
use crate::{cache_query, metadata_query::{self, PackageKey, PackageSummary}, version};
use anyhow::{bail, Context, Result};
use std::{cell::Cell, cmp::Ordering, collections::{BTreeMap, BTreeSet}, path::Path, rc::Rc};

#[derive(Debug, Clone, PartialEq, Eq)]
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
    // Debian binary package names contain at least two characters, begin
    // with an ASCII lowercase letter or digit, and contain only lowercase
    // letters, digits, '+', '-' and '.'. Reject invalid metadata before
    // dependency resolution instead of accepting names dpkg cannot install.
    let valid_name = name.len() >= 2
        && name.as_bytes().first().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && name.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()
            || matches!(c, b'+' | b'.' | b'-'));
    if !valid_name {
        bail!("unsupported dependency name: {name}");
    }
    if let Some((_, required)) = &constraint { version::compare(required, required)?; }
    Ok(Requirement { name: name.to_owned(), constraint, any_arch: false })
}

/// Support only native-architecture dependency qualifiers. Foreign-arch
/// package co-installation, :any and architecture restrictions require true
/// Multi-Arch metadata and are deliberately refused.
fn parse_atom_for_arch(text: &str, arch: &str) -> Result<Requirement> {
    let atom = text.trim();
    let (head, tail) = atom.split_once('(').map_or((atom, None), |(h, t)| (h, Some(t)));
    let head = head.trim();
    let normalized = if let Some((name, qualifier)) = head.split_once(':') {
        if qualifier != "native" && qualifier != arch && qualifier != "any" {
            bail!("unsupported foreign or ambiguous architecture qualifier: {qualifier}");
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
    Ok(requirement)
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
    }
    for key in relations.keys() {
        if !installed.contains_key(key) {
            bail!("installed dpkg safety metadata has unknown binary identity");
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
        self.resolve_concrete(&concrete, &Requirement {
            name: concrete.name.clone(), constraint: None, any_arch: false,
        })
    }

    /// Explore a branch with a copy of the entire partial plan. Commit only
    /// if exactly one fully successful continuation remains. A failing branch
    /// must never leak its already-added dependencies into the winning branch.
    fn resolve_groups(&mut self, groups: &[Vec<Requirement>], index: usize) -> Result<()> {
        if index == groups.len() { return Ok(()); }
        let group = &groups[index];
        // Preserve the established behavior: an already installed direct
        // alternative satisfies a dependency without changing package state.
        for alternative in group {
            if let Some((installed_key, installed)) = self.installed_for_name(&alternative.name)? {
                let candidate_key = self.key_for(&alternative.name)?;
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
        let mut options = BTreeSet::new();
        for alternative in group {
            if let Some(concrete) = self.concrete(alternative)? {
                options.insert(concrete);
            }
        }
        if options.is_empty() {
            bail!("no resolvable alternatives for dependency group");
        }
        let mut winner: Option<Self> = None;
        for option in options {
            let remaining = self.branch_budget.get();
            if remaining == 0 { bail!("dependency branch budget exhausted"); }
            self.branch_budget.set(remaining - 1);
            let mut branch = self.clone();
            let requirement = Requirement { name: option.name.clone(), constraint: None, any_arch: false };
            // Only branch-local resolution errors are candidates for retry.
            if branch.resolve_concrete(&option, &requirement).is_ok()
                && branch.resolve_groups(groups, index + 1).is_ok() {
                if winner.is_some() {
                    bail!("multiple viable dependency alternatives; full APT solver required");
                }
                winner = Some(branch);
            }
        }
        let selected = winner.context("no viable dependency alternative after backtracking")?;
        self.visiting = selected.visiting;
        self.done = selected.done;
        self.plan = selected.plan;
        Ok(())
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
            // APT can resolve cycles, but this preview refuses them.
            bail!("dependency cycle involving {}", requested.name);
        }
        let current = self.catalog.get(key)
            .with_context(|| format!("unresolved dependency {} (virtual packages unsupported)", requested.name))?;
        if !satisfies(&current.version, requested)? {
            bail!("repository version {} does not satisfy dependency {}", current.version, requested.name);
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
        let groups = parse_groups_for_arch(&current.pre_depends, &self.architecture)?.into_iter()
            .chain(parse_groups_for_arch(&current.depends, &self.architecture)?).collect::<Vec<_>>();
        self.resolve_groups(&groups, 0)?;
        self.visiting.remove(key);
        self.done.insert(key.clone());
        let installed_identity = self.installed_for_name(&requested.name)?;
        let installed = installed_identity.map(|(_, version)| version.clone());
        if let Some((installed_key, _)) = installed_identity {
            if installed_key != key {
                bail!("installed binary identity differs from proposed {}:{}", key.name, key.architecture);
            }
        }
        if installed.as_deref() != Some(current.version.as_str()) {
            if let Some(existing) = &installed {
                if let Some(relation) = installed_identity.and_then(|(identity, _)| self.installed_relations.get(identity)) {
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
                package: requested.name.clone(), architecture: key.architecture.clone(),
                version: current.version.clone(), installed,
            });
            self.check_negative_relations()?;
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
            if key.name == step.package {
                bail!("planned {} overlaps installed foreign architecture {}; full Multi-Arch solver required",
                    key.name, key.architecture);
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
    reject_foreign_identity_overlap(&complete, &plan, architecture)?;
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
    let mut solver = Solver {
        catalog, installed, installed_relations, visiting: BTreeSet::new(),
        done: BTreeSet::new(), plan: Vec::new(), limit: max_packages,
        architecture: architecture.to_owned(),
        branch_budget: Rc::new(Cell::new(4096)),
    };
    solver.resolve(&parse_atom_for_arch(package, architecture)?)?;
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
    if max_packages == 0 || max_packages > 5000 {
        bail!("max_packages must be 1..=5000");
    }
    let target_release = crate::policy::read_default_release(apt_root)?;
    let preferences = crate::apt_preferences::load(apt_root)?;
    crate::apt_pin_eval::evaluate_with_phasing(
        package, &[], &preferences, target_release.as_deref(), None,
    )?;
    let complete = cache_query::installed_catalog_by_arch(status)?;
    let complete_relations = read_installed_relations_by_arch(status)?;
    validate_installed_metadata(&complete, &complete_relations)?;
    let installed = scoped_installed(&complete, architecture)?;
    let installed_relations = scoped_relations(&complete_relations, architecture)?;
    let mut catalog = metadata_query::catalog_by_arch(cache, architecture, 1_000_000)?;
    // A package's installed version is not sufficient evidence that its
    // dependency metadata is available from a signed repository occurrence.
    let mut validated = BTreeSet::<String>::new();
    let mut roots = BTreeSet::new();
    roots.insert(package.to_owned());
    for _ in 0..16 {
        let mut changed = false;
        for name in &roots {
            if validated.contains(name) { continue; }
            let origins = crate::package_provenance::verified_version_origins(cache, name, architecture)?;
            let chosen = crate::apt_pin_eval::preview_candidate_with_phasing(
                name, &origins, &preferences, installed.iter().find(|(key, _)| key.name == *name).map(|(_, version)| version.as_str()),
                target_release.as_deref(), None,
            )?.candidate.with_context(|| format!("APT policy has no candidate for {name}"))?;
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
            let key = metadata_query::native_key_for(&catalog, name, architecture)?;
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
            if roots.insert(name) { discovered = true; }
        }
        if discovered { continue; }
        let plan = solve_catalog(&catalog, &installed, &installed_relations, package, max_packages, architecture)?;
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
            reject_foreign_identity_overlap(&complete, &plan, architecture)?;
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
            plan: Vec::new(), limit: 20, architecture: "amd64".into(), branch_budget: Rc::new(Cell::new(4096)),
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
            InstalledRelations { version: "1".into(), ..Default::default() },
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
Package: lib\nArchitecture: amd64\nVersion: 2\nStatus: install ok installed\n\n\
Package: lib\nArchitecture: i386\nVersion: 1\nStatus: hold ok installed\nEssential: yes\n\n").unwrap();
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
            assert!(parse_atom_for_arch(&format!("interpreter:{foreign}"), arch).is_err());
        }
    }
    #[test]
    fn native_qualifiers_accept_current_arch_and_reject_foreign_or_any() {
        for arch in ["amd64", "arm64"] {
            assert_eq!(parse_atom_for_arch("lib:native (>= 1:2.0)", arch).unwrap().name, "lib");
            assert_eq!(parse_atom_for_arch(&format!("lib:{arch}"), arch).unwrap().name, "lib");
            assert!(parse_atom_for_arch("lib:any", arch).unwrap().any_arch);
            let foreign = if arch == "arm64" { "amd64" } else { "arm64" };
            assert!(parse_atom_for_arch(&format!("lib:{foreign}"), arch).is_err());
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
        assert!(solve(&ambiguous, &BTreeMap::new(), "app").is_err());
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
    fn rejects_two_fully_viable_branches_even_after_lookahead() {
        let catalog = BTreeMap::from([
            ("app".into(), item("app", "1", "a | b")),
            ("a".into(), item("a", "1", "helper")),
            ("helper".into(), item("helper", "1", "")),
            ("b".into(), item("b", "1", "")),
        ]);
        assert!(solve(&catalog, &BTreeMap::new(), "app").is_err());
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
            plan: Vec::new(), limit: 20, architecture: "amd64".into(), branch_budget: Rc::new(Cell::new(100)),
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
            version: "1".into(), ..Default::default()
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
    fn dependency_names_follow_debian_binary_package_rules() {
        for valid in ["libc6", "python3-apt", "g++", "a.b", "0ad"] {
            assert!(parse_atom(valid).is_ok(), "expected valid: {valid}");
        }
        for invalid in ["a", "Apt", "-apt", ".apt", "+apt", "apt_name", "apt/name", "apt name"] {
            assert!(parse_atom(invalid).is_err(), "expected invalid: {invalid}");
            assert!(parse_atom_for_arch(&format!("{invalid}:native"), "amd64").is_err());
        }
    }


}

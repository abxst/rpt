//! Fail-closed dependency planning preview. NEVER executes dpkg or mutates packages.
//! This is not APT's full dependency resolver: only uniquely resolvable
//! alternatives and virtual Provides, plus bounded alternative backtracking;
//! no architecture qualifiers, pinning, package holds, triggers or removals.
use crate::{cache_query, metadata_query::{self, PackageSummary}, version};
use anyhow::{bail, Context, Result};
use std::{cell::Cell, cmp::Ordering, collections::{BTreeMap, BTreeSet}, path::Path, rc::Rc};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanStep {
    pub package: String,
    pub version: String,
    pub installed: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Requirement {
    name: String,
    constraint: Option<(String, String)>,
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
    if name.is_empty() || !name.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'+' | b'.' | b'-')) {
        bail!("unsupported dependency name: {name}");
    }
    if let Some((_, required)) = &constraint { version::compare(required, required)?; }
    Ok(Requirement { name: name.to_owned(), constraint })
}

fn parse_groups(raw: &str) -> Result<Vec<Vec<Requirement>>> {
    if raw.trim().is_empty() { return Ok(Vec::new()); }
    raw.split(',').map(|group| {
        group.split('|').map(parse_atom).collect::<Result<Vec<_>>>()
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
    provides: String,
}

/// Read conflict metadata from the *installed* dpkg status, never infer it
/// from the latest repository version. Dpkg status is local, not signed policy.
fn read_installed_relations(status: &Path, arch: &str) -> Result<BTreeMap<String, InstalledRelations>> {
    let data = std::fs::read(status)?;
    if data.len() > 64 * 1024 * 1024 { bail!("dpkg status file exceeds safe size"); }
    let text = std::str::from_utf8(&data)?;
    let mut result = BTreeMap::new();
    for record in crate::package_index::parse(text)? {
        if record.field("Status") != Some("install ok installed") { continue; }
        if !matches!(record.field("Architecture"), Some(a) if a == arch || a == "all") { continue; }
        let name = record.field("Package").context("installed status missing Package")?;
        let version = record.field("Version").context("installed status missing Version")?;
        parse_atom(name)?;
        version::compare(version, version)?;
        let entry = InstalledRelations {
            version: version.into(),
            conflicts: record.field("Conflicts").unwrap_or("").into(),
            breaks: record.field("Breaks").unwrap_or("").into(),
            provides: record.field("Provides").unwrap_or("").into(),
        };
        if result.insert(name.to_owned(), entry).is_some() {
            bail!("duplicate installed package in dpkg status: {name}");
        }
    }
    Ok(result)
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
            architecture: String::new(), description: String::new(),
            section: String::new(), depends: String::new(),
            pre_depends: String::new(), recommends: String::new(),
            suggests: String::new(), provides: provided.into(),
            conflicts: String::new(), breaks: String::new(),
        };
        for cap in provides(&advertised)? {
            if capability_satisfies(&cap, &requirement)? { return Ok(true); }
        }
    }
    Ok(false)
}

#[derive(Clone)]
struct Solver<'a> {
    catalog: &'a BTreeMap<String, PackageSummary>,
    installed: &'a BTreeMap<String, String>,
    installed_relations: &'a BTreeMap<String, InstalledRelations>,
    visiting: BTreeSet<String>,
    done: BTreeSet<String>,
    plan: Vec<PlanStep>,
    limit: usize,
    branch_budget: Rc<Cell<usize>>,
}

impl Solver<'_> {
    /// Return the concrete package to install only when there is exactly one
    /// verified catalog candidate. No automatic preference between multiple
    /// virtual providers or mutually satisfiable alternatives.
    fn concrete(&self, required: &Requirement) -> Result<Option<String>> {
        if let Some(real) = self.catalog.get(&required.name) {
            return Ok(if satisfies(&real.version, required)? {
                Some(required.name.clone())
            } else { None });
        }
        let mut candidates = Vec::new();
        for (name, item) in self.catalog {
            let caps = provides(item)?;
            let mut is_provider = false;
            for cap in &caps {
                if capability_satisfies(cap, required)? {
                    is_provider = true;
                    break;
                }
            }
            if is_provider {
                candidates.push(name.clone());
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
        let selected = Requirement { name: concrete.clone(), constraint: None };
        self.resolve_concrete(&selected)
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
            if let Some(installed) = self.installed.get(&alternative.name) {
                if satisfies(installed, alternative)? {
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
            let requirement = Requirement { name: option, constraint: None };
            // Only branch-local resolution errors are candidates for retry.
            if branch.resolve_concrete(&requirement).is_ok()
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
        let staged: BTreeMap<&str, &str> = self.plan.iter()
            .map(|step| (step.package.as_str(), step.version.as_str())).collect();
        for (new_name, new_version) in &staged {
            let new_pkg = self.catalog.get(*new_name)
                .context("staged package missing repository metadata")?;
            for (installed_name, installed_version) in self.installed {
                if installed_name == *new_name { continue; }
                let effective_version = staged.get(installed_name.as_str())
                    .copied().unwrap_or(installed_version);
                let (conflicts, breaks, provider) =
                    if let Some(pkg) = self.catalog.get(installed_name)
                        .filter(|_| staged.contains_key(installed_name.as_str())) {
                        (pkg.conflicts.as_str(), pkg.breaks.as_str(), pkg.provides.as_str())
                    } else if let Some(status) = self.installed_relations.get(installed_name) {
                        if status.version != *installed_version {
                            bail!("inconsistent dpkg installed metadata for {installed_name}");
                        }
                        (status.conflicts.as_str(), status.breaks.as_str(), status.provides.as_str())
                    } else {
                        // Tests and legacy callers may lack installed relation
                        // metadata. Never infer unknown reverse relationships.
                        ("", "", "")
                    };
                if relations_hit(&new_pkg.conflicts, installed_name, effective_version, provider)?
                    || relations_hit(&new_pkg.breaks, installed_name, effective_version, provider)?
                    || relations_hit(conflicts, new_name, new_version, &new_pkg.provides)?
                    || relations_hit(breaks, new_name, new_version, &new_pkg.provides)? {
                    bail!("Conflicts/Breaks between planned {new_name} and installed {installed_name}");
                }
            }
            for (peer_name, peer_version) in &staged {
                if peer_name == new_name { continue; }
                let peer = self.catalog.get(*peer_name)
                    .context("staged peer missing repository metadata")?;
                if relations_hit(&new_pkg.conflicts, peer_name, peer_version, &peer.provides)?
                    || relations_hit(&new_pkg.breaks, peer_name, peer_version, &peer.provides)?
                    || relations_hit(&peer.conflicts, new_name, new_version, &new_pkg.provides)?
                    || relations_hit(&peer.breaks, new_name, new_version, &new_pkg.provides)? {
                    bail!("Conflicts/Breaks between planned {new_name} and {peer_name}");
                }
            }
        }
        Ok(())
    }

    fn resolve_concrete(&mut self, requested: &Requirement) -> Result<()> {
        if self.done.contains(&requested.name) {
            let current = self.catalog.get(&requested.name)
                .context("already resolved package missing from catalog")?;
            if !satisfies(&current.version, requested)? {
                bail!("conflicting version requirements for {}", requested.name);
            }
            return Ok(());
        }
        if self.visiting.contains(&requested.name) {
            // APT can resolve cycles, but this preview refuses them.
            bail!("dependency cycle involving {}", requested.name);
        }
        let current = self.catalog.get(&requested.name)
            .with_context(|| format!("unresolved dependency {} (virtual packages unsupported)", requested.name))?;
        if !satisfies(&current.version, requested)? {
            bail!("repository version {} does not satisfy dependency {}", current.version, requested.name);
        }
        // Fail closed on unsupported Conflicts/Breaks syntax even if the
        // named conflicting package is not presently installed.
        negative_relations(&current.conflicts)?;
        negative_relations(&current.breaks)?;
        if self.plan.len() + self.visiting.len() >= self.limit {
            bail!("dependency plan exceeded maximum package count");
        }
        self.visiting.insert(requested.name.clone());
        let groups = parse_groups(&current.pre_depends)?.into_iter()
            .chain(parse_groups(&current.depends)?).collect::<Vec<_>>();
        self.resolve_groups(&groups, 0)?;
        self.visiting.remove(&requested.name);
        self.done.insert(requested.name.clone());
        let installed = self.installed.get(&requested.name).cloned();
        if installed.as_deref() != Some(current.version.as_str()) {
            // Default priorities do not permit a downgrade.
            if let Some(existing) = &installed {
                if version::compare(existing, &current.version)? == Ordering::Greater {
                    bail!("would downgrade {}; full APT policy is required", current.name);
                }
            }
            self.plan.push(PlanStep {
                package: requested.name.clone(), version: current.version.clone(), installed,
            });
            self.check_negative_relations()?;
        }
        Ok(())
    }
}

/// Read installed packages in bulk; this deliberately does not trust the
/// resolver for actual package transactions.
pub fn preview(cache: &Path, status: &Path, package: &str, architecture: &str, max_packages: usize) -> Result<Vec<PlanStep>> {
    if max_packages == 0 || max_packages > 5000 { bail!("max_packages must be 1..=5000"); }
    let catalog = metadata_query::catalog(cache, architecture, 1_000_000)?;
    let installed = cache_query::installed_catalog(status, architecture)?;
    let installed_relations = read_installed_relations(status, architecture)?;
    let mut solver = Solver {
        catalog: &catalog, installed: &installed, installed_relations: &installed_relations,
        visiting: BTreeSet::new(),
        done: BTreeSet::new(), plan: Vec::new(), limit: max_packages,
        branch_budget: Rc::new(Cell::new(4096)),
    };
    solver.resolve(&parse_atom(package)?)?;
    Ok(solver.plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn item(name: &str, version: &str, depends: &str) -> PackageSummary {
        PackageSummary {
            name: name.into(), version: version.into(), architecture: "amd64".into(),
            description: String::new(), section: String::new(), depends: depends.into(),
            pre_depends: String::new(), recommends: String::new(), suggests: String::new(),
            provides: String::new(), conflicts: String::new(), breaks: String::new(),
        }
    }
    fn solve(catalog: &BTreeMap<String, PackageSummary>, installed: &BTreeMap<String,String>, name: &str) -> Result<Vec<PlanStep>> {
        let mut s = Solver {
            catalog, installed, installed_relations: &BTreeMap::new(),
            visiting: BTreeSet::new(), done: BTreeSet::new(),
            plan: Vec::new(), limit: 20, branch_budget: Rc::new(Cell::new(4096)),
        };
        s.resolve(&parse_atom(name)?)?;
        Ok(s.plan)
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
        let mut solver = Solver {
            catalog: &catalog, installed: &installed, installed_relations: &installed_relations,
            visiting: BTreeSet::new(), done: BTreeSet::new(),
            plan: Vec::new(), limit: 20, branch_budget: Rc::new(Cell::new(100)),
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
}

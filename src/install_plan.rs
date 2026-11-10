//! Fail-closed dependency planning preview. NEVER executes dpkg or mutates packages.
//! This is not APT's full dependency resolver: no virtual Provides, architecture
//! qualifiers, pinning, package holds, triggers or removal strategies.
use crate::{cache_query, metadata_query::{self, PackageSummary}, version};
use anyhow::{bail, Context, Result};
use std::{cmp::Ordering, collections::{BTreeMap, BTreeSet}, path::Path};

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
    if atom.contains(['[', ']', '!', ':']) || (atom.contains(['<', '>']) && !atom.contains('(')) {
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

struct Solver<'a> {
    catalog: &'a BTreeMap<String, PackageSummary>,
    installed: &'a BTreeMap<String, String>,
    visiting: BTreeSet<String>,
    done: BTreeSet<String>,
    plan: Vec<PlanStep>,
    limit: usize,
}

impl Solver<'_> {
    fn resolve(&mut self, requested: &Requirement) -> Result<()> {
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
        // Conflicts/Breaks can require removals and complex solver choices.
        if !current.conflicts.trim().is_empty() || !current.breaks.trim().is_empty() {
            bail!("{} has Conflicts/Breaks; full resolver required", current.name);
        }
        if self.plan.len() + self.visiting.len() >= self.limit {
            bail!("dependency plan exceeded maximum package count");
        }
        self.visiting.insert(requested.name.clone());
        for group in parse_groups(&current.pre_depends)?.into_iter().chain(parse_groups(&current.depends)?) {
            let mut fulfilled = false;
            for alternative in &group {
                if let Some(installed) = self.installed.get(&alternative.name) {
                    if satisfies(installed, alternative)? {
                        fulfilled = true;
                        break;
                    }
                }
            }
            if fulfilled { continue; }
            // Only pick an unambiguous dependency; never guess between options.
            if group.len() != 1 {
                bail!("unresolved alternatives for {}: choose a full APT resolver", current.name);
            }
            self.resolve(&group[0])?;
        }
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
    let mut solver = Solver {
        catalog: &catalog, installed: &installed, visiting: BTreeSet::new(),
        done: BTreeSet::new(), plan: Vec::new(), limit: max_packages,
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
            catalog, installed, visiting: BTreeSet::new(), done: BTreeSet::new(),
            plan: Vec::new(), limit: 20,
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
    fn conflict_and_downgrade_fail_closed() {
        let mut p = item("pkg", "1", "");
        p.conflicts = "other".into();
        assert!(solve(&BTreeMap::from([("pkg".into(), p)]), &BTreeMap::new(), "pkg").is_err());
        let p = item("pkg", "1", "");
        assert!(solve(&BTreeMap::from([("pkg".into(), p)]),
            &BTreeMap::from([("pkg".into(), "2".into())]), "pkg").is_err());
    }
}

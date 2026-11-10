//! Bounded, read-only package metadata queries over the isolated RPT cache.
//! The output is intentionally not full apt show/search/list compatibility.
use crate::version;
use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use std::{cmp::Ordering, collections::BTreeMap, fs, io::{BufRead, BufReader, Read}, path::{Path, PathBuf}};
use xz2::read::XzDecoder;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PackageSummary {
    pub name: String,
    pub version: String,
    pub architecture: String,
    pub description: String,
    pub section: String,
    pub depends: String,
    pub pre_depends: String,
    pub recommends: String,
    pub suggests: String,
    pub provides: String,
    pub conflicts: String,
    pub breaks: String,
}

#[derive(Default)]
struct Stanza {
    name: String,
    version: String,
    arch: String,
    description: String,
    section: String,
    depends: String,
    pre_depends: String,
    recommends: String,
    suggests: String,
    provides: String,
    conflicts: String,
    breaks: String,
    last_key: String,
}

impl Stanza {
    fn field(&mut self, key: &str, value: &str) {
        match key {
            "Package" => self.name = value.to_owned(),
            "Version" => self.version = value.to_owned(),
            "Architecture" => self.arch = value.to_owned(),
            "Description" => self.description = value.to_owned(),
            "Section" => self.section = value.to_owned(),
            "Depends" => self.depends = value.to_owned(),
            "Pre-Depends" => self.pre_depends = value.to_owned(),
            "Recommends" => self.recommends = value.to_owned(),
            "Suggests" => self.suggests = value.to_owned(),
            "Provides" => self.provides = value.to_owned(),
            "Conflicts" => self.conflicts = value.to_owned(),
            "Breaks" => self.breaks = value.to_owned(),
            _ => {}
        }
        self.last_key = key.to_owned();
    }
    fn continuation(&mut self, value: &str) {
        match self.last_key.as_str() {
            "Description" => { self.description.push('\n'); self.description.push_str(value); }
            "Depends" => { self.depends.push(' '); self.depends.push_str(value.trim()); }
            "Pre-Depends" => { self.pre_depends.push(' '); self.pre_depends.push_str(value.trim()); }
            "Recommends" => { self.recommends.push(' '); self.recommends.push_str(value.trim()); }
            "Suggests" => { self.suggests.push(' '); self.suggests.push_str(value.trim()); }
            "Provides" => { self.provides.push(' '); self.provides.push_str(value.trim()); }
            "Conflicts" => { self.conflicts.push(' '); self.conflicts.push_str(value.trim()); }
            "Breaks" => { self.breaks.push(' '); self.breaks.push_str(value.trim()); }
            _ => {}
        }
    }
    fn into_summary(self) -> Option<PackageSummary> {
        if self.name.is_empty() || self.version.is_empty() || self.arch.is_empty() { return None; }
        Some(PackageSummary {
            name: self.name, version: self.version, architecture: self.arch,
            description: self.description, section: self.section, depends: self.depends,
            pre_depends: self.pre_depends, recommends: self.recommends,
            suggests: self.suggests, provides: self.provides,
            conflicts: self.conflicts, breaks: self.breaks,
        })
    }
}

fn indexes(cache: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    let repos = fs::read_dir(cache).with_context(|| format!("reading cache {}", cache.display()))?;
    for entry in repos {
        let repo = entry?.path();
        let name = repo.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if name.len() != 24 || !name.bytes().all(|b| b.is_ascii_hexdigit()) || !repo.is_dir() || !repo.join("InRelease").is_file() { continue; }
        for item in fs::read_dir(repo)? {
            let path = item?.path();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if path.is_file() && name.starts_with("index-") && matches!(path.extension().and_then(|s| s.to_str()), Some("xz" | "gz" | "raw")) {
                paths.push(path);
            }
        }
    }
    paths.sort();
    if paths.is_empty() { bail!("no cached index files; run rpt update first"); }
    Ok(paths)
}

fn open_index(path: &Path) -> Result<Box<dyn Read>> {
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    match path.extension().and_then(|s| s.to_str()) {
        Some("xz") => Ok(Box::new(XzDecoder::new_multi_decoder(file))),
        Some("gz") => Ok(Box::new(GzDecoder::new(file))),
        Some("raw") => Ok(Box::new(file)),
        _ => bail!("unsupported cached index {}", path.display()),
    }
}

fn stream_stanzas(reader: impl Read, mut callback: impl FnMut(PackageSummary) -> Result<()>) -> Result<()> {
    let mut reader = BufReader::new(reader);
    let mut line = String::new();
    let mut stanza = Stanza::default();
    let mut stanza_len = 0usize;
    loop {
        line.clear();
        let size = reader.read_line(&mut line)?;
        if size == 0 || line.trim().is_empty() {
            if let Some(entry) = std::mem::take(&mut stanza).into_summary() {
                callback(entry)?;
            }
            stanza_len = 0;
            if size == 0 { return Ok(()); }
            continue;
        }
        stanza_len += size;
        if size > 1024 * 1024 || stanza_len > 4 * 1024 * 1024 {
            bail!("invalid Packages index: oversized metadata stanza");
        }
        let clean = line.trim_end_matches(['\n', '\r']);
        if clean.starts_with(' ') || clean.starts_with('\t') {
            if stanza.last_key.is_empty() { bail!("orphan Packages metadata continuation"); }
            stanza.continuation(&clean[1..]);
            continue;
        }
        let (key, value) = clean.split_once(':').context("invalid Packages metadata field")?;
        stanza.field(key, value.trim_start());
    }
}

fn is_better(a: &str, b: &str) -> Result<bool> {
    Ok(version::compare(a, b)? == Ordering::Greater)
}

fn scan_cache(
    cache: &Path, architecture: &str,
    mut callback: impl FnMut(PackageSummary) -> Result<()>,
) -> Result<()> {
    for path in indexes(cache)? {
        stream_stanzas(open_index(&path)?, |entry| {
            if entry.architecture == architecture || entry.architecture == "all" {
                callback(entry)?;
            }
            Ok(())
        }).with_context(|| format!("parsing {}", path.display()))?;
    }
    Ok(())
}

/// Returns the highest published version in the isolated cache, not the APT Candidate.
pub fn show(cache: &Path, architecture: &str, package: &str) -> Result<Option<PackageSummary>> {
    let mut found: Option<PackageSummary> = None;
    scan_cache(cache, architecture, |entry| {
        if entry.name == package && (found.as_ref().is_none_or(|old| is_better(&entry.version, &old.version).unwrap_or(false))) {
            version::compare(&entry.version, &entry.version)?;
            found = Some(entry);
        }
        Ok(())
    })?;
    Ok(found)
}

/// Case-insensitive text search in package names and descriptions. Results
/// are deduplicated by package name and capped to avoid unbounded allocations.
pub fn search(cache: &Path, architecture: &str, query: &str, limit: usize) -> Result<Vec<PackageSummary>> {
    if query.trim().is_empty() || !(1..=1000).contains(&limit) { bail!("query required and limit must be 1..=1000"); }
    let needle = query.to_lowercase();
    let mut selected = BTreeMap::<String, PackageSummary>::new();
    scan_cache(cache, architecture, |entry| {
        if !entry.name.to_lowercase().contains(&needle) && !entry.description.to_lowercase().contains(&needle) {
            return Ok(());
        }
        if let Some(old) = selected.get(&entry.name) {
            if is_better(&entry.version, &old.version)? { selected.insert(entry.name.clone(), entry); }
        } else if selected.len() < limit {
            selected.insert(entry.name.clone(), entry);
        }
        Ok(())
    })?;
    Ok(selected.into_values().collect())
}

/// Prefix-filtered package list, one highest published version per package.
pub fn list(cache: &Path, architecture: &str, prefix: &str, limit: usize) -> Result<Vec<PackageSummary>> {
    if !(1..=1000).contains(&limit) { bail!("limit must be 1..=1000"); }
    let mut selected = BTreeMap::<String, PackageSummary>::new();
    scan_cache(cache, architecture, |entry| {
        if !entry.name.starts_with(prefix) { return Ok(()); }
        if let Some(old) = selected.get(&entry.name) {
            if is_better(&entry.version, &old.version)? { selected.insert(entry.name.clone(), entry); }
        } else if selected.len() < limit {
            selected.insert(entry.name.clone(), entry);
        }
        Ok(())
    })?;
    Ok(selected.into_values().collect())
}


/// Enumerate all packages whose package-name token appears among their
/// dependency alternatives. This is a read-only metadata search, not
/// a transitive solver and does not resolve virtual Provides.
pub fn rdepends(
    cache: &Path, architecture: &str, package: &str, limit: usize,
) -> Result<Vec<PackageSummary>> {
    if package.trim().is_empty() || !(1..=1000).contains(&limit) {
        bail!("package required and limit must be 1..=1000");
    }
    let mut selected = BTreeMap::<String, PackageSummary>::new();
    scan_cache(cache, architecture, |entry| {
        let matches = [&entry.depends, &entry.pre_depends].iter().any(|field| {
            field.split(&[',', '|'][..]).any(|clause| {
                let atom = clause.split_whitespace().next().unwrap_or("");
                atom.split(':').next().unwrap_or("") == package
            })
        });
        if !matches { return Ok(()); }
        if let Some(old) = selected.get(&entry.name) {
            if is_better(&entry.version, &old.version)? {
                selected.insert(entry.name.clone(), entry);
            }
        } else if selected.len() < limit {
            selected.insert(entry.name.clone(), entry);
        }
        Ok(())
    })?;
    Ok(selected.into_values().collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn multiline_and_latest_version() {
        let raw = b"Package: demo\nVersion: 1.0\nArchitecture: amd64\nDescription: old\n\nPackage: demo\nVersion: 2.0\nArchitecture: amd64\nDescription: New text\n next line\nDepends: libc6 (>= 2.39)\n\nPackage: other\nVersion: 9.0\nArchitecture: arm64\n\n";
        let mut entries = Vec::new();
        stream_stanzas(&raw[..], |entry| { entries.push(entry); Ok(()) }).unwrap();
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[1].description, "New text\nnext line");
        let cache = std::env::temp_dir().join(format!("rpt-metadata-{}", std::process::id()));
        let dir = cache.join("abcdefabcdefabcdefabcdef");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("InRelease"), b"fixture").unwrap();
        fs::write(dir.join("index-0.raw"), raw).unwrap();
        assert_eq!(show(&cache, "amd64", "demo").unwrap().unwrap().version, "2.0");
        assert_eq!(search(&cache, "amd64", "new text", 10).unwrap().len(), 1);
        assert_eq!(list(&cache, "amd64", "de", 10).unwrap().len(), 1);
        assert!(search(&cache, "amd64", "", 10).is_err());
        fs::remove_dir_all(cache).unwrap();
    }
}

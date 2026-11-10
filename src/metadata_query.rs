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

/// Build an in-memory catalog of the latest published versions from verified-update
/// cache. This is an exploratory catalog, not APT policy (pins, phased updates,
/// multiarch and virtual providers are not applied).
pub fn catalog(cache: &Path, architecture: &str, max_packages: usize) -> Result<BTreeMap<String, PackageSummary>> {
    if max_packages == 0 || max_packages > 1_000_000 { bail!("max_packages must be 1..=1000000"); }
    let mut result = BTreeMap::<String, PackageSummary>::new();
    scan_cache(cache, architecture, |item| {
        if let Some(old) = result.get(&item.name) {
            if is_better(&item.version, &old.version)? {
                result.insert(item.name.clone(), item);
            }
        } else {
            if result.len() >= max_packages { bail!("package catalog exceeds max_packages"); }
            version::compare(&item.version, &item.version)?;
            result.insert(item.name.clone(), item);
        }
        Ok(())
    })?;
    Ok(result)
}

/// Return exact-version metadata from the cache. The caller MUST first verify
/// signed origin/index provenance for this package/version. Reject divergent
/// dependency-bearing metadata across repositories instead of choosing one.
pub fn exact_version(
    cache: &Path, architecture: &str, package: &str, wanted: &str,
) -> Result<PackageSummary> {
    let mut selected: Option<PackageSummary> = None;
    scan_cache(cache, architecture, |item| {
        if item.name != package || item.version != wanted { return Ok(()); }
        if let Some(previous) = &selected {
            if previous != &item {
                bail!("divergent metadata for {package} {wanted} across cached indexes");
            }
        } else {
            selected = Some(item);
        }
        Ok(())
    })?;
    selected.with_context(|| format!("no exact metadata for candidate {package} {wanted}"))
}

/// Scan only local index paths linked to previously verified signed origins.
/// The caller obtains origins from verified_version_origins; metadata from
/// unlisted index files in cache must never enter the dependency solver.
pub fn verified_exact_version(
    cache: &Path, architecture: &str, package: &str, wanted: &str,
    origins: &[crate::package_provenance::VerifiedVersionOrigin],
) -> Result<PackageSummary> {
    use crate::package_provenance::RepositoryProvenance;
    let relevant = origins.iter().filter(|o| o.version == wanted)
        .collect::<Vec<_>>();
    if relevant.is_empty() {
        bail!("candidate {package} {wanted} has no verified repository occurrence");
    }
    let mut selected: Option<PackageSummary> = None;
    for origin in relevant {
        let mut matches = 0usize;
        for entry in fs::read_dir(cache)? {
            let path = entry?.path();
            let dir_name = path.file_name().and_then(|v| v.to_str()).unwrap_or("");
            if dir_name.len() != 24 || !dir_name.bytes().all(|c| c.is_ascii_hexdigit())
                || !path.is_dir() { continue; }
            let meta_path = path.join("provenance.json");
            if !meta_path.is_file() { continue; }
            let meta: RepositoryProvenance = serde_json::from_slice(&fs::read(meta_path)?)?;
            if meta.uri != origin.repository_uri || meta.suite != origin.suite {
                continue;
            }
            for index in &meta.indexes {
                if index.release_path != origin.index_path { continue; }
                matches += 1;
                // Provenance validation checked this local path and its hash
                // and GPG signature before supplying the origin.
                stream_stanzas(open_index(&path.join(&index.local_file))?, |item| {
                    if item.name != package || item.version != wanted
                        || !(item.architecture == architecture || item.architecture == "all") {
                        return Ok(());
                    }
                    if let Some(previous) = &selected {
                        if previous != &item {
                            bail!("divergent metadata for {package} {wanted} across verified indexes");
                        }
                    } else {
                        selected = Some(item);
                    }
                    Ok(())
                })?;
            }
        }
        if matches != 1 {
            bail!("verified index origin for {package} {wanted} maps to {matches} cache paths");
        }
    }
    selected.with_context(|| format!("signed index lacks exact metadata for candidate {package} {wanted}"))
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
    fn verified_exact_version_ignores_unlisted_cached_index() {
        use crate::package_provenance::{
            IndexProvenance, ReleaseIdentity, RepositoryProvenance, VerifiedVersionOrigin,
        };
        let root = std::env::temp_dir().join(format!("rpt-verified-meta-{}", std::process::id()));
        let dir = root.join("cccccccccccccccccccccccc");
        fs::create_dir_all(&dir).unwrap();
        let trusted = "Package: lib\nVersion: 1\nArchitecture: amd64\nDepends: safe-api\n\n";
        fs::write(dir.join("index-0.raw"), trusted).unwrap();
        fs::write(dir.join("index-rogue.raw"),
            "Package: lib\nVersion: 1\nArchitecture: amd64\nDepends: malicious-api\n\n").unwrap();
        let meta = RepositoryProvenance {
            uri: "https://example.org".into(), suite: "stable".into(),
            keyring: "/ignored/test-only".into(),
            indexes: vec![IndexProvenance {
                release_path: "main/binary-amd64/Packages".into(),
                local_file: "index-0.raw".into(),
                sha256: "a".repeat(64), size: trusted.len() as u64,
            }],
        };
        fs::write(dir.join("provenance.json"), serde_json::to_vec(&meta).unwrap()).unwrap();
        let origin = VerifiedVersionOrigin {
            version: "1".into(), architecture: "amd64".into(),
            repository_uri: meta.uri, suite: meta.suite,
            index_path: "main/binary-amd64/Packages".into(),
            component: "main".into(), phased_update_percentage: None,
            source_package: "lib".into(), source_version: "1".into(),
            release: ReleaseIdentity::default(),
        };
        assert_eq!(verified_exact_version(&root, "amd64", "lib", "1", &[origin])
            .unwrap().depends, "safe-api");
        fs::remove_dir_all(root).unwrap();
    }
    #[test]
    fn exact_version_returns_candidate_stanza_and_rejects_divergent_duplicates() {
        let root = std::env::temp_dir().join(format!("rpt-exact-candidate-{}", std::process::id()));
        let one = root.join("aaaaaaaaaaaaaaaaaaaaaaaa");
        let two = root.join("bbbbbbbbbbbbbbbbbbbbbbbb");
        fs::create_dir_all(&one).unwrap();
        fs::create_dir_all(&two).unwrap();
        fs::write(one.join("InRelease"), "fixture").unwrap();
        fs::write(two.join("InRelease"), "fixture").unwrap();
        fs::write(one.join("index-0.raw"),
            "Package: lib\nVersion: 2\nArchitecture: amd64\nDepends: missing\n\nPackage: lib\nVersion: 1\nArchitecture: amd64\nDepends: old-api\n\n").unwrap();
        let old = exact_version(&root, "amd64", "lib", "1").unwrap();
        assert_eq!(old.depends, "old-api");
        assert_eq!(old.version, "1");
        assert!(exact_version(&root, "amd64", "lib", "3").is_err());
        fs::write(two.join("index-0.raw"),
            "Package: lib\nVersion: 1\nArchitecture: amd64\nDepends: conflicting-api\n\n").unwrap();
        assert!(exact_version(&root, "amd64", "lib", "1").is_err());
        fs::remove_dir_all(root).unwrap();
    }
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

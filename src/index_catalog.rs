//! Read-only, memory-bounded enumeration of package versions across local APT indexes.
//! This is NOT apt-cache policy: no pin priorities, installed status or trust inference.
use crate::version;
use anyhow::{Context, Result};
use std::{cmp::Ordering, collections::HashSet, fs, io::{BufRead, BufReader}, path::Path};

fn scan_file(path: &Path, package: &str, architecture: &str, versions: &mut HashSet<String>) -> Result<()> {
    let reader = BufReader::new(fs::File::open(path).with_context(|| format!("opening {}", path.display()))?);
    let (mut name, mut ver, mut arch) = (String::new(), String::new(), String::new());
    let mut flush = |name: &mut String, ver: &mut String, arch: &mut String| {
        if name == package && !ver.is_empty() && (arch == architecture || arch == "all") {
            versions.insert(std::mem::take(ver));
        }
        name.clear();
        ver.clear();
        arch.clear();
    };
    for line in reader.lines() {
        let line = line.with_context(|| format!("reading {}", path.display()))?;
        if line.trim_end_matches('\r').is_empty() {
            flush(&mut name, &mut ver, &mut arch);
        } else if let Some((key, value)) = line.split_once(':') {
            match key {
                "Package" => name = value.trim().to_owned(),
                "Version" => ver = value.trim().to_owned(),
                "Architecture" => arch = value.trim().to_owned(),
                _ => {}
            }
        }
    }
    flush(&mut name, &mut ver, &mut arch);
    Ok(())
}

/// Enumerate version strings from uncompressed *_Packages files, sorted newest-first.
/// This does not handle compressed files or authenticate the index contents.
pub fn available_versions(root: &Path, package: &str, architecture: &str) -> Result<Vec<String>> {
    let mut paths = fs::read_dir(root)
        .with_context(|| format!("listing index directory {}", root.display()))?
        .map(|entry| entry.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    paths.sort();
    let mut versions = HashSet::new();
    for path in paths {
        let file_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if !path.is_file() || !file_name.ends_with("_Packages") {
            continue;
        }
        if !file_name.contains(&format!("_binary-{architecture}_Packages")) {
            // Indexes for Architecture: all may also be folded into amd64 lists.
            continue;
        }
        scan_file(&path, package, architecture, &mut versions)?;
    }
    let mut results = versions.into_iter().collect::<Vec<_>>();
    // Reject unrecognized Debian version strings rather than silently mis-sort.
    for ver in &results { version::compare(ver, ver)?; }
    results.sort_by(|a, b| version::compare(b, a).unwrap_or(Ordering::Equal));
    Ok(results)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn indexes_multiple_sources_and_architectures() {
        let root = std::env::temp_dir().join(format!("rpt-index-catalog-{}", std::process::id()));
        fs::create_dir_all(&root).unwrap();
        let first = "Package: bash\nVersion: 1.0\nArchitecture: amd64\n\nPackage: other\nVersion: 9\nArchitecture: amd64\n\nPackage: bash\nVersion: 2.0~rc1\nArchitecture: amd64\n\n";
        let second = "Package: bash\nVersion: 2.0\nArchitecture: amd64\n\nPackage: bash\nVersion: 999\nArchitecture: arm64\n\nPackage: bash\nVersion: 2.0\nArchitecture: all\n";
        fs::write(root.join("mirror_dists_noble_main_binary-amd64_Packages"), first).unwrap();
        fs::write(root.join("mirror_dists_noble-updates_main_binary-amd64_Packages"), second).unwrap();
        fs::write(root.join("mirror_dists_noble_main_binary-arm64_Packages"), second).unwrap();
        assert_eq!(available_versions(&root, "bash", "amd64").unwrap(),
            vec!["2.0", "2.0~rc1", "1.0"]);
        assert!(available_versions(&root, "missing", "amd64").unwrap().is_empty());
        fs::remove_dir_all(root).unwrap();
    }
}

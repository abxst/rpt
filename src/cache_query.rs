//! Read-only streaming queries over RPT's isolated verified-update cache and dpkg status.
//! This is NOT APT candidate selection (pinning, origin and policy are not applied).
use crate::version;
use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use std::{cmp::Ordering, collections::{HashSet, BTreeMap}, fs, io::{BufRead, BufReader, Read}, path::Path};
use xz2::read::XzDecoder;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    pub version: String,
    pub architecture: String,
    pub status: String,
}

fn read_index(path: &Path) -> Result<Box<dyn Read>> {
    let file = fs::File::open(path).with_context(|| format!("opening {}", path.display()))?;
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("xz") => Ok(Box::new(XzDecoder::new_multi_decoder(file))),
        Some("gz") => Ok(Box::new(GzDecoder::new(file))),
        Some("raw") => Ok(Box::new(file)),
        _ => bail!("unsupported cached index compression: {}", path.display()),
    }
}

/// Scan RFC822 stanzas without allocating all packages. The callback sees
/// package/version/architecture/status from one complete stanza.
fn scan<R: Read>(reader: R, mut found: impl FnMut(&str, &str, &str, &str)) -> Result<()> {
    let mut pkg = String::new();
    let mut ver = String::new();
    let mut arch = String::new();
    let mut status = String::new();
    let mut row = |pkg: &mut String, ver: &mut String, arch: &mut String, status: &mut String| {
        if !pkg.is_empty() { found(pkg, ver, arch, status); }
        pkg.clear(); ver.clear(); arch.clear(); status.clear();
    };
    // Avoid pathological gigantic lines from malformed inputs.
    let mut input = BufReader::new(reader);
    let mut line = String::new();
    loop {
        line.clear();
        let n = input.read_line(&mut line)?;
        if n == 0 { row(&mut pkg, &mut ver, &mut arch, &mut status); break; }
        if line.len() > 1024 * 1024 { bail!("metadata line exceeds 1 MiB"); }
        let clean = line.trim_end_matches(['\r', '\n']);
        if clean.is_empty() {
            row(&mut pkg, &mut ver, &mut arch, &mut status);
        } else if clean.starts_with(' ') || clean.starts_with('\t') {
            continue;
        } else if let Some((key, value)) = clean.split_once(':') {
            match key {
                "Package" => pkg = value.trim().to_owned(),
                "Version" => ver = value.trim().to_owned(),
                "Architecture" => arch = value.trim().to_owned(),
                "Status" => status = value.trim().to_owned(),
                _ => {}
            }
        }
    }
    Ok(())
}

/// dpkg status embeds the desired selection in its first word. A held
/// package remains fully installed and must not disappear from the installed
/// catalog simply because its selection differs from "install".
pub fn is_installed_status(status: &str) -> bool {
    matches!(status, "install ok installed" | "hold ok installed")
}

/// Versions available in a previously downloaded RPT cache, newest-first.
/// Does not access /var/lib/apt/lists or infer repository pinning.
pub fn cache_versions(cache: &Path, package: &str, architecture: &str) -> Result<Vec<String>> {
    let mut versions = HashSet::new();
    let mut repositories = fs::read_dir(cache)
        .with_context(|| format!("reading RPT cache {}", cache.display()))?
        .map(|e| e.map(|v| v.path())).collect::<std::io::Result<Vec<_>>>()?;
    repositories.sort();
    let mut indexes = 0usize;
    for repo in repositories {
        if !repo.is_dir() || repo.file_name().and_then(|n| n.to_str()).is_none_or(|name| {
            name.starts_with('.') || name.len() != 24 || !name.bytes().all(|b| b.is_ascii_hexdigit())
        }) { continue; }
        if !repo.join("InRelease").is_file() { continue; }
        let mut paths = fs::read_dir(&repo)?
            .map(|e| e.map(|v| v.path())).collect::<std::io::Result<Vec<_>>>()?;
        paths.sort();
        for index in paths {
            if !index.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("index-")) { continue; }
            indexes += 1;
            scan(read_index(&index)?, |name, version, arch, _| {
                if name == package && !version.is_empty() && (arch == architecture || arch == "all") {
                    versions.insert(version.to_owned());
                }
            }).with_context(|| format!("reading {}", index.display()))?;
        }
    }
    if indexes == 0 { bail!("no cached Packages indexes found; run rpt update first"); }
    let mut result = versions.into_iter().collect::<Vec<_>>();
    for version_string in &result { version::compare(version_string, version_string)?; }
    result.sort_by(|a, b| version::compare(b, a).unwrap_or(Ordering::Equal));
    Ok(result)
}

/// Inspect the installed version from dpkg's state, without changing the system.
/// Ignore residual-config entries and not-installed states.
pub fn installed_version(status_file: &Path, package: &str, architecture: &str) -> Result<Option<Installed>> {
    let mut found = None;
    let input = fs::File::open(status_file)
        .with_context(|| format!("opening {}", status_file.display()))?;
    scan(input, |name, version, arch, status| {
        if name == package && (arch == architecture || arch == "all")
            && is_installed_status(status) && !version.is_empty() {
            found = Some(Installed {
                version: version.to_owned(),
                architecture: arch.to_owned(),
                status: status.to_owned(),
            });
        }
    })?;
    Ok(found)
}

/// Load dpkg installed state once for dependency planning. Does not mutate dpkg.
pub fn installed_catalog(status_file: &Path, architecture: &str) -> Result<BTreeMap<String, String>> {
    let input = fs::File::open(status_file)
        .with_context(|| format!("opening {}", status_file.display()))?;
    let mut result = BTreeMap::new();
    scan(input, |name, version, arch, status| {
        if (arch == architecture || arch == "all") &&
            is_installed_status(status) && !version.is_empty() {
            result.insert(name.to_owned(), version.to_owned());
        }
    })?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn installed_status_only_reports_installed() {
        let status = b"Package: bash\nStatus: install ok installed\nArchitecture: amd64\nVersion: 5.2\n\nPackage: old\nStatus: deinstall ok config-files\nArchitecture: amd64\nVersion: 1.0\n";
        let mut matches = Vec::new();
        scan(&status[..], |name, ver, arch, state| matches.push((name.to_owned(), ver.to_owned(), arch.to_owned(), state.to_owned()))).unwrap();
        assert_eq!(matches[0].0, "bash");
        assert_eq!(matches[0].1, "5.2");
        assert_eq!(matches[1].3, "deinstall ok config-files");
    }

    #[test]
    fn held_dpkg_selection_is_still_installed() {
        assert!(is_installed_status("install ok installed"));
        assert!(is_installed_status("hold ok installed"));
        assert!(!is_installed_status("deinstall ok config-files"));
        assert!(!is_installed_status("hold reinstreq half-installed"));
        let dir = std::env::temp_dir().join(format!("rpt-held-status-{}", std::process::id()));
        fs::write(&dir, "Package: core\nVersion: 1\nArchitecture: amd64\nStatus: hold ok installed\n\n").unwrap();
        let got = installed_catalog(&dir, "amd64").unwrap();
        assert_eq!(got.get("core").map(String::as_str), Some("1"));
        assert_eq!(installed_version(&dir, "core", "amd64").unwrap().unwrap().status, "hold ok installed");
        fs::remove_file(dir).unwrap();
    }
    #[test]
    fn scans_xz_indexes() {
        use std::io::Read;
        let bytes = b"Package: bash\nVersion: 3.0\nArchitecture: amd64\n\n";
        let compressed = {
            let mut decoder = xz2::read::XzEncoder::new(&bytes[..], 6);
            let mut out = Vec::new();
            decoder.read_to_end(&mut out).unwrap();
            out
        };
        let mut names = Vec::new();
        scan(XzDecoder::new_multi_decoder(&compressed[..]), |name, ver, _, _| {
            names.push((name.to_owned(), ver.to_owned()));
        }).unwrap();
        assert_eq!(names, vec![("bash".into(), "3.0".into())]);
    }

    #[test]
    fn scans_compressed_indexes() {
        use std::io::Write;
        let root = std::env::temp_dir().join(format!("rpt-cache-query-{}", std::process::id()));
        let dir = root.join("abcdefabcdefabcdefabcdef");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("InRelease"), b"test").unwrap();
        let input = b"Package: bash\nVersion: 2.0\nArchitecture: amd64\n\nPackage: bash\nVersion: 1.0\nArchitecture: amd64\n\n";
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        enc.write_all(input).unwrap();
        fs::write(dir.join("index-0.gz"), enc.finish().unwrap()).unwrap();
        assert_eq!(cache_versions(&root, "bash", "amd64").unwrap(), vec!["2.0", "1.0"]);
        fs::remove_dir_all(root).unwrap();
    }
}

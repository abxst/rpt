//! Rechecks GPG-signed repository/index provenance before deriving a .deb download plan.
//! Read-only planning; package changes and dependency solving remain unsupported.
use crate::{version, Manifest, Package};
use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    cmp::Ordering, collections::{BTreeMap, BTreeSet}, fs,
    io::{BufRead, BufReader, Read},
    path::{Component, Path, PathBuf}, process::Command,
};
use xz2::read::XzDecoder;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexProvenance {
    pub release_path: String,
    pub local_file: String,
    pub sha256: String,
    pub size: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RepositoryProvenance {
    pub uri: String,
    pub suite: String,
    pub keyring: String,
    pub indexes: Vec<IndexProvenance>,
}
#[derive(Default)]
struct Fields {
    name: String, version: String, arch: String,
    source: Option<String>,
    filename: String, sha256: String, size: String,
    phased_update_percentage: Option<u8>,
}
fn hash_valid(hash: &str) -> bool {
    hash.len() == 64 && hash.bytes().all(|c| c.is_ascii_hexdigit())
}
fn safe_path(s: &str) -> bool {
    !s.is_empty() && Path::new(s).components().all(|c| matches!(c, Component::Normal(_)))
        && !s.split('/').any(|p| p.is_empty() || p == "." || p == "..")
        && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b'/'|b'.'|b'_'|b'-'|b'+'|b'~'))
}
fn sha256_file(path: &Path) -> Result<(String,u64)> {
    let mut f = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut bytes = 0u64;
    let mut buf = [0u8;65536];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 { break; }
        bytes = bytes.checked_add(n as u64).context("index size overflow")?;
        digest.update(&buf[..n]);
    }
    Ok((hex::encode(digest.finalize()),bytes))
}
fn signed_hashes(signed: &str) -> Result<BTreeMap<String,(String,u64)>> {
    if !signed.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") { bail!("not a clearsigned InRelease"); }
    let (_, body) = signed.split_once("\n\n").context("invalid InRelease")?;
    let (body, _) = body.split_once("-----BEGIN PGP SIGNATURE-----").context("unsigned InRelease")?;
    let mut in_sha = false;
    let mut entries = BTreeMap::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line == "SHA256:" { in_sha = true; continue; }
        if !in_sha { continue; }
        if !line.starts_with(' ') { in_sha = false; continue; }
        let parts = line.split_whitespace().collect::<Vec<_>>();
        if parts.len()!=3 || !hash_valid(parts[0]) || parts[2].starts_with('/') || parts[2].split('/').any(|part| part.is_empty() || part == ".." || part == ".") { bail!("malformed Release checksum entry: {}", line); }
        let size = parts[1].parse::<u64>()?;
        if entries.insert(parts[2].to_owned(), (parts[0].to_ascii_lowercase(),size)).is_some() {
            bail!("duplicate signed Release checksum path");
        }
    }
    if entries.is_empty() { bail!("signed Release missing SHA256 hashes"); }
    Ok(entries)
}
fn validate_repo(dir: &Path, meta: &RepositoryProvenance) -> Result<()> {
    let uri = reqwest::Url::parse(&meta.uri)?;
    if !matches!(uri.scheme(),"http"|"https") || uri.host_str().is_none()
        || uri.query().is_some() || uri.fragment().is_some()
        || !uri.username().is_empty() || uri.password().is_some() { bail!("unsafe repository base URI"); }
    if meta.suite.is_empty() || !meta.suite.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'-'|b'_')) {
        bail!("unsafe repository suite");
    }
    let key = Path::new(&meta.keyring);
    if !key.is_absolute() || !key.is_file() { bail!("Signed-By keyring unavailable"); }
    let inrelease = dir.join("InRelease");
    let result = Command::new("gpgv").arg("--keyring").arg(key).arg(&inrelease)
        .output().context("running gpgv")?;
    if !result.status.success() { bail!("cached InRelease signature verification failed"); }
    let signed = fs::read_to_string(&inrelease)?;
    crate::update::check_release_freshness(&signed, chrono::Utc::now())?;
    let checksums = signed_hashes(&signed)?;
    if meta.indexes.is_empty() { bail!("repository provenance has no indexes"); }
    let mut seen = BTreeSet::new();
    for idx in &meta.indexes {
        if idx.release_path.starts_with('/') || idx.release_path.split('/').any(|part| part.is_empty() || part == ".." || part == ".") || !safe_path(&idx.local_file)
            || idx.local_file.contains('/') || !idx.local_file.starts_with("index-")
            || !hash_valid(&idx.sha256) { bail!("unsafe index provenance"); }
        if !seen.insert(&idx.release_path) { bail!("duplicate repository index path"); }
        let (signed_digest,signed_size) = checksums.get(&idx.release_path)
            .with_context(||format!("index missing from signed Release: {}",idx.release_path))?;
        if *signed_digest != idx.sha256.to_ascii_lowercase() || *signed_size != idx.size {
            bail!("index provenance does not match signed Release");
        }
        let local = dir.join(&idx.local_file);
        if fs::symlink_metadata(&local)?.file_type().is_symlink() { bail!("symlink index rejected"); }
        let (digest, size) = sha256_file(&local)?;
        if digest != *signed_digest || size != *signed_size { bail!("cached index SHA256/size mismatch"); }
    }
    Ok(())
}
fn scan(
    reader: impl Read, mut visitor: impl FnMut(&Fields)->Result<()>,
) -> Result<()> {
    let mut input = BufReader::new(reader);
    let mut fields = Fields::default();
    let mut line = String::new();
    let mut total = 0usize;
    loop {
        line.clear();
        let n=input.read_line(&mut line)?;
        if n==0 || line.trim().is_empty() {
            if !fields.name.is_empty() { visitor(&fields)?; }
            fields=Fields::default();
            total=0;
            if n==0 { break; }
            continue;
        }
        total += n;
        if n>1048576 || total>4194304 { bail!("Packages stanza too large"); }
        if line.starts_with(' ') || line.starts_with('\t') { continue; }
        let (key,value)=line.trim_end().split_once(':').context("malformed Packages field")?;
        let value=value.trim();
        match key {
            "Package" => fields.name=value.to_owned(),
            "Version" => fields.version=value.to_owned(),
            "Architecture" => fields.arch=value.to_owned(),
            "Filename" => fields.filename=value.to_owned(),
            "SHA256" => fields.sha256=value.to_owned(),
            "Source" => {
                if fields.source.is_some() { bail!("duplicate Source field in Packages stanza"); }
                if value.is_empty() || value.len() > 1024 { bail!("invalid Source field"); }
                fields.source = Some(value.to_owned());
            },
            "Size" => fields.size=value.to_owned(),
            "Phased-Update-Percentage" => {
                if fields.phased_update_percentage.is_some() {
                    bail!("duplicate Phased-Update-Percentage in Packages stanza");
                }
                let percent = value.parse::<u8>()
                    .context("invalid Phased-Update-Percentage integer")?;
                if percent > 100 { bail!("Phased-Update-Percentage exceeds 100"); }
                fields.phased_update_percentage = Some(percent);
            },
            _ => (),
        }
    }
    Ok(())
}
fn scan_index(path: &Path, visitor: impl FnMut(&Fields)->Result<()>) -> Result<()> {
    let file=fs::File::open(path)?;
    let reader:Box<dyn Read>=match path.extension().and_then(|s|s.to_str()) {
        Some("xz")=>Box::new(XzDecoder::new_multi_decoder(file)),
        Some("gz")=>Box::new(GzDecoder::new(file)),
        Some("raw")=>Box::new(file),
        _=>bail!("unsupported cache compression"),
    };
    scan(reader,visitor).with_context(||format!("reading {}",path.display()))
}
/// Fields from the *signed* Release body, kept distinct from local source settings.
#[derive(Clone, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct ReleaseIdentity {
    pub origin: Option<String>,
    pub label: Option<String>,
    pub archive: Option<String>,
    pub codename: Option<String>,
}

/// Parse human-readable Release fields. The caller must first validate the
/// InRelease signature; parsing this string alone does NOT authenticate it.
fn parse_release_identity(signed: &str) -> Result<ReleaseIdentity> {
    if !signed.starts_with("-----BEGIN PGP SIGNED MESSAGE-----") {
        bail!("not a clearsigned InRelease");
    }
    let (_, body) = signed.split_once("\n\n").context("invalid clearsigned InRelease header")?;
    let (body, _) = body.split_once("-----BEGIN PGP SIGNATURE-----")
        .context("unsigned InRelease")?;
    let mut result = ReleaseIdentity::default();
    let mut seen = BTreeSet::new();
    for line in body.lines() {
        let line = line.trim_end_matches('\r');
        if line.starts_with(' ') || line.starts_with('\t') { continue; }
        let Some((key, value)) = line.split_once(':') else { continue; };
        let slot = match key {
            "Origin" => &mut result.origin,
            "Label" => &mut result.label,
            "Suite" | "Archive" => &mut result.archive,
            "Codename" => &mut result.codename,
            _ => continue,
        };
        // A duplicate/conflicting field is ambiguous for future policy rules.
        if !seen.insert(key) || slot.is_some() {
            bail!("duplicate signed Release identity field: {key}");
        }
        let value = value.trim();
        if value.is_empty() || value.len() > 4096
            || value.chars().any(char::is_control) {
            bail!("invalid signed Release identity field: {key}");
        }
        *slot = Some(value.to_owned());
    }
    Ok(result)
}

fn release_component(index_path: &str) -> Result<String> {
    let (component, _) = index_path.split_once('/')
        .context("Packages index lacks repository component")?;
    if component.is_empty() || !component.bytes().all(|b|
        b.is_ascii_alphanumeric() || b"-_".contains(&b)) {
        bail!("unsupported repository component");
    }
    Ok(component.to_owned())
}

/// Debian binary Source: may be "name" or "name (version)".
/// When absent, source name and version equal binary name and version.
fn source_identity(fields: &Fields) -> Result<(String, String)> {
    let Some(source) = fields.source.as_deref() else {
        return Ok((fields.name.clone(), fields.version.clone()));
    };
    let (name, version) = if let Some((name, suffix)) = source.split_once(" (") {
        let version = suffix.strip_suffix(')')
            .context("invalid Source version field")?;
        (name, version)
    } else {
        (source, fields.version.as_str())
    };
    if name.is_empty() || name.len() > 256
        || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"+.-".contains(&b))
        || version.is_empty() || version.len() > 512 {
        bail!("invalid Source package name or version");
    }
    version::compare(version, version)?;
    Ok((name.to_owned(), version.to_owned()))
}

/// Repository-scoped package occurrence, not a candidate or an APT pin.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct VerifiedVersionOrigin {
    pub version: String,
    pub architecture: String,
    pub repository_uri: String,
    pub suite: String,
    pub index_path: String,
    pub component: String,
    /// Authenticated Packages rollout percentage; None means not phased.
    pub phased_update_percentage: Option<u8>,
    /// Effective source package and source version from authenticated Packages.
    pub source_package: String,
    pub source_version: String,
    pub release: ReleaseIdentity,
}

/// Enumerate a single package's occurrences only after verifying GPG signatures,
/// Release freshness, signed index digests and the on-disk cache contents.
/// Never combine occurrences into a candidate: release pinning, architecture
/// priority, phased updates and APT preferences are not evaluated here.
pub fn verified_version_origins(
    cache: &Path, name: &str, arch: &str,
) -> Result<Vec<VerifiedVersionOrigin>> {
    if name.is_empty() || !name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.')) {
        bail!("unsafe package name");
    }
    if arch.is_empty() || !arch.bytes().all(|b| b.is_ascii_alphanumeric()) {
        bail!("unsafe architecture");
    }
    let mut dirs = Vec::new();
    for entry in fs::read_dir(cache).with_context(|| format!("reading {}", cache.display()))? {
        let path = entry?.path();
        let id = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
        if path.is_dir() && id.len() == 24 && id.bytes().all(|b| b.is_ascii_hexdigit()) {
            dirs.push(path);
        }
    }
    dirs.sort();
    if dirs.is_empty() { bail!("no repository cache; run rpt update first"); }
    let mut found = BTreeSet::new();
    for dir in dirs {
        let meta: RepositoryProvenance = serde_json::from_slice(
            &fs::read(dir.join("provenance.json"))
                .context("missing signed cache provenance: run rpt update again")?
        )?;
        validate_repo(&dir, &meta)?;
        let release = parse_release_identity(&fs::read_to_string(dir.join("InRelease"))?)?;
        for index in &meta.indexes {
            let component = release_component(&index.release_path)?;
            scan_index(&dir.join(&index.local_file), |field| {
                if field.name == name && (field.arch == arch || field.arch == "all") {
                    version::compare(&field.version, &field.version)?;
                    found.insert(VerifiedVersionOrigin {
                        version: field.version.clone(),
                        architecture: field.arch.clone(),
                        repository_uri: meta.uri.clone(),
                        suite: meta.suite.clone(),
                        index_path: index.release_path.clone(),
                        component: component.clone(),
                        phased_update_percentage: field.phased_update_percentage,
                        source_package: source_identity(field)?.0,
                        source_version: source_identity(field)?.1,
                        release: release.clone(),
                    });
                }
                Ok(())
            })?;
        }
    }
    let mut result = found.into_iter().collect::<Vec<_>>();
    result.sort_by(|a, b| {
        version::compare(&b.version, &a.version).unwrap_or(Ordering::Equal)
            .then_with(|| a.repository_uri.cmp(&b.repository_uri))
            .then_with(|| a.suite.cmp(&b.suite))
            .then_with(|| a.index_path.cmp(&b.index_path))
            .then_with(|| a.architecture.cmp(&b.architecture))
    });
    Ok(result)
}

/// Only the requested artifacts are selected; this is not an install plan or Candidate.
pub fn prepare(cache:&Path, names:&[String],arch:&str)->Result<Manifest> {
    if names.is_empty() || names.len()>100 { bail!("must request 1..=100 packages"); }
    if arch.is_empty() || !arch.bytes().all(|b|b.is_ascii_alphanumeric()){bail!("unsafe architecture");}
    let names: BTreeSet<String>=names.iter().cloned().collect();
    if names.iter().any(|x| x.is_empty() || !x.bytes().all(|b|b.is_ascii_alphanumeric() || matches!(b,b'+'|b'-'|b'.'))) {
        bail!("unsafe package name");
    }
    let mut selected=BTreeMap::<String,(String,Package)>::new();
    let mut dirs=Vec::<PathBuf>::new();
    for entry in fs::read_dir(cache).with_context(||format!("reading {}",cache.display()))? {
        let path=entry?.path();
        let name=path.file_name().and_then(|s|s.to_str()).unwrap_or("");
        if path.is_dir() && name.len()==24 && name.bytes().all(|b|b.is_ascii_hexdigit()) {
            dirs.push(path);
        }
    }
    dirs.sort();
    if dirs.is_empty() { bail!("no repository cache; run rpt update first"); }
    for dir in dirs {
        let meta:RepositoryProvenance=serde_json::from_slice(
            &fs::read(dir.join("provenance.json")).context("missing signed cache provenance: run rpt update again")?
        )?;
        validate_repo(&dir,&meta)?;
        for index in &meta.indexes {
            scan_index(&dir.join(&index.local_file),|field| {
                if !names.contains(&field.name) || (field.arch!=arch && field.arch!="all") {
                    return Ok(());
                }
                if field.version.is_empty() || !safe_path(&field.filename) || !hash_valid(&field.sha256) {
                    bail!("invalid package Filename/SHA256 in authenticated Packages");
                }
                let size=field.size.parse::<u64>()?;
                if size==0 { bail!("zero-sized artifact"); }
                version::compare(&field.version,&field.version)?;
                let package=Package {
                    name:field.name.clone(),
                    url:format!("{}/{}",meta.uri.trim_end_matches('/'),field.filename),
                    sha256:field.sha256.to_ascii_lowercase(),size,
                    filename:field.filename.clone(),
                };
                crate::validate_package(&package)?;
                match selected.get(&field.name) {
                    Some((old_version,old_package))=>{
                        match version::compare(&field.version,old_version)? {
                            Ordering::Greater => { selected.insert(field.name.clone(),(field.version.clone(),package)); }
                            Ordering::Equal if old_package.sha256!=package.sha256 || old_package.size!=package.size => {
                                bail!("same package/version has conflicting hashes");
                            }
                            _=>(),
                        }
                    }
                    None=>{selected.insert(field.name.clone(),(field.version.clone(),package));}
                }
                Ok(())
            })?;
        }
    }
    let mut packages=Vec::new();
    for name in names {
        let (_,item)=selected.remove(&name).with_context(||format!("package missing from verified cache: {name}"))?;
        packages.push(item);
    }
    Ok(Manifest{packages})
}
/// SHA256-locked artifact corresponding to one exact candidate-policy plan step.
/// Extra fields are intentionally ignored by the legacy Manifest downloader;
/// SHA256 and size still bind the fetched bytes.
#[derive(Clone, Debug, Serialize)]
pub struct ExactArtifact {
    #[serde(flatten)]
    pub artifact: Package,
    pub version: String,
    pub architecture: String,
}

#[derive(Debug, Serialize)]
pub struct ExactManifest {
    pub packages: Vec<ExactArtifact>,
    pub executable: bool,
    pub warning: String,
}

/// Revalidate signed metadata and select exact (name, architecture, version)
/// artifacts from a policy-selected plan. Never substitute "latest".
pub fn prepare_exact(
    cache: &Path, steps: &[crate::install_plan::PlanStep], native_arch: &str,
) -> Result<ExactManifest> {
    if steps.len() > 5000 { bail!("plan exceeds 5000 packages"); }
    if native_arch.is_empty() || !native_arch.bytes().all(|b| b.is_ascii_alphanumeric()) {
        bail!("invalid native architecture");
    }
    let mut requested = BTreeMap::<(String, String), String>::new();
    for step in steps {
        if step.package.len() < 2 || !step.package.bytes().all(|b|
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'+' | b'-' | b'.')) {
            bail!("invalid planned binary name");
        }
        if step.architecture != native_arch && step.architecture != "all" {
            bail!("unsupported foreign-architecture download: {}:{}", step.package, step.architecture);
        }
        version::compare(&step.version, &step.version)?;
        let key = (step.package.clone(), step.architecture.clone());
        if requested.insert(key, step.version.clone()).is_some() {
            bail!("duplicate planned package identity: {}:{}", step.package, step.architecture);
        }
    }
    let mut desired = BTreeMap::new();
    for step in steps {
        // An exact already-installed version does not require artifact download.
        if step.installed.as_deref() == Some(step.version.as_str()) { continue; }
        desired.insert((step.package.clone(), step.architecture.clone()), step.version.clone());
    }
    if desired.is_empty() {
        return Ok(ExactManifest {
            packages: Vec::new(), executable: false,
            warning: "Verified download manifest only; no dpkg execution.".into(),
        });
    }
    let mut dirs = Vec::<PathBuf>::new();
    for entry in fs::read_dir(cache).with_context(|| format!("reading {}", cache.display()))? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if path.is_dir() && name.len() == 24 && name.bytes().all(|b| b.is_ascii_hexdigit()) {
            if fs::symlink_metadata(&path)?.file_type().is_symlink() {
                bail!("symlink repository directory rejected");
            }
            dirs.push(path);
        }
    }
    dirs.sort();
    if dirs.is_empty() { bail!("no signed cache; run rpt update first"); }
    let mut found = BTreeMap::<(String, String), ExactArtifact>::new();
    for dir in dirs {
        let meta: RepositoryProvenance = serde_json::from_slice(
            &fs::read(dir.join("provenance.json"))
                .context("missing signed provenance: run rpt update first")?,
        )?;
        validate_repo(&dir, &meta)?;
        for index in &meta.indexes {
            scan_index(&dir.join(&index.local_file), |field| {
                let key = (field.name.clone(), field.arch.clone());
                let Some(wanted_version) = desired.get(&key) else { return Ok(()); };
                if &field.version != wanted_version { return Ok(()); }
                if !safe_path(&field.filename) || !field.filename.ends_with(".deb")
                    || !hash_valid(&field.sha256) {
                    bail!("invalid signed package Filename/SHA256 for {}", field.name);
                }
                let size = field.size.parse::<u64>()?;
                if size == 0 { bail!("zero-length signed package artifact"); }
                let pkg = Package {
                    name: field.name.clone(),
                    url: format!("{}/{}", meta.uri.trim_end_matches('/'), field.filename),
                    sha256: field.sha256.to_ascii_lowercase(),
                    size, filename: field.filename.clone(),
                };
                crate::validate_package(&pkg)?;
                if let Some(previous) = found.get(&key) {
                    if previous.artifact.sha256 != pkg.sha256
                        || previous.artifact.size != pkg.size
                        || previous.artifact.filename != pkg.filename {
                        bail!("ambiguous signed artifact for {}:{} {}", field.name, field.arch, field.version);
                    }
                } else {
                    found.insert(key, ExactArtifact {
                        artifact: pkg,
                        version: field.version.clone(),
                        architecture: field.arch.clone(),
                    });
                }
                Ok(())
            })?;
        }
    }
    let mut artifacts = Vec::new();
    let mut filenames = BTreeSet::new();
    for step in steps {
        if step.installed.as_deref() == Some(step.version.as_str()) { continue; }
        let key = (step.package.clone(), step.architecture.clone());
        let item = found.remove(&key).with_context(|| format!(
            "verified artifact unavailable for {}:{} version {}",
            step.package, step.architecture, step.version,
        ))?;
        if !filenames.insert(item.artifact.filename.clone()) {
            bail!("colliding output filenames in locked manifest");
        }
        artifacts.push(item);
    }
    Ok(ExactManifest {
        packages: artifacts,
        executable: false,
        warning: "Exact signed artifacts selected for the read-only policy plan. No dpkg execution.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_manifest_validates_architecture_identity_and_duplicate_roots() {
        let mk = |arch: &str| crate::install_plan::PlanStep {
            package: "testpkg".into(), architecture: arch.into(),
            version: "1".into(), installed: None,
        };
        assert!(prepare_exact(Path::new("/not-needed"), &[mk("arm64")], "amd64").is_err());
        assert!(prepare_exact(Path::new("/not-needed"), &[mk("amd64"), mk("amd64")], "amd64").is_err());
    }
    #[test]
    fn already_installed_exact_version_needs_no_download() {
        let step = crate::install_plan::PlanStep {
            package: "testpkg".into(), architecture: "amd64".into(),
            version: "1".into(), installed: Some("1".into()),
        };
        let manifest = prepare_exact(Path::new("/not-needed"), &[step], "amd64").unwrap();
        assert!(manifest.packages.is_empty());
        assert!(!manifest.executable);
    }

    #[test]
    fn filename_and_hash_are_strict() {
        assert!(!safe_path("../../etc/passwd"));
        assert!(!safe_path("pool/%2e%2e/deb.deb"));
        assert!(safe_path("pool/main/b/bash_5.2-1_amd64.deb"));
        assert!(hash_valid(&"ab".repeat(32)));
    }
    #[test]
    fn reads_package_stanza() {
        let data=format!("Package: bash\nVersion: 5.2\nArchitecture: amd64\nFilename: pool/main/bash.deb\nSHA256: {}\nSize: 123\n\n","a".repeat(64));
        let mut count=0;
        scan(data.as_bytes(), |x| {assert_eq!(x.name,"bash");assert_eq!(x.size,"123");count+=1;Ok(())}).unwrap();
        assert_eq!(count,1);
    }
    #[test]
    fn release_hash_parser() {
        let h="aa".repeat(32);
        let input=format!("-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nDate: Tue, 10 Nov 2026 09:00:00 UTC\nSHA256:\n {h} 7 main/binary-amd64/Packages.xz\n-----BEGIN PGP SIGNATURE-----");
        assert_eq!(signed_hashes(&input).unwrap()["main/binary-amd64/Packages.xz"].1,7);
    }
}


#[cfg(test)]
mod release_identity_tests {
    use super::*;

    #[test]
    fn reads_only_signed_release_header_fields() {
        let signed = "-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\nOrigin: Ubuntu\nLabel: Ubuntu\nSuite: noble-updates\nCodename: noble\nSHA256:\n abc 123 main/binary-amd64/Packages.xz\n-----BEGIN PGP SIGNATURE-----\nsignature\n";
        let fields = parse_release_identity(signed).unwrap();
        assert_eq!(fields.origin.as_deref(), Some("Ubuntu"));
        assert_eq!(fields.label.as_deref(), Some("Ubuntu"));
        assert_eq!(fields.archive.as_deref(), Some("noble-updates"));
        assert_eq!(fields.codename.as_deref(), Some("noble"));
        assert_eq!(release_component("main/binary-amd64/Packages.xz").unwrap(), "main");
    }
    #[test]
    fn refuses_ambiguous_release_identity() {
        let dup = "-----BEGIN PGP SIGNED MESSAGE-----\n\nOrigin: Ubuntu\nOrigin: Other\n-----BEGIN PGP SIGNATURE-----\n";
        assert!(parse_release_identity(dup).is_err());
        let conflicting_alias = "-----BEGIN PGP SIGNED MESSAGE-----\n\nSuite: stable\nArchive: testing\n-----BEGIN PGP SIGNATURE-----\n";
        assert!(parse_release_identity(conflicting_alias).is_err());
        assert!(parse_release_identity("Origin: Ubuntu").is_err());
        assert!(release_component("../binary-amd64/Packages.xz").is_err());
    }
}

#[cfg(test)]
mod source_identity_tests {
    use super::*;
    #[test]
    fn source_name_and_version_fallback_and_override() {
        let mut fields = Fields { name: "libdemo1".into(), version: "1.2-3".into(), ..Default::default() };
        assert_eq!(source_identity(&fields).unwrap(), ("libdemo1".into(), "1.2-3".into()));
        fields.source = Some("demo".into());
        assert_eq!(source_identity(&fields).unwrap(), ("demo".into(), "1.2-3".into()));
        fields.source = Some("demo (2:1.0-4)".into());
        assert_eq!(source_identity(&fields).unwrap(), ("demo".into(), "2:1.0-4".into()));
        for invalid in ["", "bad name", "demo ()", "demo (invalid)", "demo (1.0) garbage"] {
            fields.source = Some(invalid.into());
            assert!(source_identity(&fields).is_err(), "{invalid}");
        }
    }
}
#[cfg(test)]
mod phased_metadata_tests {
    use super::*;
    use std::io::Cursor;
    fn get(input: &str) -> Result<Option<u8>> {
        let mut value = None;
        scan(Cursor::new(input.as_bytes()), |entry| {
            value = entry.phased_update_percentage;
            Ok(())
        })?;
        Ok(value)
    }
    #[test]
    fn accepts_absent_zero_intermediate_and_complete_rollouts() {
        for (input, expected) in [
            ("Package: demo\nVersion: 1\n\n", None),
            ("Package: demo\nVersion: 1\nPhased-Update-Percentage: 0\n\n", Some(0)),
            ("Package: demo\nVersion: 1\nPhased-Update-Percentage: 42\n\n", Some(42)),
            ("Package: demo\nVersion: 1\nPhased-Update-Percentage: 100\n\n", Some(100)),
        ] {
            assert_eq!(get(input).unwrap(), expected);
        }
    }
    #[test]
    fn rejects_invalid_and_duplicate_rollout_metadata() {
        for input in [
            "Phased-Update-Percentage: 101",
            "Phased-Update-Percentage: -1",
            "Phased-Update-Percentage: nope",
            "Phased-Update-Percentage: 30\nPhased-Update-Percentage: 40",
        ] {
            assert!(get(&format!("Package: demo\nVersion: 1\n{input}\n\n")).is_err(), "{input}");
        }
    }
}

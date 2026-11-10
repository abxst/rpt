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
    filename: String, sha256: String, size: String,
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
        if parts.len()!=3 || !hash_valid(parts[0]) || !safe_path(parts[2]) { bail!("malformed Release checksum"); }
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
    let checksums = signed_hashes(&fs::read_to_string(&inrelease)?)?;
    if meta.indexes.is_empty() { bail!("repository provenance has no indexes"); }
    let mut seen = BTreeSet::new();
    for idx in &meta.indexes {
        if !safe_path(&idx.release_path) || !safe_path(&idx.local_file)
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
            "Size" => fields.size=value.to_owned(),
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
#[cfg(test)]
mod tests {
    use super::*;
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

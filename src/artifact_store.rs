//! Re-check downloaded .deb bytes against the exact authenticated plan manifest.
//! No dpkg execution, filesystem commit, or package-state mutation.
use crate::{install_plan::PlanStep, package_provenance::{self, ExactManifest}, validate_package, Package};
use anyhow::{bail, Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{fs, io::Read, path::Path};

#[derive(Debug, Serialize)]
pub struct StoreAudit {
    pub verified: usize,
    pub bytes: u64,
    pub executable: bool,
    pub warning: String,
}
fn verify_file(root: &Path, pkg: &Package) -> Result<u64> {
    validate_package(pkg)?;
    let root = fs::canonicalize(root).context("resolving artifact root")?;
    if !root.is_dir() { bail!("artifact root is not a directory"); }
    let candidate = root.join(&pkg.filename);
    if !fs::symlink_metadata(&candidate)?.file_type().is_file() {
        bail!("downloaded artifact must be a regular file: {}", candidate.display());
    }
    let canonical = fs::canonicalize(&candidate)?;
    if !canonical.starts_with(&root) {
        bail!("artifact escapes its designated root");
    }
    let mut file = fs::File::open(canonical)?;
    let mut sha = Sha256::new();
    let mut total = 0u64;
    let mut buffer = [0u8; 65_536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 { break; }
        total = total.checked_add(count as u64).context("artifact size overflow")?;
        if total > pkg.size { bail!("artifact is larger than signed size"); }
        sha.update(&buffer[..count]);
    }
    if total != pkg.size || hex::encode(sha.finalize()) != pkg.sha256.to_ascii_lowercase() {
        bail!("artifact SHA256/size mismatch for {}", pkg.name);
    }
    Ok(total)
}

/// Revalidate signed repository provenance, selected exact identities, and
/// bytes on disk. This is an artifact audit, never an execution authorization.
pub fn verify(
    manifest: &Path, artifacts: &Path, signed_cache: &Path, native_arch: &str,
) -> Result<StoreAudit> {
    let data = fs::read(manifest)?;
    if data.len() > 16 * 1024 * 1024 { bail!("locked manifest exceeds 16 MiB"); }
    let requested: ExactManifest = serde_json::from_slice(&data)
        .context("parsing locked plan manifest")?;
    if requested.executable { bail!("manifest cannot authorize package execution"); }
    let mut steps = Vec::with_capacity(requested.packages.len());
    for pkg in &requested.packages {
        steps.push(PlanStep {
            package: pkg.artifact.name.clone(), architecture: pkg.architecture.clone(),
            version: pkg.version.clone(), installed: None,
        });
    }
    let trusted = package_provenance::prepare_exact(signed_cache, &steps, native_arch)?;
    if trusted.packages.len() != requested.packages.len() {
        bail!("signed artifact identity count changed");
    }
    let mut bytes = 0u64;
    for (want, signed) in requested.packages.iter().zip(trusted.packages.iter()) {
        if want.artifact.name != signed.artifact.name || want.architecture != signed.architecture
            || want.version != signed.version || want.artifact.sha256 != signed.artifact.sha256
            || want.artifact.size != signed.artifact.size
            || want.artifact.filename != signed.artifact.filename {
            bail!("manifest differs from current signed exact-version provenance");
        }
        bytes = bytes.checked_add(verify_file(artifacts, &want.artifact)?)
            .context("total verified byte count overflow")?;
    }
    Ok(StoreAudit {
        verified: requested.packages.len(), bytes, executable: false,
        warning: "Artifact bytes and signed provenance verified; no dpkg execution or APT parity guarantee.".into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verify_file_rejects_corrupted_artifact() {
        let dir = std::env::temp_dir().join(format!("rpt-artifact-audit-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let target = dir.join("package.deb");
        fs::write(&target, "x").unwrap();
        let package = Package {
            name: "testpkg".into(), url: "https://example.test/package.deb".into(),
            sha256: hex::encode(Sha256::digest(b"x")),
            size: 1, filename: "package.deb".into(),
        };
        assert_eq!(verify_file(&dir, &package).unwrap(), 1);
        fs::write(&target, "y").unwrap();
        assert!(verify_file(&dir, &package).is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

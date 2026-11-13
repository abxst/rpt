//! Exact signed install/upgrade intent execution in an RPT-labeled Docker
//! session. Never executes dpkg on the host or mounts the host root.
use super::*;

/// Reject stale plans before staging; repeat SHA256 preflight *inside* the
/// container immediately before dpkg. Postconditions are checked in the same
/// journal-supervised process, before a Succeeded phase may be persisted.
pub async fn apply_bound(
    session: &str, transaction: &Path, artifacts: &Path, cache: &Path,
    apt_root: &Path, journal: &Path, id: &str, timeout_seconds: u64,
) -> Result<()> {
    if !sandbox_execution::valid_id(id) {
        bail!("invalid sandbox transaction identifier");
    }
    let original = fs::read(transaction).context("opening bound sandbox transaction")?;
    if original.len() > 16 * 1024 * 1024 {
        bail!("bound transaction exceeds 16 MiB");
    }
    let bound: BoundTransaction = serde_json::from_slice(&original)?;
    if bound.schema_version != 1 || bound.session != session || bound.manifest.executable {
        bail!("unsupported or mismatched sandbox transaction intent");
    }
    if bound.steps.is_empty() || bound.manifest.packages.is_empty() {
        bail!("no package changes in bound transaction");
    }
    let (container, status) = status_snapshot(session).await?;
    if hex::encode(Sha256::digest(&status)) != bound.status_sha256 {
        bail!("stale sandbox transaction: dpkg status changed since planning");
    }
    // Detect changes in signed repository policy or hand-edited plan fields.
    let recomputed = plan_install_snapshot(
        &status, cache, apt_root, &bound.architecture,
        bound.max_packages, &bound.requested,
    )?;
    if recomputed != bound.steps {
        bail!("bound package plan differs from current APT candidate policy");
    }
    let trusted = package_provenance::prepare_exact(cache, &recomputed, &bound.architecture)?;
    if serde_json::to_value(&trusted.packages)? != serde_json::to_value(&bound.manifest.packages)? {
        bail!("signed artifacts differ from bound transaction intent");
    }
    let audited = artifact_store::verify(transaction, artifacts, cache, &bound.architecture)?;
    if audited.verified != bound.manifest.packages.len()
        || original != fs::read(transaction)? {
        bail!("bound transaction changed during artifact verification");
    }
    fs::create_dir_all(journal)?;
    let canonical = fs::canonicalize(journal)?;
    if canonical == Path::new("/") { bail!("journal cannot be filesystem root"); }
    let stages = canonical.join("sandbox-session-stages");
    if !stages.exists() { fs::create_dir(&stages)?; }
    if !fs::symlink_metadata(&stages)?.file_type().is_dir() {
        bail!("sandbox staging root must be a real directory");
    }
    let stage = stages.join(id);
    fs::create_dir(&stage).context("sandbox transaction stage already exists")?;
    struct StageGuard(PathBuf);
    impl Drop for StageGuard {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }
    let _guard = StageGuard(stage.clone());
    let staged = sandbox_execution::stage_artifacts(&stage, artifacts, &bound.manifest)?;
    let stage = fs::canonicalize(stage)?;
    let stage_str = stage.to_str().context("non-UTF8 sandbox stage")?;
    if stage_str.contains(':') || stage_str.contains('\n') {
        bail!("unsafe Docker cp staging path");
    }
    let guest_script = format!(
        "{BOUND_GUEST_PREFLIGHT}\n{DPKG_CHECK_SCRIPT}\n{DPKG_INSTALL_SCRIPT}\n{BOUND_GUEST_POSTFLIGHT}"
    );
    let mut args = [
        "-euc", HOST_INSTALL_SCRIPT, "rpt-session-host", &container,
        stage_str, &guest_script, "rpt-session-dpkg", &bound.status_sha256,
    ].iter().map(|v| (*v).to_owned()).collect::<Vec<_>>();
    args.extend(staged.into_iter().map(|file| format!("/rpt-input/{file}")));
    run_command(session, journal, id, "/bin/sh", args, timeout_seconds).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bound_manifest_is_compatible_with_existing_fetch() {
        let bound = BoundTransaction {
            schema_version: 1, session: "dev01".into(), architecture: "amd64".into(),
            status_sha256: "a".repeat(64), requested: vec!["demopkg".into()],
            max_packages: 20, steps: vec![],
            manifest: ExactManifest {
                packages: vec![], executable: false, warning: "test".into(),
            },
        };
        let bytes = serde_json::to_vec(&bound).unwrap();
        let fetch: crate::Manifest = serde_json::from_slice(&bytes).unwrap();
        assert!(fetch.packages.is_empty());
        let decoded: BoundTransaction = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(decoded.session, "dev01");
    }
    #[test]
    fn guest_preflight_and_postflight_protect_journal_success() {
        assert!(BOUND_GUEST_PREFLIGHT.contains("sha256sum /var/lib/dpkg/status"));
        assert!(BOUND_GUEST_POSTFLIGHT.contains("dpkg-query"));
        assert!(BOUND_GUEST_POSTFLIGHT.contains("dpkg --audit"));
        assert!(!BOUND_GUEST_PREFLIGHT.contains("--force"));
    }
}

//! M7: One supervised, journaled combined remove + signed upgrade
//! transaction on a disposable native rootfs only.
//! NEVER a production host executor. A rootfs chroot is NOT a security boundary.
use crate::{
    artifact_store,
    cache_query::{self, InstalledKey},
    dry_run_executor::DryRunExecutor,
    install_plan::PlanStep,
    native_rootfs,
    package_provenance::{self, ExactManifest},
    sandbox_execution,
    system_auto_upgrade::{self, AutoUpgradeSelection},
    system_full_upgrade::{self, JointUpgradeScenario},
};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::{BTreeMap, BTreeSet}, fs, path::{Path, PathBuf}, time::Duration};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BoundRemoval {
    pub name: String,
    pub architecture: String,
    pub installed_version: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct NativeCombinedIntent {
    pub schema_version: u32,
    pub session: String,
    pub architecture: String,
    pub status_sha256: String,
    pub marks_sha256: String,
    pub max_roots: usize,
    pub max_packages: usize,
    pub max_removals: usize,
    /// "explicit" or "automatic", never infer a mode from missing input.
    pub selection_mode: String,
    pub requested_removals: Vec<String>,
    pub selected_removals: Vec<BoundRemoval>,
    pub requested_upgrades: Vec<String>,
    pub scenario_sha256: String,
    pub steps: Vec<PlanStep>,
    #[serde(flatten)]
    pub manifest: ExactManifest,
}

fn digest(input: &[u8]) -> String { hex::encode(Sha256::digest(input)) }

/// Explicit typed preparation contract, also replayed at apply time.
pub struct Prepare<'a> {
    pub root: &'a Path,
    pub cache: &'a Path,
    pub apt_root: &'a Path,
    pub architecture: &'a str,
    pub mode: &'a str,
    pub requested: &'a [String],
    pub max_roots: usize,
    pub max_packages: usize,
    pub max_removals: usize,
}

fn scenario(opts: &Prepare<'_>, status: &Path) -> Result<JointUpgradeScenario> {
    let Prepare {
        cache, apt_root, architecture, mode, requested,
        max_roots, max_packages, max_removals, ..
    } = opts;
    match *mode {
        "explicit" => {
            if requested.is_empty() { bail!("explicit removal set must not be empty"); }
            system_full_upgrade::preview(cache, status, apt_root, architecture,
                requested, *max_roots, *max_packages)
        }
        "automatic" => {
            if !requested.is_empty() {
                bail!("automatic full-upgrade intent cannot also specify explicit removals");
            }
            let report = system_auto_upgrade::preview(cache, status, apt_root,
                architecture, *max_roots, *max_packages, *max_removals)?;
            match report.selection {
                AutoUpgradeSelection::RemovalScenario { scenario } => Ok(scenario),
                AutoUpgradeSelection::NoRemoval { .. } =>
                    bail!("no removals required; use native-rootfs-prepare-system-upgrade"),
            }
        }
        _ => bail!("unsupported native full-upgrade selection mode"),
    }
}

/// Produce a *nonexecutable* artifact manifest with exact provenance and
/// bind it to a globally checked, original-status full-upgrade scenario.
pub fn prepare(opts: Prepare<'_>) -> Result<NativeCombinedIntent> {
    let Prepare {
        root: requested_root, cache, apt_root, architecture, mode, requested,
        max_roots, max_packages, max_removals,
    } = &opts;
    let root = native_rootfs::rootfs(requested_root)?;
    if !(1..=128).contains(max_roots) || !(1..=5000).contains(max_packages)
        || !(1..=3).contains(max_removals) {
        bail!("invalid bounded native full-upgrade solver limits");
    }
    let status = native_rootfs::status(&root)?;
    // Current guest-native combined executor intentionally rejects
    // foreign-architecture and duplicated binary names. Silent selection of
    // a coinstalled :i386 binary would be an unacceptable production hazard.
    let installed = cache_query::installed_catalog_from_bytes(&status)?;
    let mut names = BTreeSet::new();
    for key in installed.keys() {
        if (key.architecture != *architecture && key.architecture != "all")
            || !names.insert(&key.name) {
            bail!("native combined upgrade requires unambiguous native/all-only dpkg state");
        }
    }
    let pre_sha = digest(&status);
    let scenario = scenario(&opts, &root.join("var/lib/dpkg/status"))
        .context("selecting the fully audited combined upgrade/removal scenario")?;
    if scenario.original_status_sha256 != pre_sha || scenario.removals.is_empty() {
        bail!("combined signed scenario is stale or contains no removals");
    }
    if scenario.upgrades.transaction.downgrades > 0
        || scenario.upgrades.transaction.reinstalls > 0
        || scenario.upgrades.requested_upgrades.is_empty() {
        bail!("combined upgrade requires real signed upgrades, no downgrades/reinstalls");
    }
    if scenario.removals.len() > *max_removals {
        bail!("selected scenario exceeds approved removal limit");
    }
    let selected_removals = scenario.removals.iter().map(|r| BoundRemoval {
        name: r.name.clone(), architecture: r.architecture.clone(),
        installed_version: r.installed_version.clone(),
    }).collect::<Vec<_>>();
    let steps = scenario.upgrades.transaction.changes.iter().map(|c| PlanStep {
        package: c.package.clone(), architecture: c.architecture.clone(),
        version: c.to.clone(), installed: c.from.clone(),
    }).collect::<Vec<_>>();
    let manifest = package_provenance::prepare_exact(cache, &steps, architecture)?;
    if manifest.packages.is_empty() || manifest.executable {
        bail!("combined upgrade must bind real signed artifacts to nonexecutable intent");
    }
    let marks_sha256 = native_rootfs::mark_state_digest(&root)?;
    if digest(&native_rootfs::status(&root)?) != pre_sha {
        bail!("guest dpkg status changed during combined intent preparation");
    }
    let scenario_sha256 = digest(&serde_json::to_vec(&scenario)?);
    Ok(NativeCombinedIntent {
        schema_version: 1, session: native_rootfs::root_identity(&root),
        architecture: (*architecture).into(), status_sha256: pre_sha,
        marks_sha256, max_roots: *max_roots, max_packages: *max_packages,
        max_removals: *max_removals,
        selection_mode: (*mode).into(), requested_removals: requested.to_vec(),
        selected_removals, requested_upgrades: scenario.upgrades.requested_upgrades,
        scenario_sha256, steps, manifest,
    })
}

/// Complete expected installed inventory from the ORIGINAL bound status.
/// Reject overlaps and any implicit package removal/reinstall outside the
/// scenario. Output is sorted and compared to the real guest dpkg inventory
/// after all scripts and triggers have run, before declaring success.
fn expected_inventory(
    status: &[u8], native: &str, intent: &NativeCombinedIntent,
) -> Result<Vec<u8>> {
    let mut projected = cache_query::installed_catalog_from_bytes(status)?
        .into_iter().map(|(k,v)| (k, v.version))
        .collect::<BTreeMap<InstalledKey, String>>();
    for rem in &intent.selected_removals {
        if rem.architecture != native && rem.architecture != "all" {
            bail!("foreign removal is not supported by native combined executor");
        }
        let key = InstalledKey {
            name: rem.name.clone(), architecture: rem.architecture.clone(),
        };
        if projected.remove(&key).as_deref() != Some(rem.installed_version.as_str()) {
            bail!("stale or duplicate native combined removal in bound intent");
        }
    }
    for step in &intent.steps {
        let key = InstalledKey {
            name: step.package.clone(), architecture: step.architecture.clone(),
        };
        if projected.get(&key).map(String::as_str) != step.installed.as_deref() {
            bail!("combined signed step installed-version preflight mismatch");
        }
        projected.insert(key, step.version.clone());
    }
    let mut output = String::new();
    for (key,version) in projected {
        for field in [&key.name, &key.architecture, &version] {
            if field.contains(['\t', '\r', '\n']) {
                bail!("unsafe combined installed inventory field");
            }
        }
        output.push_str(&format!("{}\t{}\t{}\n",key.name,key.architecture,version));
    }
    Ok(output.into_bytes())
}

pub struct Apply<'a> {
    pub rootfs: &'a Path,
    pub transaction: &'a Path,
    pub artifacts: &'a Path,
    pub cache: &'a Path,
    pub apt_root: &'a Path,
    pub journal: &'a Path,
    pub id: &'a str,
    pub timeout_seconds: u64,
    /// A verified, new full-tree checkpoint is MANDATORY for combined
    /// remove/install operations. No automatic rollback is implied.
    pub checkpoint_rootfs: &'a Path,
    /// Exact SHA256 of the full intent file. Explicit operator approval
    /// is necessary but not sufficient to authorize any unsafe guest.
    pub approved_intent_sha256: &'a str,
}

/// Apply *only to a marked disposable guest rootfs*, with exact intent,
/// artifact and status revalidation, mandatory checkpoint and one journal.
/// Never runs the remove and install phases as separate independent jobs.
pub async fn apply(opts: Apply<'_>) -> Result<()> {
    if !sandbox_execution::valid_id(opts.id) || !(10..=3600).contains(&opts.timeout_seconds) {
        bail!("invalid native combined transaction ID or timeout");
    }
    let root = native_rootfs::rootfs(opts.rootfs)?;
    let meta = fs::symlink_metadata(opts.transaction)?;
    if !meta.file_type().is_file() || meta.len() > 16 * 1024 * 1024 {
        bail!("combined native intent must be a regular file <=16 MiB");
    }
    let intent_bytes = fs::read(opts.transaction)?;
    if digest(&intent_bytes) != opts.approved_intent_sha256 {
        bail!("explicit combined transaction approval does not match intent SHA256");
    }
    let bound: NativeCombinedIntent = serde_json::from_slice(&intent_bytes)?;
    if bound.schema_version != 1 || bound.session != native_rootfs::root_identity(&root)
        || bound.manifest.executable || bound.selected_removals.is_empty()
        || bound.steps.is_empty() || bound.manifest.packages.is_empty() {
        bail!("invalid or wrong-rootfs combined full-upgrade intent");
    }
    let before = native_rootfs::status(&root)?;
    if digest(&before) != bound.status_sha256
        || native_rootfs::mark_state_digest(&root)? != bound.marks_sha256 {
        bail!("combined dpkg status or APT marks changed since intent preparation");
    }
    // Re-run the complete signed candidate policy, bounded removal selection,
    // original-state reverse dependencies and exact manifest generation.
    let replay = prepare(Prepare {
        root: &root, cache: opts.cache, apt_root: opts.apt_root,
        architecture: &bound.architecture, mode: &bound.selection_mode,
        requested: &bound.requested_removals, max_roots: bound.max_roots,
        max_packages: bound.max_packages, max_removals: bound.max_removals,
    }).context("signed full-upgrade intent replay failed before guest mutation")?;
    if serde_json::to_value(&replay)? != serde_json::to_value(&bound)? {
        bail!("combined transaction changed since signed/policy preflight");
    }
    let verified = artifact_store::verify(
        opts.transaction, opts.artifacts, opts.cache, &bound.architecture,
    )?;
    if verified.verified != bound.manifest.packages.len()
        || fs::read(opts.transaction)? != intent_bytes {
        bail!("combined signed artifact verification or intent integrity failed");
    }
    if !root.join("usr/bin/apt-mark").exists() {
        bail!("disposable guest has no apt-mark");
    }
    fs::create_dir_all(opts.journal)?;
    let journal = fs::canonicalize(opts.journal)?;
    if journal == Path::new("/") || journal.starts_with(&root) {
        bail!("combined journal must be outside the disposable rootfs");
    }
    let stages = journal.join("native-rootfs-stages");
    fs::create_dir_all(&stages)?;
    if !fs::symlink_metadata(&stages)?.file_type().is_dir() {
        bail!("combined staging directory must not be a symlink");
    }
    let stage = stages.join(opts.id);
    fs::create_dir(&stage).context("combined stage already exists; manual recovery required")?;
    struct StageGuard(PathBuf);
    impl Drop for StageGuard {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }
    let _stage_guard = StageGuard(stage.clone());
    let artifacts = sandbox_execution::stage_artifacts(
        &stage, opts.artifacts, &bound.manifest,
    )?;
    let mut removals = String::new();
    for r in &bound.selected_removals {
        // The signed projected-state audit resolves exact name+architecture,
        // but dpkg --remove receives only the unambiguous native binary name.
        if r.name.is_empty() || r.name.contains(['\n','\t','\r'])
            || r.installed_version.contains(['\n','\t','\r']) {
            bail!("unsafe removal TSV content");
        }
        removals.push_str(&format!(
            "{}\t{}\t{}\n", r.name, r.installed_version, r.architecture,
        ));
    }
    fs::write(stage.join("removals.tsv"), removals)?;
    fs::write(stage.join("expected-installed.tsv"),
        expected_inventory(&before, &bound.architecture, &bound)?)?;
    let (auto, manual) = native_rootfs::install_mark_targets(
        &bound.requested_upgrades, &bound.steps,
    );
    native_rootfs::write_mark_targets(&stage.join("auto-marks.txt"), &auto)?;
    native_rootfs::write_mark_targets(&stage.join("manual-marks.txt"), &manual)?;
    // A checkpoint is mandatory and checked against the bound dpkg and APT
    // mark digests before the one supervised dpkg command can be submitted.
    native_rootfs::checkpoint_before(
        &root, &journal, opts.id, Some(opts.checkpoint_rootfs),
        &before, Some(&bound.marks_sha256),
    )?;
    if fs::read(opts.transaction)? != intent_bytes
        || digest(&native_rootfs::status(&root)?) != bound.status_sha256
        || native_rootfs::mark_state_digest(&root)? != bound.marks_sha256 {
        bail!("combined guest state/intent changed before execution");
    }
    let stage = fs::canonicalize(stage)?;
    let mut args = vec![
        "-euc".into(), native_rootfs::INSTALL_SCRIPT.into(),
        "rpt-native-combined".into(), root.to_string_lossy().into_owned(),
        bound.status_sha256, stage.to_string_lossy().into_owned(),
        bound.marks_sha256,
    ];
    args.extend(artifacts.iter().map(|file|
        stage.join(file).to_string_lossy().into_owned()));
    DryRunExecutor::new(journal, 1)?.submit_command(
        opts.id.to_owned(), "/bin/sh".into(), args,
        Duration::from_secs(opts.timeout_seconds), Duration::from_secs(5),
    ).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guest_removal_validation_requires_bound_original_versions() {
        let status = b"Package: oldpkg\nStatus: install ok installed\nVersion: 1\nArchitecture: amd64\n\n\
Package: keeppkg\nStatus: install ok installed\nVersion: 1\nArchitecture: amd64\n\n";
        let rem = BoundRemoval {
            name: "oldpkg".into(), architecture: "amd64".into(),
            installed_version: "1".into(),
        };
        let mut intent = NativeCombinedIntent {
            schema_version: 1, session: "test".into(), architecture: "amd64".into(),
            status_sha256: digest(status), marks_sha256: "missing".into(),
            max_roots: 128, max_packages: 200, max_removals: 3,
            selection_mode: "explicit".into(), requested_removals: vec!["oldpkg".into()],
            selected_removals: vec![rem.clone()], requested_upgrades: vec!["keeppkg".into()],
            scenario_sha256: "a".repeat(64), steps: vec![PlanStep {
                package: "keeppkg".into(), architecture: "amd64".into(),
                version: "2".into(), installed: Some("1".into()),
            }],
            manifest: ExactManifest {
                packages: Vec::new(), executable: false, warning: "fixture".into(),
            },
        };
        let expected = String::from_utf8(expected_inventory(status, "amd64", &intent).unwrap()).unwrap();
        assert_eq!(expected, "keeppkg\tamd64\t2\n");
        intent.selected_removals[0].installed_version = "0".into();
        assert!(expected_inventory(status, "amd64", &intent).is_err());
        intent.selected_removals[0] = rem;
        intent.steps[0].installed = Some("0".into());
        assert!(expected_inventory(status, "amd64", &intent).is_err());
        intent.steps[0].installed = Some("1".into());
        intent.selected_removals[0].name = "keeppkg".into();
        assert!(expected_inventory(status, "amd64", &intent).is_err());
    }

    #[test]
    fn combined_executor_requires_exact_approval_and_guest() {
        assert!(native_rootfs::rootfs(Path::new("/")).is_err());
        let intent = b"test";
        assert_ne!(digest(intent), digest(b"tampered"));
        assert!(native_rootfs::INSTALL_SCRIPT.contains("removals.tsv"));
        assert!(native_rootfs::INSTALL_SCRIPT.contains("expected-installed.tsv"));
    }
}

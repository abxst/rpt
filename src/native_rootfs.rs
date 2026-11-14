//! Signed dpkg transactions on an explicitly marked disposable Linux rootfs.
//! This is independent of Docker, not an executor for the live machine.
use crate::{artifact_store, dry_run_executor::DryRunExecutor, install_plan, package_provenance,
    sandbox_execution::{self, valid_id}, sandbox_session::BoundTransaction};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{collections::BTreeSet, fs, path::{Path, PathBuf}, time::Duration};

const MARKER: &str = "rpt-disposable-rootfs-v1\n";

fn rootfs(path: &Path) -> Result<PathBuf> {
    if !cfg!(target_os = "linux") { bail!("native rootfs requires Linux"); }
    if fs::symlink_metadata(path)?.file_type().is_symlink() { bail!("rootfs cannot be a symlink"); }
    let root = fs::canonicalize(path)?;
    if !root.is_dir() || !(root.starts_with("/tmp/") || root.starts_with("/var/tmp/")) {
        bail!("only disposable rootfs beneath /tmp or /var/tmp is supported; refusing live root");
    }
    let marker = root.join(".rpt-disposable-rootfs");
    if !fs::symlink_metadata(&marker)?.file_type().is_file()
        || fs::read(&marker)? != MARKER.as_bytes() {
        bail!("missing exact disposable-rootfs marker");
    }
    for path in ["var/lib/dpkg/status", "usr/bin/dpkg", "bin/sh"] {
        if !root.join(path).exists() { bail!("incomplete rootfs: {path}"); }
    }
    // Refuse bind mounts, proc/sys/dev mounts, and external writable volumes.
    let mount_prefix = format!("{}/", root.display());
    for line in fs::read_to_string("/proc/self/mountinfo")?.lines() {
        let Some(target) = line.split_whitespace().nth(4) else { continue };
        if target == root.to_string_lossy() || target.starts_with(&mount_prefix) {
            bail!("rootfs contains a mount: {target}");
        }
    }
    Ok(root)
}
fn root_identity(root: &Path) -> String { format!("native-rootfs:{}", root.display()) }
fn status(root: &Path) -> Result<Vec<u8>> {
    let path = root.join("var/lib/dpkg/status");
    if !fs::symlink_metadata(&path)?.file_type().is_file() { bail!("dpkg status must be a regular file"); }
    fs::read(path).context("reading guest dpkg status")
}

/// Installed user roots must become manual, and only newly installed
/// dependency archives become automatic. Never relabel an existing dependency:
/// it may have been explicitly installed by an operator before this transaction.
fn install_mark_targets(requested: &[String], steps: &[install_plan::PlanStep])
    -> (Vec<String>, Vec<String>) {
    let roots = requested.iter().cloned().collect::<BTreeSet<_>>();
    let automatic = steps.iter().filter(|step|
        step.installed.is_none() && !roots.contains(&step.package))
        .map(|step| step.package.clone()).collect::<BTreeSet<_>>();
    (automatic.into_iter().collect(), roots.into_iter().collect())
}

/// Exact guest APT mark-state identity, sampled before a dpkg transaction.
/// The executor checks it again immediately before touching dpkg.
fn mark_state_digest(root: &Path) -> Result<String> {
    let marks = root.join("var/lib/apt/extended_states");
    match fs::symlink_metadata(&marks) {
        Ok(meta) if meta.file_type().is_file() && meta.len() <= 64 * 1024 * 1024 =>
            Ok(hex::encode(Sha256::digest(fs::read(&marks)?))),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok("missing".into()),
        _ => bail!("guest APT marks must be absent or a regular file <=64 MiB"),
    }
}

fn write_mark_targets(path: &Path, targets: &[String]) -> Result<()> {
    let mut contents = String::new();
    for package in targets {
        // apt-mark arguments come from the verified solver, but the staged
        // text protocol must also reject malformed names and line breaks.
        if package.len() < 2 || !package.bytes().all(|c|
            c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, b'+' | b'.' | b'-')) {
            bail!("invalid package identity in native APT marking plan: {package}");
        }
        contents.push_str(package);
        contents.push('\n');
    }
    fs::write(path, contents)?;
    Ok(())
}

/// Reuse the exact signed candidate-policy and download intent contract.
pub fn prepare(root: &Path, cache: &Path, apt_root: &Path, architecture: &str,
    max_packages: usize, packages: &[String], include_recommends: bool) -> Result<BoundTransaction> {
    let root = rootfs(root)?;
    let bytes = status(&root)?;
    let steps = install_plan::preview_many_with_policy_options(
        cache, &root.join("var/lib/dpkg/status"), apt_root, packages,
        architecture, max_packages, include_recommends)?;
    let manifest = package_provenance::prepare_exact(cache, &steps, architecture)?;
    Ok(BoundTransaction {
        schema_version: 1, session: root_identity(&root), architecture: architecture.into(),
        status_sha256: hex::encode(Sha256::digest(&bytes)), requested: packages.to_vec(),
        max_packages, include_recommends, steps, manifest,
    })
}

const INSTALL_SCRIPT: &str = r#"set -eu
root="$1"; expected="$2"; stage="$3"; expected_marks="$4"; shift 4
test -f "$root/.rpt-disposable-rootfs"
test "$(cat "$root/.rpt-disposable-rootfs")" = "rpt-disposable-rootfs-v1"
actual="$(sha256sum "$root/var/lib/dpkg/status" | cut -d ' ' -f1)"
if [ "$expected" != "$actual" ]; then
    echo "stale rootfs dpkg state" >&2; exit 1
fi
if [ -n "$(dpkg --root="$root" --audit)" ]; then
    echo "rootfs dpkg database is already broken" >&2; exit 1
fi
marks="$root/var/lib/apt/extended_states"
if [ "$expected_marks" = missing ]; then
    if [ -e "$marks" ] || [ -L "$marks" ]; then
        echo "guest APT mark state appeared before install" >&2; exit 1
    fi
elif [ ! -f "$marks" ] || [ -L "$marks" ] ||
     [ "$(sha256sum "$marks" | cut -d ' ' -f1)" != "$expected_marks" ]; then
    echo "guest APT mark state changed before install" >&2; exit 1
fi
tab="$(printf '\t')"
while IFS="$tab" read -r file package version arch; do
    archive="$stage/$file"
    if [ "$(dpkg-deb -f "$archive" Package)" != "$package" ] ||
       [ "$(dpkg-deb -f "$archive" Version)" != "$version" ] ||
       [ "$(dpkg-deb -f "$archive" Architecture)" != "$arch" ]; then
        echo "archive control mismatches signed plan" >&2; exit 1
    fi
done < "$stage/expected.tsv"
# If the verified plan has no .deb archives, this is an APT mark-only
# transaction: never trigger dpkg configure/pending or any maintainer script.
if [ "$#" -gt 0 ]; then
    for archive in "$@"; do
        dpkg --root="$root" --install -- "$archive"
    done
    dpkg --root="$root" --configure --pending
    dpkg --root="$root" --triggers-only --pending
fi
if [ -n "$(dpkg --root="$root" --audit)" ]; then
    echo "rootfs dpkg postflight audit failed" >&2; exit 1
fi
while IFS="$tab" read -r file package version arch; do
    actual="$(dpkg-query --admindir="$root/var/lib/dpkg" -W '-f=${Status}|${Version}|${Architecture}' "$package")"
    if [ "$actual" != "install ok installed|$version|$arch" ]; then
        echo "rootfs postflight version mismatch: $package" >&2; exit 1
    fi
done < "$stage/expected.tsv"

# Mark only newly installed dependency archives as automatic. Existing
# dependencies keep their previous marks, including manual state. Explicit
# request roots are always manual, even if they were already installed.
if [ ! -x "$root/usr/bin/apt-mark" ]; then
    echo "guest apt-mark binary missing" >&2; exit 1
fi
status_after_install="$(sha256sum "$root/var/lib/dpkg/status" | cut -d ' ' -f1)"
set --
while IFS= read -r package; do
    if [ -n "$package" ]; then set -- "$@" "$package"; fi
done < "$stage/auto-marks.txt"
if [ "$#" -gt 0 ]; then
    chroot "$root" /usr/bin/apt-mark auto "$@"
fi
for package in "$@"; do
    chroot "$root" /usr/bin/apt-mark showauto "$package" | grep -Fxq "$package" ||
        { echo "native auto mark missing for $package" >&2; exit 1; }
done
set --
while IFS= read -r package; do
    if [ -n "$package" ]; then set -- "$@" "$package"; fi
done < "$stage/manual-marks.txt"
# Never allow a mark-only intent to mark an uninstalled or ambiguous root.
# The policy replan and snapshot binding already enforce exact candidate
# selection; verify the guest dpkg state again before applying marks.
for package in "$@"; do
    installed="$(dpkg-query --admindir="$root/var/lib/dpkg" -W \
        '-f=${Status}' "$package" 2>/dev/null)" || installed=""
    case "$installed" in
        "install ok installed"|"hold ok installed") ;;
        *) echo "native manual root is not installed: $package" >&2; exit 1 ;;
    esac
done
if [ "$#" -gt 0 ]; then
    chroot "$root" /usr/bin/apt-mark manual "$@"
fi
for package in "$@"; do
    chroot "$root" /usr/bin/apt-mark showmanual "$package" | grep -Fxq "$package" ||
        { echo "native manual mark missing for $package" >&2; exit 1; }
done
if [ "$(sha256sum "$root/var/lib/dpkg/status" | cut -d ' ' -f1)" != "$status_after_install" ]; then
    echo "native APT marking unexpectedly changed dpkg state" >&2; exit 1
fi
if [ -n "$(dpkg --root="$root" --audit)" ]; then
    echo "native install/mark postflight dpkg audit failed" >&2; exit 1
fi
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApplyOutcome {
    Installed,
    MarksOnly,
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
}

/// Failure leaves the rootfs and journal for operator inspection; no
/// filesystem rollback is implied by successful process supervision.
pub async fn apply(opts: Apply<'_>) -> Result<ApplyOutcome> {
    if !valid_id(opts.id) || !(10..=3600).contains(&opts.timeout_seconds) {
        bail!("invalid native transaction ID or timeout");
    }
    let root = rootfs(opts.rootfs)?;
    let metadata = fs::symlink_metadata(opts.transaction)?;
    if !metadata.file_type().is_file() || metadata.len() > 16 * 1024 * 1024 {
        bail!("native intent must be a regular file <=16 MiB");
    }
    let original = fs::read(opts.transaction)?;
    let bound: BoundTransaction = serde_json::from_slice(&original)?;
    if bound.schema_version != 1 || bound.session != root_identity(&root)
        || bound.manifest.executable
        || bound.steps.is_empty() != bound.manifest.packages.is_empty() {
        bail!("wrong rootfs, inconsistent or unsupported native transaction");
    }
    let mark_only = bound.steps.is_empty();
    let guest_status = status(&root)?;
    if hex::encode(Sha256::digest(&guest_status)) != bound.status_sha256 {
        bail!("native transaction stale: dpkg status changed");
    }
    if mark_only {
        // Reject empty/ambiguous/uninstalled request roots. The signed
        // solver must additionally prove that no package changes are due.
        crate::sandbox_session::resolve_mark_packages(&guest_status, &bound.requested)?;
    }
    let steps = install_plan::preview_many_with_policy_options(
        opts.cache, &root.join("var/lib/dpkg/status"), opts.apt_root,
        &bound.requested, &bound.architecture, bound.max_packages, bound.include_recommends)?;
    if steps != bound.steps { bail!("native plan changed since signing/policy resolution"); }
    let trusted = package_provenance::prepare_exact(opts.cache, &steps, &bound.architecture)?;
    if serde_json::to_value(&trusted.packages)? != serde_json::to_value(&bound.manifest.packages)? {
        bail!("native artifact identities differ from signed repository");
    }
    let verified = artifact_store::verify(opts.transaction, opts.artifacts, opts.cache, &bound.architecture)?;
    if verified.verified != bound.manifest.packages.len() || original != fs::read(opts.transaction)? {
        bail!("native artifact verification or intent integrity failed");
    }
    fs::create_dir_all(opts.journal)?;
    let journal = fs::canonicalize(opts.journal)?;
    if journal == Path::new("/") || journal.starts_with(&root) {
        bail!("native transaction journal must be outside rootfs");
    }
    let stages = journal.join("native-rootfs-stages");
    fs::create_dir_all(&stages)?;
    if !fs::symlink_metadata(&stages)?.file_type().is_dir() { bail!("staging root cannot be symlink"); }
    let stage = stages.join(opts.id);
    fs::create_dir(&stage).context("native stage exists; inspect previous attempt")?;
    struct StageGuard(PathBuf);
    impl Drop for StageGuard { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
    let _guard = StageGuard(stage.clone());
    let names = if mark_only {
        // No artifacts are needed and callers need not create a dummy cache
        // directory. Still create the expected empty transcript for the
        // same script, status preflight, mark checks, and journal.
        fs::write(stage.join("expected.tsv"), b"")?;
        Vec::new()
    } else {
        sandbox_execution::stage_artifacts(&stage, opts.artifacts, &bound.manifest)?
    };
    if !root.join("usr/bin/apt-mark").exists() {
        bail!("native disposable rootfs does not include apt-mark");
    }
    let expected_marks = mark_state_digest(&root)?;
    let (automatic, manual) = install_mark_targets(&bound.requested, &bound.steps);
    write_mark_targets(&stage.join("auto-marks.txt"), &automatic)?;
    write_mark_targets(&stage.join("manual-marks.txt"), &manual)?;
    let stage = fs::canonicalize(stage)?;
    let mut args = vec![
        "-euc".into(), INSTALL_SCRIPT.into(), "rpt-native-rootfs".into(),
        root.to_string_lossy().into_owned(), bound.status_sha256,
        stage.to_string_lossy().into_owned(), expected_marks,
    ];
    args.extend(names.iter().map(|name| stage.join(name).to_string_lossy().into_owned()));
    DryRunExecutor::new(journal, 1)?.submit_command(
        opts.id.to_owned(), "/bin/sh".into(), args,
        Duration::from_secs(opts.timeout_seconds), Duration::from_secs(5),
    ).await?;
    Ok(if mark_only { ApplyOutcome::MarksOnly } else { ApplyOutcome::Installed })
}


/// Native remove, purge, and autoremove share the same rootfs guard, durable
/// transaction journal and supervision as signed install/upgrade.
const REMOVE_SCRIPT: &str = r#"set -eu
root="$1"; expected="$2"; mode="$3"; expected_marks="$4"; shift 4
test "$(cat "$root/.rpt-disposable-rootfs")" = "rpt-disposable-rootfs-v1"
actual="$(sha256sum "$root/var/lib/dpkg/status" | cut -d ' ' -f1)"
if [ "$actual" != "$expected" ]; then
    echo "stale native rootfs removal plan" >&2; exit 1
fi
if [ "$expected_marks" != "ignore" ]; then
    file="$root/var/lib/apt/extended_states"
    if [ ! -f "$file" ] || [ -L "$file" ] ||
       [ "$(sha256sum "$file" | cut -d ' ' -f1)" != "$expected_marks" ]; then
        echo "APT auto marks changed since autoremove plan" >&2; exit 1
    fi
fi
if [ -n "$(dpkg --root="$root" --audit)" ]; then
    echo "native rootfs dpkg audit failed before removal" >&2; exit 1
fi
case "$mode" in
    remove) dpkg --root="$root" --remove -- "$@" ;;
    purge) dpkg --root="$root" --purge -- "$@" ;;
    *) echo "unsupported removal mode" >&2; exit 1 ;;
esac
if [ -n "$(dpkg --root="$root" --audit)" ]; then
    echo "native rootfs dpkg audit failed after removal" >&2; exit 1
fi
for package in "$@"; do
    installed="$(dpkg-query --admindir="$root/var/lib/dpkg" -W \
        '-f=${Status}' "$package" 2>/dev/null)" || installed=""
    if [ "$installed" = "install ok installed" ]; then
        echo "package remained installed after native rootfs removal: $package" >&2
        exit 1
    fi
    # A successful purge must leave no residual conffile-only dpkg state.
    # This is stricter than remove, which deliberately retains conffiles.
    if [ "$mode" = purge ] && [ -n "$installed" ] &&
       [ "$installed" != "unknown ok not-installed" ]; then
        echo "package remained in dpkg database after native rootfs purge: $package ($installed)" >&2
        exit 1
    fi
done
"#;

/// Explicit removal is conservatively planned from the *guest* installed
/// state. A refuse/blocker never reaches dpkg, including Essential packages.
pub struct Remove<'a> {
    pub rootfs: &'a Path,
    pub journal: &'a Path,
    pub id: &'a str,
    pub architecture: &'a str,
    pub packages: &'a [String],
    pub cascade: bool,
    pub purge: bool,
    pub timeout_seconds: u64,
}
pub async fn remove_planned(opts: Remove<'_>) -> Result<crate::removal_planner::RemovalPlan> {
    let root = rootfs(opts.rootfs)?;
    let before = status(&root)?;
    let plan = if opts.purge {
        crate::removal_planner::purge_snapshot(
            &before, opts.architecture, opts.packages, opts.cascade)?
    } else {
        crate::removal_planner::remove_snapshot(
            &before, opts.architecture, opts.packages, opts.cascade)?
    };
    if !plan.removable { bail!("unsafe native removal: {}", plan.blockers.join("; ")); }
    execute_remove(&root, Run {
        journal: opts.journal, id: opts.id, timeout_seconds: opts.timeout_seconds,
    }, &before, None, &plan.remove, opts.purge).await?;
    // The intent is a preview (executable=false); the separate executor
    // has already journaled and audited the selected removal/purge operation.
    Ok(plan)
}

/// Orphan cleanup uses the same guest dpkg status and guest auto-mark
/// database. A nonexistent mark database is an error, not an empty set.
pub async fn autoremove_planned(root: &Path, journal: &Path, id: &str,
    architecture: &str, timeout_seconds: u64)
    -> Result<crate::removal_planner::RemovalPlan> {
    let root = rootfs(root)?;
    let before = status(&root)?;
    let marks_path = root.join("var/lib/apt/extended_states");
    let meta = fs::symlink_metadata(&marks_path)?;
    if !meta.file_type().is_file() || meta.len() > 64 * 1024 * 1024 {
        bail!("guest APT marks must be a regular file <=64 MiB");
    }
    let marks = fs::read(marks_path)?;
    let plan = crate::removal_planner::autoremove_snapshot(&before, &marks, architecture)?;
    if !plan.removable { bail!("native autoremove denied: {}", plan.blockers.join("; ")); }
    if !plan.remove.is_empty() {
        execute_remove(&root, Run { journal, id, timeout_seconds }, &before,
            Some(&marks), &plan.remove, false).await?;
    }
    Ok(plan)
}

struct Run<'a> {
    journal: &'a Path,
    id: &'a str,
    timeout_seconds: u64,
}
async fn execute_remove(root: &Path, run: Run<'_>,
    before: &[u8], marks: Option<&[u8]>, packages: &[String],
    purge: bool) -> Result<()> {
    if !valid_id(run.id) || !(10..=3600).contains(&run.timeout_seconds)
        || packages.is_empty() || packages.len() > 128 {
        bail!("invalid native rootfs removal id, timeout or closure size");
    }
    fs::create_dir_all(run.journal)?;
    let journal = fs::canonicalize(run.journal)?;
    if journal == Path::new("/") || journal.starts_with(root) {
        bail!("native removal journal must be outside rootfs");
    }
    let digest = hex::encode(Sha256::digest(before));
    let marks_hash = marks.map_or_else(
        || "ignore".to_owned(), |bytes| hex::encode(Sha256::digest(bytes)));
    let mode = if purge { "purge" } else { "remove" };
    let mut args = vec![
        "-euc".into(), REMOVE_SCRIPT.into(), "rpt-native-remove".into(),
        root.to_string_lossy().into_owned(), digest, mode.into(), marks_hash,
    ];
    args.extend(packages.iter().cloned());
    DryRunExecutor::new(journal, 1)?.submit_command(
        run.id.to_owned(), "/bin/sh".into(), args,
        Duration::from_secs(run.timeout_seconds), Duration::from_secs(5),
    ).await
}


/// Mark auto/manual through the real APT binary inside the disposable rootfs,
/// rather than editing a host or guessed extended-states database.
const MARK_SCRIPT: &str = r#"set -eu
root="$1"; expected="$2"; mark_digest="$3"; mode="$4"; shift 4
test "$(cat "$root/.rpt-disposable-rootfs")" = "rpt-disposable-rootfs-v1"
if [ "$(sha256sum "$root/var/lib/dpkg/status" | cut -d ' ' -f1)" != "$expected" ]; then
    echo "native dpkg status changed before marking" >&2; exit 1
fi
marks="$root/var/lib/apt/extended_states"
if [ "$mark_digest" = "missing" ]; then
    if [ -e "$marks" ] || [ -L "$marks" ]; then
        echo "native APT marks appeared since planning" >&2; exit 1
    fi
elif [ ! -f "$marks" ] || [ -L "$marks" ] ||
     [ "$(sha256sum "$marks" | cut -d ' ' -f1)" != "$mark_digest" ]; then
    echo "native APT marks changed since planning" >&2; exit 1
fi
case "$mode" in auto|manual) ;; *) exit 2;; esac
chroot "$root" /usr/bin/apt-mark "$mode" "$@"
if [ "$(sha256sum "$root/var/lib/dpkg/status" | cut -d ' ' -f1)" != "$expected" ]; then
    echo "native apt-mark unexpectedly changed dpkg status" >&2; exit 1
fi
for package in "$@"; do
    if ! chroot "$root" /usr/bin/apt-mark "show$mode" "$package" |
         grep -Fxq "$package"; then
        echo "native apt-mark postflight failed for $package" >&2; exit 1
    fi
done
if [ -n "$(dpkg --root="$root" --audit)" ]; then
    echo "native APT marking left broken dpkg state" >&2; exit 1
fi
"#;

pub async fn mark(root: &Path, journal_path: &Path, id: &str, mode: &str,
    packages: &[String], timeout_seconds: u64) -> Result<()> {
    if !matches!(mode, "auto" | "manual") || !valid_id(id)
        || !(10..=3600).contains(&timeout_seconds) {
        bail!("invalid native APT mark mode, ID or timeout");
    }
    let root = rootfs(root)?;
    let before = status(&root)?;
    let names = crate::sandbox_session::resolve_mark_packages(&before, packages)?;
    if !root.join("usr/bin/apt-mark").exists() {
        bail!("native disposable rootfs does not include apt-mark");
    }
    let digest = mark_state_digest(&root)?;
    fs::create_dir_all(journal_path)?;
    let journal = fs::canonicalize(journal_path)?;
    if journal == Path::new("/") || journal.starts_with(&root) {
        bail!("native APT marking journal must be outside rootfs");
    }
    let mut args = vec![
        "-euc".into(), MARK_SCRIPT.into(), "rpt-native-mark".into(),
        root.to_string_lossy().into_owned(), hex::encode(Sha256::digest(&before)),
        digest, mode.into(),
    ];
    args.extend(names);
    DryRunExecutor::new(journal, 1)?.submit_command(
        id.to_owned(), "/bin/sh".into(), args,
        Duration::from_secs(timeout_seconds), Duration::from_secs(5),
    ).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn live_root_and_unmarked_tree_are_refused() {
        assert!(rootfs(Path::new("/")).is_err());
        assert!(rootfs(Path::new("/etc")).is_err());
        let dir = tempfile::tempdir().unwrap();
        assert!(rootfs(dir.path()).is_err());
    }
    #[test]
    fn native_install_auto_manual_marks_never_demote_preinstalled_dependencies() {
        let requested = vec!["rpt-app".to_owned(), "rpt-already-installed".to_owned()];
        let steps = vec![
            install_plan::PlanStep {
                package: "rpt-new-dep".into(), architecture: "all".into(),
                version: "1".into(), installed: None,
            },
            install_plan::PlanStep {
                package: "rpt-upgraded-dep".into(), architecture: "amd64".into(),
                version: "2".into(), installed: Some("1".into()),
            },
            install_plan::PlanStep {
                package: "rpt-app".into(), architecture: "all".into(),
                version: "1".into(), installed: None,
            },
        ];
        let (auto, manual) = install_mark_targets(&requested, &steps);
        assert_eq!(auto, ["rpt-new-dep"]);
        assert_eq!(manual, ["rpt-already-installed", "rpt-app"]);
        assert!(!auto.contains(&"rpt-upgraded-dep".to_owned()));
    }

    #[test]
    fn native_install_staged_mark_names_reject_injection() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("marks.txt");
        assert!(write_mark_targets(&file, &["lib-dep1".into(), "app".into()]).is_ok());
        assert_eq!(fs::read_to_string(&file).unwrap(), "lib-dep1\napp\n");
        assert!(write_mark_targets(&file, &["bad\nname".into()]).is_err());
        assert!(write_mark_targets(&file, &["bad_name".into()]).is_err());
    }

    #[test]
    fn mark_only_install_script_never_unconditionally_configures_dpkg() {
        assert!(INSTALL_SCRIPT.contains("if [ \"$#\" -gt 0 ]; then\n    for archive"));
        assert!(INSTALL_SCRIPT.contains("native manual root is not installed"));
        assert!(INSTALL_SCRIPT.contains("guest APT mark state changed before install"));
        assert!(INSTALL_SCRIPT.contains("native APT marking unexpectedly changed dpkg state"));
    }

    #[test]
    fn native_script_revalidates_and_audits() {
        for term in ["dpkg --root=", "dpkg-deb -f", "expected.tsv", "postflight version mismatch"] {
            assert!(INSTALL_SCRIPT.contains(term));
        }
        assert!(!INSTALL_SCRIPT.contains("--force"));
    }
}

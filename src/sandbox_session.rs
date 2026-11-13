//! Persistent *isolated* Docker package sessions for functional dpkg testing.
//! Container root may persist across commands, but the host dpkg database
//! and root filesystem are never mounted or modified.
use crate::{
    artifact_store, dry_run_executor::DryRunExecutor, package_provenance::ExactManifest,
    removal_planner::{self, RemovalPlan},
    sandbox_execution::{self, DPKG_CHECK_SCRIPT, DPKG_INSTALL_SCRIPT},
};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}, time::Duration};
use tokio::process::Command;

const SESSION_LABEL: &str = "org.rpt.sandbox=true";
const INSPECT_TEMPLATE: &str = r#"{{index .Config.Labels "org.rpt.sandbox"}}|{{.State.Running}}"#;
const HOST_INSTALL_SCRIPT: &str = r#"set -eu
container="$1"; staged="$2"; shift 2
docker exec "$container" mkdir -p /rpt-input
docker cp "$staged/." "$container:/rpt-input/"
docker exec "$container" /bin/sh -euc "$@"
docker exec "$container" rm -rf /rpt-input
"#;
const REMOVE_SCRIPT: &str = r#"set -eu
dpkg --remove -- "$@"
dpkg --audit
"#;
const PURGE_SCRIPT: &str = r#"set -eu
dpkg --purge -- "$@"
dpkg --audit
"#;

fn name(session: &str) -> Result<String> {
    if !sandbox_execution::valid_id(session) {
        bail!("session identifier must be 1..=64 ASCII letters/digits/dashes/underscores");
    }
    Ok(format!("rpt-session-{session}"))
}
fn validate_package(package: &str) -> Result<()> {
    let (root, arch) = package.split_once(':').map_or((package,None), |(r,a)| (r,Some(a)));
    if root.len() < 2
        || !root.as_bytes()[0].is_ascii_lowercase() && !root.as_bytes()[0].is_ascii_digit()
        || !root.bytes().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit()
            || matches!(c,b'+'|b'.'|b'-')) {
        bail!("invalid binary package name: {package}");
    }
    if let Some(arch) = arch {
        if arch.is_empty() || !arch.bytes().all(|c| c.is_ascii_lowercase()
            || c.is_ascii_digit() || c == b'-') {
            bail!("invalid architecture in binary package: {package}");
        }
    }
    Ok(())
}
async fn docker(args: &[String]) -> Result<std::process::Output> {
    Command::new("docker").args(args).output().await
        .context("invoking Docker CLI")
}
fn success(result: std::process::Output, operation: &str) -> Result<String> {
    if !result.status.success() {
        bail!("{operation} failed: {}", String::from_utf8_lossy(&result.stderr));
    }
    Ok(String::from_utf8(result.stdout)?)
}
async fn verify_session(session: &str, require_running: bool) -> Result<String> {
    let container = name(session)?;
    let result = docker(&[
        "container".into(), "inspect".into(), "--format".into(),
        INSPECT_TEMPLATE.into(), container.clone(),
    ]).await?;
    let output = success(result, "inspecting RPT sandbox")?;
    let expected = if require_running { "true|true" } else { "true|false" };
    if output.trim() != "true|true" && (require_running || output.trim() != expected) {
        bail!("Docker container is not an RPT-labeled sandbox or has unexpected running state");
    }
    Ok(container)
}
pub async fn create(session: &str, image: &str) -> Result<String> {
    let container = name(session)?;
    if !sandbox_execution::valid_image(image) { bail!("invalid Docker image identifier"); }
    let args = [
        "run", "-d", "--pull=never", "--network=none",
        "--cap-drop=ALL", "--security-opt=no-new-privileges",
        "--pids-limit=128", "--memory=1g", "--cpus=2", "--ipc=private",
        "--label", SESSION_LABEL, "--name", &container,
        "--entrypoint", "/bin/sh", image, "-ec", "exec sleep infinity",
    ].iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    success(docker(&args).await?, "creating isolated session")?;
    verify_session(session, true).await
}
pub async fn inspect(session: &str) -> Result<String> {
    let container = verify_session(session, true).await?;
    success(docker(&[
        "exec".into(), container, "dpkg-query".into(), "--show".into(),
    ]).await?, "reading sandbox package state")
}
pub async fn destroy(session: &str) -> Result<()> {
    let container = verify_session(session, false).await?;
    success(docker(&[
        "rm".into(), "--force".into(), container,
    ]).await?, "destroying isolated session")?;
    Ok(())
}

/// All mutating commands are confined to an existing RPT-labeled container.
/// On failure, stop the container so the operator must inspect it before retry.
async fn run_command(
    session: &str, journal: &Path, id: &str, program: &str,
    args: Vec<String>, seconds: u64,
) -> Result<()> {
    let container = verify_session(session, true).await?;
    if !sandbox_execution::valid_id(id) { bail!("invalid transaction ID"); }
    if !(10..=3600).contains(&seconds) { bail!("timeout must be 10..=3600 seconds"); }
    let queue = DryRunExecutor::new(journal.to_path_buf(), 1)?;
    let outcome = queue.submit_command(
        id.to_owned(), program.to_owned(), args,
        Duration::from_secs(seconds), Duration::from_secs(5),
    ).await;
    if outcome.is_err() {
        let stop = docker(&[
            "stop".into(), "--time".into(), "3".into(), container,
        ]).await;
        match stop {
            Ok(output) if output.status.success() => {}
            _ => bail!("sandbox transaction failed; could not confirm container stop; manual Docker inspection required"),
        }
    }
    outcome
}
pub struct SessionInstall {
    pub session: String,
    pub id: String,
    pub journal_dir: PathBuf,
    pub manifest: PathBuf,
    pub artifacts: PathBuf,
    pub cache: PathBuf,
    pub architecture: String,
    pub timeout_seconds: u64,
}
pub async fn install(opts: SessionInstall) -> Result<()> {
    let container = verify_session(&opts.session, true).await?;
    if !sandbox_execution::valid_id(&opts.id) { bail!("invalid transaction ID"); }
    let audited = artifact_store::verify(
        &opts.manifest, &opts.artifacts, &opts.cache, &opts.architecture,
    )?;
    if audited.verified == 0 { bail!("empty signed install transaction"); }
    let before = fs::read(&opts.manifest)?;
    let exact: ExactManifest = serde_json::from_slice(&before)?;
    if exact.executable || exact.packages.len() != audited.verified
        || before != fs::read(&opts.manifest)? {
        bail!("manifest inconsistency during isolated transaction preflight");
    }
    fs::create_dir_all(&opts.journal_dir)?;
    let root = fs::canonicalize(&opts.journal_dir)?;
    if root == Path::new("/") { bail!("journal cannot be filesystem root"); }
    let stages = root.join("sandbox-session-stages");
    if !stages.exists() { fs::create_dir(&stages)?; }
    if !fs::symlink_metadata(&stages)?.file_type().is_dir() {
        bail!("sandbox-session staging directory is not real");
    }
    let stage = stages.join(&opts.id);
    fs::create_dir(&stage).context("staging exists; inspect previous transaction")?;
    struct StageGuard(PathBuf);
    impl Drop for StageGuard {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }
    let _guard = StageGuard(stage.clone());
    let filenames = sandbox_execution::stage_artifacts(&stage, &opts.artifacts, &exact)?;
    let stage = fs::canonicalize(stage)?;
    let src = stage.to_str().context("non-UTF8 sandbox stage directory")?;
    if src.contains(':') || src.contains('\n') {
        bail!("Docker cp cannot safely represent the sandbox stage path");
    }
    let inner = format!("{DPKG_CHECK_SCRIPT}\n{DPKG_INSTALL_SCRIPT}");
    let mut args = [
        "-euc", HOST_INSTALL_SCRIPT, "rpt-session-host", &container,
        src, &inner, "rpt-session-dpkg",
    ].iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    for item in filenames {
        args.push(format!("/rpt-input/{item}"));
    }
    run_command(&opts.session, &opts.journal_dir, &opts.id,
        "/bin/sh", args, opts.timeout_seconds).await
}
pub async fn remove(
    session: &str, journal: &Path, id: &str, packages: &[String],
    purge: bool, timeout_seconds: u64,
) -> Result<()> {
    let container = verify_session(session, true).await?;
    if packages.is_empty() || packages.len() > 128 {
        bail!("expected 1..=128 installed binary identities to remove");
    }
    let mut unique = std::collections::BTreeSet::new();
    for pkg in packages {
        validate_package(pkg)?;
        if !unique.insert(pkg) { bail!("duplicate removal request"); }
    }
    let script = if purge { PURGE_SCRIPT } else { REMOVE_SCRIPT };
    let mut args = ["exec", &container, "/bin/sh", "-euc", script, "rpt-session-dpkg"]
        .iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    args.extend(packages.iter().cloned());
    run_command(session, journal, id, "docker", args, timeout_seconds).await
}

/// SHA256 preflight is repeated inside the container immediately before dpkg.
/// Host cannot pass arbitrary dpkg --force flags.
const REMOVE_SNAPSHOT_SCRIPT: &str = r#"set -eu
expected="$1"; shift
actual="$(sha256sum /var/lib/dpkg/status)"
actual="${actual%% *}"
if [ "$actual" != "$expected" ]; then
    echo "Sandbox dpkg status changed since removal planning" >&2
    exit 1
fi
dpkg --remove -- "$@"
dpkg --audit
"#;
const AUTOREMOVE_SNAPSHOT_SCRIPT: &str = r#"set -eu
expected="$1"; extended="$2"; shift 2
actual="$(sha256sum /var/lib/dpkg/status)"
actual="${actual%% *}"
if [ "$actual" != "$expected" ]; then
    echo "Sandbox dpkg status changed since autoremove planning" >&2
    exit 1
fi
actual_auto="$(sha256sum /var/lib/apt/extended_states)"
actual_auto="${actual_auto%% *}"
if [ "$actual_auto" != "$extended" ]; then
    echo "Sandbox auto/manual marks changed since autoremove planning" >&2
    exit 1
fi
dpkg --remove -- "$@"
dpkg --audit
"#;

async fn guest_file(container: &str, file: &str) -> Result<Vec<u8>> {
    let result = docker(&[
        "exec".to_owned(), container.to_owned(), "cat".to_owned(), file.to_owned(),
    ]).await?;
    if !result.status.success() {
        bail!("reading sandbox state {file}: {}", String::from_utf8_lossy(&result.stderr));
    }
    if result.stdout.len() > 64 * 1024 * 1024 {
        bail!("sandbox state exceeds maximum size");
    }
    Ok(result.stdout)
}
async fn status_snapshot(session: &str) -> Result<(String, Vec<u8>)> {
    let container = verify_session(session, true).await?;
    let status = guest_file(&container, "/var/lib/dpkg/status").await?;
    Ok((container, status))
}

/// Preview removal using the container's installed database, never host dpkg.
pub async fn plan_remove(
    session: &str, architecture: &str, packages: &[String], cascade: bool,
) -> Result<RemovalPlan> {
    let (_, status) = status_snapshot(session).await?;
    removal_planner::remove_snapshot(&status, architecture, packages, cascade)
}

/// Read true in-sandbox APT auto marks (missing marks fail closed).
pub async fn plan_autoremove(session: &str, architecture: &str) -> Result<RemovalPlan> {
    let (container, status) = status_snapshot(session).await?;
    let marks = guest_file(&container, "/var/lib/apt/extended_states").await?;
    removal_planner::autoremove_snapshot(&status, &marks, architecture)
}

pub async fn remove_planned(
    session: &str, journal: &Path, id: &str, architecture: &str,
    packages: &[String], cascade: bool, timeout_seconds: u64,
) -> Result<RemovalPlan> {
    let (container, status) = status_snapshot(session).await?;
    let plan = removal_planner::remove_snapshot(&status, architecture, packages, cascade)?;
    if !plan.removable {
        bail!("removal denied by installed dependency plan: {}", plan.blockers.join("; "));
    }
    if plan.remove.is_empty() { bail!("removal planner returned no packages"); }
    if plan.remove.len() > 128 { bail!("removal closure exceeds 128 packages"); }
    let digest = hex::encode(Sha256::digest(&status));
    let mut args = [
        "exec".to_owned(), container, "/bin/sh".into(), "-euc".into(),
        REMOVE_SNAPSHOT_SCRIPT.into(), "rpt-sandbox-remove".into(), digest,
    ].to_vec();
    args.extend(plan.remove.iter().cloned());
    run_command(session, journal, id, "docker", args, timeout_seconds).await?;
    Ok(plan)
}

pub async fn autoremove_planned(
    session: &str, journal: &Path, id: &str, architecture: &str,
    timeout_seconds: u64,
) -> Result<RemovalPlan> {
    let (container, status) = status_snapshot(session).await?;
    let marks = guest_file(&container, "/var/lib/apt/extended_states").await?;
    let plan = removal_planner::autoremove_snapshot(&status, &marks, architecture)?;
    if !plan.removable {
        bail!("autoremove denied by installed dependency plan: {}", plan.blockers.join("; "));
    }
    if plan.remove.is_empty() { return Ok(plan); }
    if plan.remove.len() > 128 { bail!("autoremove closure exceeds 128 packages"); }
    let status_digest = hex::encode(Sha256::digest(&status));
    let marks_digest = hex::encode(Sha256::digest(&marks));
    let mut args = [
        "exec".to_owned(), container, "/bin/sh".into(), "-euc".into(),
        AUTOREMOVE_SNAPSHOT_SCRIPT.into(), "rpt-sandbox-autoremove".into(),
        status_digest, marks_digest,
    ].to_vec();
    args.extend(plan.remove.iter().cloned());
    run_command(session, journal, id, "docker", args, timeout_seconds).await?;
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_session_and_binary_names() {
        assert_eq!(name("demo_01").unwrap(), "rpt-session-demo_01");
        assert!(name("../host").is_err());
        assert!(validate_package("libc6:amd64").is_ok());
        assert!(validate_package("libfoo+1").is_ok());
        assert!(validate_package("--force-remove").is_err());
        assert!(validate_package("a").is_err());
        assert!(validate_package("foo:").is_err());
    }
    #[test]
    fn scripts_never_bind_host_roots() {
        assert!(HOST_INSTALL_SCRIPT.contains("docker cp"));
        assert!(HOST_INSTALL_SCRIPT.contains("docker exec"));
        assert!(!HOST_INSTALL_SCRIPT.contains("/var/lib/dpkg"));
        assert!(!HOST_INSTALL_SCRIPT.contains("--privileged"));
    }
}

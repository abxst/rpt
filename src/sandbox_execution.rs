//! Actual dpkg unpack/configure in an EPHEMERAL Docker container.
//! No host dpkg invocation, host-root bind, or persistent package mutation.
use crate::{artifact_store, dry_run_executor::DryRunExecutor,
    package_provenance::ExactManifest, version};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet, fs::{self, File, OpenOptions},
    io::{Read, Write}, path::{Path, PathBuf}, time::Duration,
};

pub struct SandboxInstall {
    pub manifest: PathBuf,
    pub artifacts: PathBuf,
    pub signed_cache: PathBuf,
    pub journal_dir: PathBuf,
    pub id: String,
    pub architecture: String,
    pub image: String,
    pub timeout_seconds: u64,
}

// File names are passed as positional arguments, never interpolated into shell.
const DPKG_INSTALL_SCRIPT: &str = r#"set -eu
dpkg --install -- "$@"
dpkg --configure --pending
dpkg --triggers-only --pending
if [ -n "$(dpkg --audit)" ]; then
    dpkg --audit >&2
    exit 1
fi
"#;

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn valid_image(image: &str) -> bool {
    !image.is_empty() && image.len() <= 255 && !image.starts_with('-')
        && image.bytes().all(|b| b.is_ascii_alphanumeric()
            || matches!(b, b'/' | b':' | b'@' | b'.' | b'-' | b'_'))
}

fn docker_args(image: &str, name: &str, stage: &Path, files: &[String]) -> Result<Vec<String>> {
    if !valid_image(image) || !name.starts_with("rpt-sbx-") {
        bail!("invalid sandbox image or name");
    }
    if files.is_empty() || files.len() > 5000 {
        bail!("sandbox install requires 1..=5000 verified artifacts");
    }
    let stage = fs::canonicalize(stage)?;
    let path = stage.to_str().context("sandbox path is not UTF-8")?;
    if path.contains(',') || path.contains('\n') {
        bail!("sandbox path cannot be represented as a Docker bind argument");
    }
    let mut args = [
        "run", "--rm", "--pull=never", "--network=none",
        "--cap-drop=ALL", "--security-opt=no-new-privileges",
        "--pids-limit=128", "--memory=1g", "--cpus=2",
        "--ipc=private",
        "--name", name, "--mount",
    ].iter().map(|x| (*x).to_owned()).collect::<Vec<_>>();
    args.push(format!("type=bind,src={path},dst=/rpt-input,readonly"));
    args.extend(["--entrypoint", "/bin/sh", image, "-euc", DPKG_INSTALL_SCRIPT, "rpt-sandbox"]
        .iter().map(|x| (*x).to_owned()));
    for file in files {
        if file.contains('/') || !file.ends_with(".deb")
            || !file.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b,b'.'|b'-'|b'_')) {
            bail!("invalid staged artifact filename");
        }
        args.push(format!("/rpt-input/{file}"));
    }
    Ok(args)
}

// The immutable-looking staging view is private and populated from fully
// rehashed copies, never a direct bind of the caller's download tree.
fn stage_artifacts(stage: &Path, artifacts: &Path, manifest: &ExactManifest) -> Result<Vec<String>> {
    let root = fs::canonicalize(artifacts)?;
    if !root.is_dir() { bail!("artifact root is not a directory"); }
    let mut staged = Vec::new();
    let mut seen = BTreeSet::new();
    for (index, entry) in manifest.packages.iter().enumerate() {
        if !seen.insert((&entry.artifact.name, &entry.architecture)) {
            bail!("duplicate planned binary in signed manifest");
        }
        version::compare(&entry.version, &entry.version)?;
        crate::validate_package(&entry.artifact)?;
        let path = root.join(&entry.artifact.filename);
        if !fs::symlink_metadata(&path)?.file_type().is_file() {
            bail!("artifact is not a regular file");
        }
        let canonical = fs::canonicalize(&path)?;
        if !canonical.starts_with(&root) { bail!("artifact escapes download root"); }
        let name = format!("{index:05}.deb");
        let mut source = File::open(&canonical)?;
        let mut output = OpenOptions::new().write(true).create_new(true).open(stage.join(&name))?;
        let mut hash = Sha256::new();
        let mut size = 0u64;
        let mut buffer = [0u8; 65_536];
        loop {
            let n = source.read(&mut buffer)?;
            if n == 0 { break; }
            size = size.checked_add(n as u64).context("artifact size overflow")?;
            if size > entry.artifact.size { bail!("downloaded artifact too large"); }
            output.write_all(&buffer[..n])?;
            hash.update(&buffer[..n]);
        }
        output.sync_all()?;
        if size != entry.artifact.size
            || hex::encode(hash.finalize()) != entry.artifact.sha256.to_ascii_lowercase() {
            bail!("downloaded artifact changed during private staging");
        }
        staged.push(name);
    }
    Ok(staged)
}

/// All maintainer scripts run inside a disposable, network-isolated container.
/// No host filesystem is mounted writable and the Docker socket is not bound.
pub async fn install(opts: SandboxInstall) -> Result<()> {
    if !cfg!(target_os = "linux") { bail!("Docker sandbox install requires Linux"); }
    if !valid_id(&opts.id) { bail!("invalid transaction ID"); }
    if !valid_image(&opts.image) { bail!("invalid Docker image"); }
    if !(10..=3600).contains(&opts.timeout_seconds) {
        bail!("sandbox timeout must be 10..=3600 seconds");
    }
    let verified = artifact_store::verify(
        &opts.manifest, &opts.artifacts, &opts.signed_cache, &opts.architecture,
    )?;
    if verified.verified == 0 { bail!("no installable artifacts in manifest"); }
    let original = fs::read(&opts.manifest)?;
    let manifest: ExactManifest = serde_json::from_slice(&original)?;
    if manifest.packages.len() != verified.verified || manifest.executable {
        bail!("manifest is inconsistent or requests host execution");
    }
    if original != fs::read(&opts.manifest)? {
        bail!("manifest changed during verification");
    }

    fs::create_dir_all(&opts.journal_dir)?;
    let journal_dir = fs::canonicalize(&opts.journal_dir)?;
    if journal_dir == Path::new("/") { bail!("journal directory cannot be filesystem root"); }
    let stages_root = journal_dir.join("sandbox-stages");
    if !stages_root.exists() { fs::create_dir(&stages_root)?; }
    if !fs::symlink_metadata(&stages_root)?.file_type().is_dir() {
        bail!("sandbox stages root is not a real directory");
    }
    let stage = stages_root.join(&opts.id);
    fs::create_dir(&stage).context("sandbox stage already exists; recovery may be required")?;
    struct StageGuard(PathBuf);
    impl Drop for StageGuard {
        fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
    }
    let _guard = StageGuard(stage.clone());
    let staged = stage_artifacts(&stage, &opts.artifacts, &manifest)?;
    let name = format!("rpt-sbx-{}", opts.id);
    let args = docker_args(&opts.image, &name, &stage, &staged)?;

    let executor = DryRunExecutor::new(journal_dir, 1)?;
    let outcome = executor.submit_command(
        opts.id, "docker".to_owned(), args,
        Duration::from_secs(opts.timeout_seconds), Duration::from_secs(5),
    ).await;

    // The Docker client can exit while its daemon-owned container remains
    // running. Always ask the daemon to force-remove it after process exit.
    let cleanup = tokio::process::Command::new("docker")
        .args(["rm", "--force", &name]).output().await;
    match cleanup {
        Ok(output) if output.status.success() => {}
        Ok(output) if String::from_utf8_lossy(&output.stderr)
            .contains("No such container") => {}
        Ok(output) => bail!("cannot verify container cleanup: {}", String::from_utf8_lossy(&output.stderr)),
        Err(error) => return Err(error).context("cannot verify Docker container cleanup"),
    }
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn docker_args_reject_override_and_never_bind_host_root() {
        assert!(!valid_image("--privileged"));
        assert!(!valid_image("debian:latest --net host"));
        assert!(!valid_id("../root"));
        let args = docker_args("debian:bookworm-slim", "rpt-sbx-test", &std::env::temp_dir(),
            &["00000.deb".into()]).unwrap();
        assert!(args.contains(&"--network=none".to_owned()));
        assert!(args.contains(&"--cap-drop=ALL".to_owned()));
        assert!(args.contains(&"--security-opt=no-new-privileges".to_owned()));
        assert!(!args.iter().any(|a| a.contains("docker.sock") || a == "--privileged"));
        assert_eq!(args.last().unwrap(), "/rpt-input/00000.deb");
    }
    #[test]
    fn cannot_run_without_artifacts() {
        assert!(docker_args("debian:bookworm-slim","rpt-sbx-test",&std::env::temp_dir(),&[]).is_err());
    }
}

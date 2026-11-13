//! Durable, explicit rollback of RPT-owned Docker sandbox rootfs only.
//! Snapshots are Docker image commits; Docker volume mounts are rejected.
//! No automatic rollback or journal gate bypass is performed.
use super::*;
use crate::transaction_journal::{JournalRecord, Phase};
use std::{fs::{File, OpenOptions}, io::Write, time::{SystemTime, UNIX_EPOCH}};

const CONTAINER_INSPECT: &str = r#"{{.Id}}|{{.State.Running}}|{{index .Config.Labels "org.rpt.sandbox"}}|{{json .Mounts}}"#;
const IMAGE_INSPECT: &str = "{{.Id}}";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub schema_version: u32,
    pub session: String,
    pub transaction_id: String,
    pub original_container_id: String,
    pub image_id: String,
    pub status_sha256: String,
    pub created_unix_ms: u128,
    pub restored_container_id: Option<String>,
    pub quarantined_container: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RecoveryReport {
    pub session: String,
    pub transaction_id: String,
    pub snapshot_image: String,
    pub captured_status_sha256: String,
    pub journal_phase: Option<Phase>,
    pub container_running: bool,
    pub original_container_matches: bool,
    pub snapshot_present: bool,
    pub already_restored: bool,
    pub can_restore: bool,
    pub journal_gate_remains_locked: bool,
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}
fn valid_image_id(value: &str) -> bool {
    value.starts_with("sha256:") && valid_digest(&value[7..])
}
fn valid_container_id(value: &str) -> bool {
    valid_digest(value)
}
fn snapshot_dir(journal: &Path, create: bool) -> Result<PathBuf> {
    let journal = fs::canonicalize(journal)
        .context("journal directory must exist before a sandbox checkpoint")?;
    if journal == Path::new("/") {
        bail!("journal directory cannot be filesystem root");
    }
    let dir = journal.join("sandbox-checkpoints");
    if create {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)] {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&dir) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e).context("creating sandbox checkpoint directory"),
        }
    }
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.file_type().is_dir() {
        bail!("sandbox checkpoint directory cannot be a symlink");
    }
    #[cfg(unix)] {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("sandbox checkpoint directory must be private");
        }
    }
    Ok(dir)
}
fn file_path(dir: &Path, session: &str, id: &str) -> Result<PathBuf> {
    if !sandbox_execution::valid_id(session) || !sandbox_execution::valid_id(id) {
        bail!("invalid session or transaction identifier");
    }
    Ok(dir.join(format!("{session}--{id}.json")))
}
fn save_new(path: &Path, checkpoint: &Checkpoint) -> Result<()> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)] {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).context("checkpoint already exists; refusing overwrite")?;
    serde_json::to_writer_pretty(&mut file, checkpoint)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    File::open(path.parent().context("checkpoint parent missing")?)?.sync_all()?;
    Ok(())
}
fn read_record(journal: &Path, session: &str, id: &str) -> Result<Checkpoint> {
    let dir = snapshot_dir(journal, false)?;
    let file = file_path(&dir, session, id)?;
    if !fs::symlink_metadata(&file)?.file_type().is_file() {
        bail!("checkpoint metadata is not a regular file");
    }
    let entry: Checkpoint = serde_json::from_slice(&fs::read(file)?)?;
    if entry.schema_version != 1 || entry.session != session || entry.transaction_id != id
        || !valid_digest(&entry.status_sha256) || !valid_container_id(&entry.original_container_id)
        || !valid_image_id(&entry.image_id) {
        bail!("invalid sandbox checkpoint metadata");
    }
    Ok(entry)
}
fn read_journal_phase(journal: &Path, id: &str) -> Result<Option<Phase>> {
    let file = journal.join(format!("{id}.json"));
    match fs::symlink_metadata(&file) {
        Ok(meta) if !meta.file_type().is_file() => bail!("transaction journal is not a regular file"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
        _ => {}
    }
    let entry: JournalRecord = serde_json::from_slice(&fs::read(file)?)?;
    if entry.id != id { bail!("transaction journal ID mismatch"); }
    Ok(Some(entry.phase))
}
struct ContainerIdentity {
    id: String,
    running: bool,
}
async fn container_identity(container: &str) -> Result<ContainerIdentity> {
    let raw = success(docker(&[
        "container".into(), "inspect".into(), "--format".into(),
        CONTAINER_INSPECT.into(), container.into(),
    ]).await?, "inspecting sandbox container identity")?;
    let parts = raw.trim().split('|').collect::<Vec<_>>();
    if parts.len() != 4 || !valid_container_id(parts[0]) || parts[2] != "true"
        || parts[3] != "[]" || !matches!(parts[1], "true" | "false") {
        bail!("container identity/mounts mismatch: recovery requires RPT label and zero mounts");
    }
    Ok(ContainerIdentity { id: parts[0].into(), running: parts[1] == "true" })
}
async fn verify_image(image: &str) -> Result<()> {
    if !valid_image_id(image) { bail!("invalid checkpoint image identifier"); }
    let actual = success(docker(&[
        "image".into(), "inspect".into(), "--format".into(),
        IMAGE_INSPECT.into(), image.into(),
    ]).await?, "inspecting Docker checkpoint image")?;
    if actual.trim() != image {
        bail!("checkpoint image no longer matches recorded immutable image ID");
    }
    Ok(())
}

/// Caller MUST hold the cross-process per-session lease. This snapshot is
/// created after artifact verification and before any sandbox dpkg mutation.
pub(super) async fn checkpoint_locked(
    session: &str, id: &str, journal: &Path, expected_sha256: &str,
) -> Result<Checkpoint> {
    if !valid_digest(expected_sha256) { bail!("invalid expected status digest"); }
    let container = verify_session(session, true).await?;
    let identity = container_identity(&container).await?;
    if !identity.running { bail!("cannot checkpoint a stopped sandbox"); }
    let actual = hex::encode(Sha256::digest(
        &guest_file(&container, "/var/lib/dpkg/status").await?,
    ));
    if actual != expected_sha256 { bail!("sandbox status changed before checkpoint"); }
    let dir = snapshot_dir(journal, true)?;
    let path = file_path(&dir, session, id)?;
    if fs::symlink_metadata(&path).is_ok() {
        bail!("checkpoint already exists for this transaction");
    }
    // --pause=true is mandatory so the committed filesystem has a coherent
    // view at a single point. Containers with mounts are always refused.
    let image = success(docker(&[
        "commit".into(), "--pause=true".into(), container.clone(),
    ]).await?, "snapshotting isolated Docker root filesystem")?;
    let image = image.trim().to_owned();
    verify_image(&image).await?;
    let after = hex::encode(Sha256::digest(
        &guest_file(&container, "/var/lib/dpkg/status").await?,
    ));
    if after != expected_sha256 { bail!("sandbox status changed while snapshotting"); }
    let entry = Checkpoint {
        schema_version: 1, session: session.into(), transaction_id: id.into(),
        original_container_id: identity.id, image_id: image,
        status_sha256: expected_sha256.into(),
        created_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        restored_container_id: None, quarantined_container: None,
    };
    save_new(&path, &entry)?;
    Ok(entry)
}

async fn inspect_locked(
    session: &str, id: &str, journal: &Path,
) -> Result<RecoveryReport> {
    let record = read_record(journal, session, id)?;
    verify_image(&record.image_id).await?;
    let phase = read_journal_phase(journal, id)?;
    let current = container_identity(&name(session)?).await?;
    let matching = current.id == record.original_container_id;
    let restorable_phase = matches!(
        phase,
        Some(Phase::Prepared | Phase::Running | Phase::Interrupted | Phase::Failed)
    );
    Ok(RecoveryReport {
        session: session.into(), transaction_id: id.into(),
        snapshot_image: record.image_id, captured_status_sha256: record.status_sha256,
        journal_phase: phase, container_running: current.running,
        original_container_matches: matching, snapshot_present: true,
        already_restored: record.restored_container_id.is_some(),
        can_restore: matching && !current.running && restorable_phase
            && record.restored_container_id.is_none(),
        journal_gate_remains_locked: matches!(phase, Some(Phase::Prepared | Phase::Running | Phase::Interrupted)),
    })
}

/// Read-only, operator-facing recovery evidence.
pub async fn inspect_checkpoint(session: &str, id: &str, journal: &Path) -> Result<RecoveryReport> {
    let _lease = session_lock::acquire(session)?;
    inspect_locked(session, id, journal).await
}

/// Explicit recovery only after the damaged Docker container is STOPPED.
/// Quarantines old rootfs under a distinct name; validates the restored
/// container's exact dpkg status SHA256 before acknowledging restoration.
/// The existing journal gate remains locked pending separate manual audit.
pub async fn restore_checkpoint(session: &str, id: &str, journal: &Path) -> Result<RecoveryReport> {
    let _lease = session_lock::acquire(session)?;
    let report = inspect_locked(session, id, journal).await?;
    if !report.can_restore {
        bail!("restore refused: sandbox must be stopped, original ID must match, and journal must be unfinished/failed");
    }
    let mut record = read_record(journal, session, id)?;
    let container = name(session)?;
    let quarantine = format!("rpt-recovery-{session}-{id}");
    // Name collision is a hard failure, preserving the original container.
    let collision = docker(&[
        "container".into(), "inspect".into(), quarantine.clone(),
    ]).await?;
    if collision.status.success() {
        bail!("quarantine container name already exists; refusing replacement");
    }
    success(docker(&[
        "rename".into(), container.clone(), quarantine.clone(),
    ]).await?, "quarantining failed sandbox root")?;
    let run = docker(&[
        "run".into(), "-d".into(), "--pull=never".into(), "--network=none".into(),
        "--cap-drop=ALL".into(), "--security-opt=no-new-privileges".into(),
        "--pids-limit=128".into(), "--memory=1g".into(), "--cpus=2".into(),
        "--ipc=private".into(), "--label".into(), SESSION_LABEL.into(),
        "--name".into(), container.clone(), "--entrypoint".into(), "/bin/sh".into(),
        record.image_id.clone(), "-ec".into(), "exec sleep infinity".into(),
    ]).await?;
    let verification = async {
        success(run, "recreating sandbox from checkpoint")?;
        let current = container_identity(&container).await?;
        if !current.running { bail!("restored container is not running"); }
        let restored_hash = hex::encode(Sha256::digest(
            &guest_file(&container, "/var/lib/dpkg/status").await?,
        ));
        if restored_hash != record.status_sha256 {
            bail!("restored dpkg status hash differs from checkpoint");
        }
        Ok::<String, anyhow::Error>(current.id)
    }.await;
    let restored_id = match verification {
        Ok(id) => id,
        Err(error) => {
            let _ = docker(&["rm".into(), "--force".into(), container.clone()]).await;
            let renamed = docker(&[
                "rename".into(), quarantine.clone(), container.clone(),
            ]).await;
            match renamed {
                Ok(output) if output.status.success() => return Err(error),
                _ => bail!("restore failed and original quarantine could not be renamed: {error:#}"),
            }
        }
    };
    record.restored_container_id = Some(restored_id);
    record.quarantined_container = Some(quarantine);
    let dir = snapshot_dir(journal, false)?;
    let target = file_path(&dir, session, id)?;
    let temp = dir.join(format!(".{session}--{id}.restore.tmp"));
    save_new(&temp, &record)?;
    fs::rename(&temp, &target).context("recording verified sandbox recovery")?;
    File::open(&dir)?.sync_all()?;
    inspect_locked(session, id, journal).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_digest_and_immutable_checkpoint_image_id() {
        assert!(valid_image_id(&format!("sha256:{}", "a".repeat(64))));
        assert!(!valid_image_id("debian:latest"));
        assert!(!valid_digest("x"));
        assert!(!valid_container_id("../../../etc/passwd"));
    }
    #[test]
    fn checkpoint_path_is_bounded_to_safe_ids() {
        let root = Path::new("/tmp");
        assert!(file_path(root, "dev01", "tx-1").unwrap().ends_with("dev01--tx-1.json"));
        assert!(file_path(root, "../host", "tx-1").is_err());
        assert!(file_path(root, "dev01", "../etc").is_err());
    }
}

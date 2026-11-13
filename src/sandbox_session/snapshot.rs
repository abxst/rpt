//! Immutable Docker image checkpoints before *sandbox-only* dpkg mutation.
//! Restore always creates a different RPT sandbox; never overwrites evidence
//! in the failed container or automatically clears the transaction journal.
use super::*;
use crate::transaction_journal::Journal;
use std::{fs::OpenOptions, io::Write};

const IMAGE_INSPECT: &str = r#"{{index .Config.Labels "org.rpt.snapshot"}}|{{index .Config.Labels "org.rpt.source_session"}}|{{.Id}}"#;
const SNAPSHOT_VERIFY_SCRIPT: &str =
    "set -eu; audit=\"$(dpkg --audit)\"; test -z \"$audit\" || { echo \"$audit\" >&2; exit 1; }; sha256sum /var/lib/dpkg/status | cut -d' ' -f1";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SnapshotReceipt {
    pub schema_version: u32,
    pub source_session: String,
    pub transaction_id: String,
    pub container_id: String,
    pub image_id: String,
    pub status_sha256: String,
    pub created_utc: String,
}

fn hash64(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit())
}

fn checked_receipt(receipt: &SnapshotReceipt) -> Result<()> {
    if receipt.schema_version != 1
        || !sandbox_execution::valid_id(&receipt.source_session)
        || !sandbox_execution::valid_id(&receipt.transaction_id)
        || !receipt.image_id.starts_with("sha256:")
        || !hash64(&receipt.image_id[7..])
        || !hash64(&receipt.status_sha256)
        || !hash64(&receipt.container_id) {
        bail!("invalid sandbox snapshot receipt");
    }
    Ok(())
}

fn receipt_path(journal: &Path, session: &str, id: &str) -> PathBuf {
    journal.join("sandbox-snapshots").join(format!("{session}--{id}.json"))
}

async fn image_identity(receipt: &SnapshotReceipt) -> Result<()> {
    let inspected = success(docker(&[
        "image".into(), "inspect".into(), "--format".into(),
        IMAGE_INSPECT.into(), receipt.image_id.clone(),
    ]).await?, "validating saved snapshot image")?;
    let expected = format!(
        "true|{}|{}", receipt.source_session, receipt.image_id
    );
    if inspected.trim() != expected {
        bail!("Docker snapshot image identity or RPT provenance label mismatch");
    }
    Ok(())
}

pub fn read_receipt(path: &Path) -> Result<SnapshotReceipt> {
    if !fs::symlink_metadata(path)?.file_type().is_file() {
        bail!("snapshot receipt must be a regular file");
    }
    let data = fs::read(path)?;
    if data.len() > 64 * 1024 { bail!("oversized snapshot receipt"); }
    let receipt: SnapshotReceipt = serde_json::from_slice(&data)?;
    checked_receipt(&receipt)?;
    Ok(receipt)
}

/// Capture under the per-session lock held by run_command. The image is
/// committed with Docker's pause semantics, so committed files are consistent
/// within the container's writable layer. Volumes/bind mounts are forbidden.
pub(super) async fn capture_locked(
    session: &str, id: &str, container: &str, journal: &Path,
) -> Result<SnapshotReceipt> {
    if !sandbox_execution::valid_id(session) || !sandbox_execution::valid_id(id)
        || container != name(session)? {
        bail!("invalid sandbox checkpoint identity");
    }
    let journal = fs::canonicalize(journal).context("resolving transaction journal")?;
    if journal == Path::new("/") { bail!("snapshot journal cannot be filesystem root"); }
    // Failed transactions are terminal; in-progress records must not be bypassed.
    drop(Journal::open(&journal)?);
    let directory = journal.join("sandbox-snapshots");
    if !directory.exists() {
        fs::create_dir(&directory).context("creating private snapshot receipt directory")?;
    }
    if !fs::symlink_metadata(&directory)?.file_type().is_dir() {
        bail!("snapshot receipt directory must be a real directory");
    }
    let path = receipt_path(&journal, session, id);
    if fs::symlink_metadata(&path).is_ok() {
        bail!("checkpoint for this transaction already exists; refusing overwrite");
    }

    let mounts = success(docker(&[
        "container".into(), "inspect".into(), "--format".into(),
        "{{json .Mounts}}".into(), container.into(),
    ]).await?, "inspecting Docker mounts")?;
    if mounts.trim() != "[]" && mounts.trim() != "null" {
        bail!("Docker checkpoint cannot preserve volumes/bind mounts; refusing");
    }
    let container_id = success(docker(&[
        "container".into(), "inspect".into(), "--format".into(),
        "{{.Id}}".into(), container.into(),
    ]).await?, "reading original Docker container identity")?.trim().to_owned();
    if !hash64(&container_id) { bail!("invalid original Docker container identity"); }
    let status = guest_file(container, "/var/lib/dpkg/status").await?;
    let status_sha256 = hex::encode(Sha256::digest(status));
    let audit = success(docker(&[
        "exec".into(), container.into(), "dpkg".into(), "--audit".into(),
    ]).await?, "auditing pre-transaction dpkg database")?;
    if !audit.trim().is_empty() {
        bail!("dpkg contains unfinished packages before checkpoint; refusing mutation");
    }

    let image_id = success(docker(&[
        "commit".into(), "--pause=true".into(),
        "--change".into(), "LABEL org.rpt.snapshot=true".into(),
        "--change".into(), format!("LABEL org.rpt.source_session={session}"),
        container.into(),
    ]).await?, "committing an immutable pre-transaction checkpoint")?
        .trim().to_owned();
    let receipt = SnapshotReceipt {
        schema_version: 1,
        source_session: session.into(),
        transaction_id: id.into(),
        container_id,
        image_id,
        status_sha256,
        created_utc: chrono::Utc::now().to_rfc3339(),
    };
    checked_receipt(&receipt)?;
    image_identity(&receipt).await?;

    // Independently boot the committed image in a no-network test container
    // and require a clean dpkg audit AND the exact source status hash.
    let image_status = success(docker(&[
        "run".into(), "--rm".into(), "--pull=never".into(),
        "--network=none".into(), "--cap-drop=ALL".into(),
        "--security-opt=no-new-privileges".into(),
        "--entrypoint".into(), "/bin/sh".into(),
        receipt.image_id.clone(), "-ec".into(),
        SNAPSHOT_VERIFY_SCRIPT.into(),
    ]).await?, "validating committed dpkg snapshot")?;
    if image_status.trim() != receipt.status_sha256 {
        bail!("Docker checkpoint does not match original dpkg state; refusing mutation");
    }

    // create_new + fsync: crashes cannot silently overwrite an older receipt.
    let mut file = OpenOptions::new().write(true).create_new(true).open(&path)?;
    file.write_all(&serde_json::to_vec_pretty(&receipt)?)?;
    file.sync_all()?;
    File::open(&directory)?.sync_all()?;
    Ok(receipt)
}

/// Read-only verification; checks the immutable Docker image ID and labels,
/// but does NOT authorize retrying an interrupted journal transaction.
pub async fn verify(path: &Path) -> Result<SnapshotReceipt> {
    let receipt = read_receipt(path)?;
    image_identity(&receipt).await?;
    Ok(receipt)
}

/// Restoration is a NEW sandbox. The forensic original remains untouched.
/// No incomplete journal is reset, bypassed or marked as recovered.
pub async fn restore(path: &Path, new_session: &str) -> Result<String> {
    let receipt = verify(path).await?;
    if new_session == receipt.source_session {
        bail!("restoring over the original forensic sandbox is forbidden");
    }
    let container = create(new_session, &receipt.image_id).await?;
    let verification = async {
        let status = guest_file(&container, "/var/lib/dpkg/status").await?;
        if hex::encode(Sha256::digest(status)) != receipt.status_sha256 {
            bail!("restored sandbox dpkg status differs from checkpoint");
        }
        let audited = success(docker(&[
            "exec".into(), container.clone(), "dpkg".into(), "--audit".into(),
        ]).await?, "auditing restored sandbox dpkg state")?;
        if !audited.trim().is_empty() {
            bail!("restored sandbox has unfinished dpkg packages");
        }
        Ok::<(), anyhow::Error>(())
    }.await;
    if let Err(error) = verification {
        // On verification failure, drop only the new sandbox, not the
        // source snapshot image, original failed session or journal.
        let _ = destroy(new_session).await;
        return Err(error).context("snapshot restoration failed verification");
    }
    Ok(container)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn receipt_validation_rejects_traversal_and_tampering() {
        let ok = SnapshotReceipt {
            schema_version: 1, source_session: "test1".into(),
            transaction_id: "tx1".into(), container_id: "a".repeat(64),
            image_id: format!("sha256:{}", "b".repeat(64)),
            status_sha256: "c".repeat(64), created_utc: "test".into(),
        };
        assert!(checked_receipt(&ok).is_ok());
        let mut bad = ok.clone();
        bad.source_session = "../../unsafe".into();
        assert!(checked_receipt(&bad).is_err());
        bad = ok.clone(); bad.image_id = "ubuntu:latest".into();
        assert!(checked_receipt(&bad).is_err());
        bad = ok; bad.status_sha256 = "000".into();
        assert!(checked_receipt(&bad).is_err());
    }
}

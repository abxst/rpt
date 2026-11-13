//! Operator recovery evidence capture. This records an attestation but NEVER
//! unlocks a transaction, rewrites journals, or asserts dpkg consistency.
use crate::recovery_inspector::inspect;
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{fs::{self, OpenOptions, File}, io::Write, path::Path, time::{SystemTime, UNIX_EPOCH}};

#[derive(Debug, Serialize, Deserialize)]
pub struct RecoveryAttestation {
    pub transaction_id: String,
    pub operator: String,
    pub reason: String,
    pub evidence: String,
    pub journal_sha256: String,
    pub recorded_unix_ms: u128,
    pub gate_remains_locked: bool,
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Persist a single audit artifact with exclusive create and fsync.
/// This is an operator assertion, not an automatic safety determination.
pub fn attest(dir: &Path, id: &str, operator: &str, reason: &str, evidence: &str) -> Result<RecoveryAttestation> {
    if !valid_id(id) { bail!("invalid transaction ID"); }
    if operator.trim().is_empty() || reason.trim().is_empty() || evidence.trim().is_empty() {
        bail!("operator, reason, and evidence are required");
    }
    let report = inspect(dir)?;
    if !report.warnings.is_empty() {
        bail!("journal integrity warnings must be investigated before attestation");
    }
    let record = report.records.iter().find(|r| r.id == id)
        .context("transaction not found")?;
    if !matches!(record.phase, crate::transaction_journal::Phase::Prepared |
        crate::transaction_journal::Phase::Running |
        crate::transaction_journal::Phase::Interrupted) {
        bail!("transaction is not awaiting recovery");
    }
    let journal_path = dir.join(format!("{id}.json"));
    if !fs::symlink_metadata(&journal_path)?.file_type().is_file() {
        bail!("journal is not a regular file");
    }
    let before = fs::read(&journal_path)?;
    let digest = hex::encode(Sha256::digest(&before));
    let audit_dir = dir.join("recovery-audit");
    fs::create_dir_all(&audit_dir)?;
    if !fs::symlink_metadata(&audit_dir)?.file_type().is_dir() {
        bail!("audit directory is not a real directory");
    }
    // One attestation per transaction; no overwrite and no automatic retry.
    let target = audit_dir.join(format!("{id}.json"));
    let entry = RecoveryAttestation {
        transaction_id: id.to_owned(),
        operator: operator.to_owned(),
        reason: reason.to_owned(),
        evidence: evidence.to_owned(),
        journal_sha256: digest,
        recorded_unix_ms: SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
        gate_remains_locked: true,
    };
    let mut file = OpenOptions::new().write(true).create_new(true).open(&target)
        .with_context(|| format!("audit already exists or cannot be created: {}", target.display()))?;
    let result = (|| -> Result<()> {
        serde_json::to_writer_pretty(&mut file, &entry)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        File::open(&audit_dir)?.sync_all()?;
        // Detect a concurrent or external edit to the journal. Never unlock.
        if fs::read(&journal_path)? != before {
            bail!("journal changed while recording attestation; manual review required");
        }
        Ok(())
    })();
    // Retain the audit file even on failure: never discard potentially relevant evidence.
    result?;
    Ok(entry)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transaction_journal::{Journal, JournalRecord, Phase};
    #[test]
    fn attestation_is_durable_non_mutating_and_does_not_unlock() {
        let dir = std::env::temp_dir().join(format!("rpt-audit-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        let journal = Journal::open(&dir).unwrap();
        journal.advance(&JournalRecord { id: "tx".into(), phase: Phase::Prepared, error: None }).unwrap();
        journal.advance(&JournalRecord { id: "tx".into(), phase: Phase::Running, error: None }).unwrap();
        journal.advance(&JournalRecord { id: "tx".into(), phase: Phase::Interrupted, error: Some("SIGTERM".into()) }).unwrap();
        drop(journal);
        let original = fs::read(dir.join("tx.json")).unwrap();
        assert!(attest(&dir, "tx", "", "checked", "dpkg --audit").is_err());
        let entry = attest(&dir, "tx", "admin", "reviewed", "dpkg --audit output saved").unwrap();
        assert!(entry.gate_remains_locked);
        assert_eq!(entry.journal_sha256, hex::encode(Sha256::digest(&original)));
        assert!(attest(&dir, "tx", "admin", "again", "again").is_err());
        assert_eq!(fs::read(dir.join("tx.json")).unwrap(), original);
        assert!(Journal::open(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn refuses_corrupt_journal_and_completed_transaction() {
        let dir = std::env::temp_dir().join(format!("rpt-audit-invalid-{}-{}", std::process::id(),
            SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("tx.json"), r#"{"id":"tx","phase":"succeeded","error":null}"#).unwrap();
        assert!(attest(&dir, "tx", "admin", "review", "evidence").is_err());
        fs::write(dir.join("broken.json"), "{").unwrap();
        assert!(attest(&dir, "tx", "admin", "review", "evidence").is_err());
        fs::remove_dir_all(dir).unwrap();
    }
}

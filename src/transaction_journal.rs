//! Durable pre-execution transaction journal and cross-process advisory lock.
//! This module does not run dpkg or claim to roll back maintainer-script effects.
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{fs::{self, File, OpenOptions}, io::Write, path::{Path, PathBuf}};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Prepared,
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct JournalRecord {
    pub id: String,
    pub phase: Phase,
    pub error: Option<String>,
}

/// One cross-process writer at a time. Keep the lock file inode stable:
/// deleting a locked file can permit a second process to lock a new inode.
pub struct Journal {
    _lock: File,
    dir: PathBuf,
}

impl Journal {
    pub fn open(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("creating journal directory {}", dir.display()))?;
        let lock = Self::open_lock(dir)?;
        lock.try_lock_exclusive().context("another RPT transaction owns the journal lock")?;
        Self::inspect_locked(dir, lock)
    }

    /// Wait for another process to finish instead of failing on contention.
    /// Advisory OS locks provide mutual exclusion, NOT strict FIFO fairness.
    /// Call from a blocking thread; use `wait_async` in Tokio applications.
    pub fn wait(dir: &Path) -> Result<Self> {
        fs::create_dir_all(dir).with_context(|| format!("creating journal directory {}", dir.display()))?;
        let lock = Self::open_lock(dir)?;
        lock.lock_exclusive().context("waiting for RPT transaction lock")?;
        Self::inspect_locked(dir, lock)
    }

    /// Wait without blocking a Tokio runtime worker.
    pub async fn wait_async(dir: PathBuf) -> Result<Self> {
        tokio::task::spawn_blocking(move || Self::wait(&dir)).await?
    }

    fn open_lock(dir: &Path) -> Result<File> {
        let path = dir.join(".rpt-transaction.lock");
        // Do not follow a planted symlink or open an existing FIFO/device.
        // The lock inode must remain stable across cooperating processes.
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if !meta.file_type().is_file() {
                bail!("journal lock is not a regular file: {}", path.display());
            }
        }
        let lock = OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(&path)?;
        if !lock.metadata()?.is_file() {
            bail!("journal lock is not a regular file: {}", path.display());
        }
        Ok(lock)
    }

    fn inspect_locked(dir: &Path, lock: File) -> Result<Self> {
        let journal = Self { _lock: lock, dir: dir.to_path_buf() };
        // An unfinished record is not safe to retry automatically: dpkg and
        // maintainer scripts may have made changes before the process died.
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "tmp") {
                bail!("incomplete journal write {}; manual inspection required", path.display());
            }
            if path.extension().is_some_and(|ext| ext == "json") {
                if !entry.file_type()?.is_file() {
                    bail!("journal record is not a regular file {}; manual inspection required", path.display());
                }
                let bytes = fs::read(&path)?;
                let record: JournalRecord = serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid journal record {}", path.display()))?;
                if path.file_stem().and_then(|name| name.to_str()) != Some(record.id.as_str()) {
                    bail!("journal filename/identifier mismatch {}; manual inspection required", path.display());
                }
                if matches!(record.phase, Phase::Prepared | Phase::Running | Phase::Interrupted) {
                    bail!("unfinished transaction {} ({:?}); manual recovery required", record.id, record.phase);
                }
            }
        }
        Ok(journal)
    }

    /// State-checked journal API for future transaction executors.
    /// The caller must hold this Journal's exclusive cross-process lock.
    /// A failed or interrupted transaction is terminal until operator recovery.
    pub fn advance(&self, record: &JournalRecord) -> Result<()> {
        if record.id.is_empty()
            || !record.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            bail!("invalid transaction identifier");
        }
        if record.phase == Phase::Failed && record.error.as_deref().is_none_or(str::is_empty) {
            bail!("failed transaction requires a diagnostic error");
        }
        if record.phase != Phase::Failed && record.error.is_some() {
            bail!("only failed transactions may contain an error");
        }
        let path = self.dir.join(format!("{}.json", record.id));
        if let Ok(meta) = fs::symlink_metadata(&path) {
            if !meta.file_type().is_file() {
                bail!("journal record is not a regular file {}", path.display());
            }
        }
        let previous = match fs::read(&path) {
            Ok(bytes) => {
                let old: JournalRecord = serde_json::from_slice(&bytes)
                    .with_context(|| format!("invalid existing journal {}", path.display()))?;
                if old.id != record.id { bail!("journal identifier mismatch"); }
                Some(old.phase)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        let valid = matches!(
            (previous, record.phase),
            (None, Phase::Prepared)
                | (Some(Phase::Prepared), Phase::Running)
                | (Some(Phase::Prepared), Phase::Failed)
                | (Some(Phase::Running), Phase::Succeeded)
                | (Some(Phase::Running), Phase::Failed)
        );
        if !valid {
            bail!("invalid transaction phase transition: {:?} -> {:?}", previous, record.phase);
        }
        self.write(record)
    }

    pub fn write(&self, record: &JournalRecord) -> Result<()> {
        if record.id.is_empty() || !record.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_') {
            bail!("invalid transaction identifier");
        }
        let target = self.dir.join(format!("{}.json", record.id));
        if let Ok(meta) = fs::symlink_metadata(&target) {
            if !meta.file_type().is_file() {
                bail!("refusing to replace non-regular journal record {}", target.display());
            }
        }
        // Never overwrite a journal entry with a malformed or unrelated ID.
        let data = serde_json::to_vec_pretty(record)?;
        let temp = self.dir.join(format!(".{}.{}.tmp", record.id, std::process::id()));
        let result = (|| -> Result<()> {
            let mut file = OpenOptions::new().write(true).create_new(true).open(&temp)?;
            file.write_all(&data)?;
            file.sync_all()?;
            fs::rename(&temp, &target)?;
            File::open(&self.dir)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() { let _ = fs::remove_file(&temp); }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT_JOURNAL_FIXTURE: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> PathBuf {
        std::env::temp_dir().join(format!("rpt-journal-test-{}-{}-{}",
            std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT_JOURNAL_FIXTURE.fetch_add(1, Ordering::Relaxed)))
    }

    #[test]
    fn locks_across_handles_and_detects_interrupted_transactions() {
        let dir = temp_dir();
        let journal = Journal::open(&dir).unwrap();
        assert!(Journal::open(&dir).is_err());
        journal.write(&JournalRecord { id: "tx1".into(), phase: Phase::Running, error: None }).unwrap();
        drop(journal);
        assert!(Journal::open(&dir).is_err());
        // An operator can inspect the journal before explicitly marking the
        // transaction resolved; opening never silently resumes mutations.
        let path = dir.join("tx1.json");
        let record: JournalRecord = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(record.phase, Phase::Running);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn completed_record_allows_reopening() {
        let dir = temp_dir();
        let journal = Journal::open(&dir).unwrap();
        journal.write(&JournalRecord { id: "tx2".into(), phase: Phase::Succeeded, error: None }).unwrap();
        drop(journal);
        assert!(Journal::open(&dir).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn waiting_writer_runs_after_first_writer_releases_lock() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = temp_dir();
        let first = Journal::open(&dir).unwrap();
        let (tx, rx) = mpsc::channel();
        let second_dir = dir.clone();
        let handle = std::thread::spawn(move || {
            let second = Journal::wait(&second_dir).unwrap();
            tx.send(()).unwrap();
            drop(second);
        });
        assert!(rx.recv_timeout(Duration::from_millis(50)).is_err());
        drop(first);
        rx.recv_timeout(Duration::from_secs(3)).unwrap();
        handle.join().unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn checked_transitions_reject_skips_restarts_and_missing_diagnostics() {
        let dir = temp_dir();
        let journal = Journal::open(&dir).unwrap();
        let mut record = JournalRecord { id: "tx-state".into(), phase: Phase::Running, error: None };
        assert!(journal.advance(&record).is_err());
        record.phase = Phase::Prepared;
        journal.advance(&record).unwrap();
        assert!(journal.advance(&record).is_err());
        record.phase = Phase::Succeeded;
        assert!(journal.advance(&record).is_err());
        record.phase = Phase::Running;
        journal.advance(&record).unwrap();
        record.phase = Phase::Failed;
        assert!(journal.advance(&record).is_err());
        record.error = Some("fixture failure".into());
        journal.advance(&record).unwrap();
        record.phase = Phase::Running;
        record.error = None;
        assert!(journal.advance(&record).is_err());
        drop(journal);
        assert!(Journal::open(&dir).is_ok());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn terminal_transaction_ids_cannot_be_reused() {
        for terminal in [Phase::Succeeded, Phase::Failed] {
            let dir = temp_dir();
            let journal = Journal::open(&dir).unwrap();
            journal.advance(&JournalRecord { id: "reused".into(), phase: Phase::Prepared, error: None }).unwrap();
            journal.advance(&JournalRecord { id: "reused".into(), phase: Phase::Running, error: None }).unwrap();
            journal.advance(&JournalRecord {
                id: "reused".into(), phase: terminal,
                error: if terminal == Phase::Failed { Some("test failure".into()) } else { None },
            }).unwrap();
            drop(journal);
            let reopened = Journal::open(&dir).unwrap();
            assert!(reopened.advance(&JournalRecord {
                id: "reused".into(), phase: Phase::Prepared, error: None,
            }).is_err());
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn incomplete_and_corrupt_journals_fail_closed() {
        for (filename, data) in [
            ("broken.json", "{not-json"),
            (".interrupted.tmp", "partial"),
            ("unfinished.json", r#"{"id":"unfinished","phase":"running","error":null}"#),
        ] {
            let dir = temp_dir();
            fs::create_dir_all(&dir).unwrap();
            fs::write(dir.join(filename), data).unwrap();
            assert!(Journal::open(&dir).is_err(), "accepted {filename}");
            fs::remove_dir_all(dir).unwrap();
        }
    }

    #[test]
    fn many_sequential_transactions_keep_durable_terminal_records() {
        let dir = temp_dir();
        for n in 0..32 {
            let journal = Journal::wait(&dir).unwrap();
            let id = format!("batch-{n}");
            journal.advance(&JournalRecord { id: id.clone(), phase: Phase::Prepared, error: None }).unwrap();
            journal.advance(&JournalRecord { id: id.clone(), phase: Phase::Running, error: None }).unwrap();
            journal.advance(&JournalRecord { id: id.clone(), phase: Phase::Succeeded, error: None }).unwrap();
        }
        let journal = Journal::open(&dir).unwrap();
        for n in 0..32 {
            let record: JournalRecord = serde_json::from_slice(
                &fs::read(dir.join(format!("batch-{n}.json"))).unwrap(),
            ).unwrap();
            assert_eq!(record.phase, Phase::Succeeded);
        }
        drop(journal);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn rejects_path_traversal_identifiers() {
        let dir = temp_dir();
        let journal = Journal::open(&dir).unwrap();
        assert!(journal.write(&JournalRecord { id: "../escape".into(), phase: Phase::Prepared, error: None }).is_err());
        drop(journal);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn rejects_mismatched_journal_filename() {
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("wrong.json"), r#"{"id":"right","phase":"succeeded","error":null}"#).unwrap();
        assert!(Journal::open(&dir).is_err());
        fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_lock_file() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let outside = temp_dir();
        fs::write(&outside, "untouched").unwrap();
        symlink(&outside, dir.join(".rpt-transaction.lock")).unwrap();
        assert!(Journal::open(&dir).is_err());
        assert!(Journal::wait(&dir).is_err());
        assert_eq!(fs::read_to_string(&outside).unwrap(), "untouched");
        fs::remove_dir_all(dir).unwrap();
        fs::remove_file(outside).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_journal_without_following_it() {
        use std::os::unix::fs::symlink;
        let dir = temp_dir();
        fs::create_dir_all(&dir).unwrap();
        let external = temp_dir();
        fs::write(&external, r#"{"id":"tx","phase":"succeeded","error":null}"#).unwrap();
        symlink(&external, dir.join("tx.json")).unwrap();
        assert!(Journal::open(&dir).is_err());
        fs::remove_file(dir.join("tx.json")).unwrap();
        let journal = Journal::open(&dir).unwrap();
        symlink(&external, dir.join("tx.json")).unwrap();
        let record = JournalRecord { id: "tx".into(), phase: Phase::Prepared, error: None };
        assert!(journal.advance(&record).is_err());
        assert!(journal.write(&record).is_err());
        assert_eq!(fs::read_to_string(&external).unwrap(), r#"{"id":"tx","phase":"succeeded","error":null}"#);
        drop(journal);
        fs::remove_dir_all(dir).unwrap();
        fs::remove_file(external).unwrap();
    }

}

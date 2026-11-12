//! Non-mutating transaction executor. This is a lifecycle test harness,
//! not a dpkg executor or a safe filesystem commit implementation.
use crate::transaction::{TransactionQueue};
use crate::transaction_journal::{Journal, JournalRecord, Phase};
use anyhow::{bail, Context, Result};
use std::path::PathBuf;

/// Serializes dry-run jobs in-process, then acquires a cross-process journal lock.
/// No package installation, mount, or filesystem commit is performed.
#[derive(Clone)]
pub struct DryRunExecutor {
    queue: TransactionQueue,
    journal_dir: PathBuf,
}

impl DryRunExecutor {
    pub fn new(journal_dir: PathBuf, capacity: usize) -> Result<Self> {
        Ok(Self { queue: TransactionQueue::new(capacity)?, journal_dir })
    }

    /// Records the lifecycle of a simulated transaction.
    /// A process crash while Running remains fail-closed for operator inspection.
    pub async fn submit(&self, id: String, fail_message: Option<String>) -> Result<()> {
        let dir = self.journal_dir.clone();
        self.queue.submit(move || async move {
            let journal = Journal::wait_async(dir).await.context("acquiring transaction journal lock")?;
            let mut record = JournalRecord { id, phase: Phase::Prepared, error: None };
            journal.advance(&record).context("preparing dry-run transaction")?;
            record.phase = Phase::Running;
            journal.advance(&record).context("starting dry-run transaction")?;
            if let Some(message) = fail_message {
                record.phase = Phase::Failed;
                record.error = Some(message.clone());
                journal.advance(&record).context("recording dry-run failure")?;
                bail!("dry-run transaction failed: {message}");
            }
            record.phase = Phase::Succeeded;
            journal.advance(&record).context("completing dry-run transaction")?;
            Ok(())
        }).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn fixture() -> PathBuf {
        std::env::temp_dir().join(format!("rpt-dry-run-{}-{}",
            std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos()))
    }

    #[tokio::test]
    async fn succeeds_and_records_durable_terminal_state() {
        let dir = fixture();
        let executor = DryRunExecutor::new(dir.clone(), 4).unwrap();
        executor.submit("dry1".into(), None).await.unwrap();
        let record: JournalRecord = serde_json::from_slice(&std::fs::read(dir.join("dry1.json")).unwrap()).unwrap();
        assert_eq!(record.phase, Phase::Succeeded);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn failure_is_durable_and_does_not_block_next_transaction() {
        let dir = fixture();
        let executor = DryRunExecutor::new(dir.clone(), 4).unwrap();
        assert!(executor.submit("dry1".into(), Some("simulated failure".into())).await.is_err());
        let record: JournalRecord = serde_json::from_slice(&std::fs::read(dir.join("dry1.json")).unwrap()).unwrap();
        assert_eq!(record.phase, Phase::Failed);
        assert_eq!(record.error.as_deref(), Some("simulated failure"));
        executor.submit("dry2".into(), None).await.unwrap();
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn unfinished_journal_blocks_new_work() {
        let dir = fixture();
        let journal = Journal::open(&dir).unwrap();
        journal.advance(&JournalRecord { id: "orphan".into(), phase: Phase::Prepared, error: None }).unwrap();
        drop(journal);
        let executor = DryRunExecutor::new(dir.clone(), 4).unwrap();
        assert!(executor.submit("blocked".into(), None).await.is_err());
        assert!(!dir.join("blocked.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[tokio::test]
    async fn two_executors_share_cross_process_lock() {
        let dir = fixture();
        let first = DryRunExecutor::new(dir.clone(), 4).unwrap();
        let second = DryRunExecutor::new(dir.clone(), 4).unwrap();
        let (a, b) = tokio::join!(
            first.submit("one".into(), None),
            second.submit("two".into(), None)
        );
        a.unwrap();
        b.unwrap();
        assert!(dir.join("one.json").exists());
        assert!(dir.join("two.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }
}

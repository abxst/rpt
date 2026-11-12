//! Non-mutating transaction executor. This is a lifecycle test harness,
//! not a dpkg executor or a safe filesystem commit implementation.
use crate::process_supervisor::{ProcessSupervisor, StopOutcome};
use crate::transaction::TransactionQueue;
use crate::transaction_journal::{Journal, JournalRecord, Phase};
use anyhow::{bail, Context, Result};
use std::{path::PathBuf, time::Duration};
use tokio::process::Command;

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

    /// Executes an unprivileged command under process-group supervision.
    /// Timeout is a failed transaction only after shutdown has been verified.
    /// An unverified shutdown deliberately leaves Running in the journal.
    pub async fn submit_command(
        &self,
        id: String,
        program: String,
        args: Vec<String>,
        timeout: Duration,
        stop_grace: Duration,
    ) -> Result<()> {
        let dir = self.journal_dir.clone();
        self.queue.submit(move || async move {
            let journal = Journal::wait_async(dir).await.context("acquiring transaction journal lock")?;
            let mut record = JournalRecord { id, phase: Phase::Prepared, error: None };
            journal.advance(&record).context("preparing supervised transaction")?;
            let mut command = Command::new(&program);
            command.args(&args);
            let mut supervisor = match ProcessSupervisor::spawn(&mut command) {
                Ok(child) => child,
                Err(error) => {
                    record.phase = Phase::Failed;
                    record.error = Some(format!("spawn failed: {error:#}"));
                    journal.advance(&record)?;
                    return Err(error);
                }
            };
            record.phase = Phase::Running;
            journal.advance(&record).context("starting supervised transaction")?;
            let status = match tokio::time::timeout(timeout, supervisor.wait()).await {
                Ok(result) => result.context("waiting for supervised command")?,
                Err(_) => {
                    let stopped = supervisor.stop(stop_grace).await
                        .context("shutdown unverified; journal left Running for manual recovery")?;
                    let detail = match stopped {
                        StopOutcome::Exited(status) | StopOutcome::Terminated(status) | StopOutcome::Killed(status) =>
                            format!("command timed out; supervised process group stopped (status: {status})"),
                    };
                    record.phase = Phase::Failed;
                    record.error = Some(detail.clone());
                    journal.advance(&record).context("recording verified timeout")?;
                    bail!("{detail}");
                }
            };
            if status.success() {
                record.phase = Phase::Succeeded;
                journal.advance(&record).context("recording successful supervised transaction")?;
                Ok(())
            } else {
                let detail = format!("supervised command exited with {status}");
                record.phase = Phase::Failed;
                record.error = Some(detail.clone());
                journal.advance(&record).context("recording supervised command failure")?;
                bail!("{detail}")
            }
        }).await
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

    #[cfg(unix)]
    #[tokio::test]
    async fn supervised_command_success_is_journaled() {
        let dir = fixture();
        let executor = DryRunExecutor::new(dir.clone(), 2).unwrap();
        executor.submit_command("command-ok".into(), "sh".into(),
            vec!["-c".into(), "exit 0".into()], Duration::from_secs(3), Duration::from_secs(1)).await.unwrap();
        let record: JournalRecord = serde_json::from_slice(&std::fs::read(dir.join("command-ok.json")).unwrap()).unwrap();
        assert_eq!(record.phase, Phase::Succeeded);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn supervised_command_timeout_records_verified_failure() {
        let dir = fixture();
        let executor = DryRunExecutor::new(dir.clone(), 2).unwrap();
        let result = executor.submit_command("command-timeout".into(), "sh".into(),
            vec!["-c".into(), "exec sleep 30".into()],
            Duration::from_millis(100), Duration::from_secs(1)).await;
        assert!(result.is_err());
        let record: JournalRecord = serde_json::from_slice(&std::fs::read(dir.join("command-timeout.json")).unwrap()).unwrap();
        assert_eq!(record.phase, Phase::Failed);
        assert!(record.error.unwrap().contains("timed out"));
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

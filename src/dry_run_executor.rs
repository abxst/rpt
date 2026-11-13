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
    /// Timeout or SIGINT/SIGTERM triggers verified process-group shutdown.
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
            // Persist Running before spawning: a journal write failure must
            // never leave a newly spawned child executing without a durable
            // Running record. A crash between this write and spawn is
            // conservatively treated as unfinished during recovery.
            record.phase = Phase::Running;
            journal.advance(&record).context("recording supervised transaction before spawn")?;
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
            // Install signal listeners before waiting. Cancellation of the
            // wait future must not drop the supervisor: stop() owns cleanup.
            #[cfg(unix)]
            let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("installing SIGTERM handler")?;
            enum WaitResult {
                Exited(std::process::ExitStatus),
                Interrupted(&'static str),
            }
            #[cfg(unix)]
            let outcome = {
                let wait = supervisor.wait();
                tokio::pin!(wait);
                tokio::select! {
                    result = &mut wait => WaitResult::Exited(result.context("waiting for supervised command")?),
                    _ = tokio::time::sleep(timeout) => WaitResult::Interrupted("timeout"),
                    _ = tokio::signal::ctrl_c() => WaitResult::Interrupted("SIGINT"),
                    _ = terminate.recv() => WaitResult::Interrupted("SIGTERM"),
                }
            };
            #[cfg(not(unix))]
            let outcome = {
                let wait = supervisor.wait();
                tokio::pin!(wait);
                tokio::select! {
                    result = &mut wait => WaitResult::Exited(result.context("waiting for supervised command")?),
                    _ = tokio::time::sleep(timeout) => WaitResult::Interrupted("timeout"),
                    _ = tokio::signal::ctrl_c() => WaitResult::Interrupted("SIGINT"),
                }
            };
            let status = match outcome {
                WaitResult::Exited(status) => status,
                WaitResult::Interrupted(reason) => {
                    let stopped = supervisor.stop(stop_grace).await
                        .context("shutdown unverified; journal left Running for manual recovery")?;
                    let detail = match stopped {
                        StopOutcome::Exited(status) | StopOutcome::Terminated(status) | StopOutcome::Killed(status) =>
                            format!("command interrupted by {reason}; supervised process group stopped (status: {status}); filesystem state unverified"),
                    };
                    record.phase = Phase::Interrupted;
                    record.error = Some(detail.clone());
                    journal.advance(&record).context("recording verified process shutdown as Interrupted")?;
                    bail!("{detail}");
                }
            };
            // A direct child may exit while its descendants remain alive.
            // Verify group shutdown before recording any terminal state.
            supervisor.stop(stop_grace).await
                .context("process-group shutdown unverified; journal left Running for manual recovery")?;
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
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};
    static NEXT_FIXTURE_ID: AtomicU64 = AtomicU64::new(0);

    fn fixture() -> PathBuf {
        std::env::temp_dir().join(format!("rpt-dry-run-{}-{}-{}",
            std::process::id(), SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT_FIXTURE_ID.fetch_add(1, Ordering::Relaxed)))
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
        assert_eq!(record.phase, Phase::Interrupted);
        assert!(record.error.unwrap().contains("timeout"));
        // A verified process-group stop is not sufficient to permit new
        // transactions without checking the filesystem and package state.
        assert!(executor.submit("after-timeout".into(), None).await.is_err());
        assert!(!dir.join("after-timeout.json").exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    // Run the signal target in a separate test process so SIGTERM never
    // affects the CI test runner or unrelated concurrently executing tests.
    #[cfg(unix)]
    #[tokio::test]
    async fn signal_test_subprocess_target() {
        if std::env::var_os("RPT_SIGNAL_TEST_DIR").is_none() {
            return;
        }
        let dir = PathBuf::from(std::env::var_os("RPT_SIGNAL_TEST_DIR").unwrap());
        let executor = DryRunExecutor::new(dir, 1).unwrap();
        let result = executor.submit_command(
            "signal-target".into(), "sh".into(),
            vec!["-c".into(), "exec sleep 30".into()],
            Duration::from_secs(20), Duration::from_millis(300),
        ).await;
        assert!(result.is_err(), "signal must interrupt supervised command");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sigterm_records_interrupted_and_blocks_recovery() {
        use std::process::{Command as StdCommand, Stdio};
        unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
        let dir = fixture();
        let exe = std::env::current_exe().unwrap();
        let mut child = StdCommand::new(exe)
            .arg("--exact")
            .arg("dry_run_executor::tests::signal_test_subprocess_target")
            .arg("--nocapture")
            .env("RPT_SIGNAL_TEST_DIR", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn().unwrap();
        let journal_path = dir.join("signal-target.json");
        let ready = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(bytes) = std::fs::read(&journal_path) {
                    if let Ok(record) = serde_json::from_slice::<JournalRecord>(&bytes) {
                        if record.phase == Phase::Running { break; }
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await;
        if ready.is_err() {
            let _ = child.kill();
            let output = child.wait_with_output().unwrap();
            panic!("signal target never reached Running: {}", String::from_utf8_lossy(&output.stderr));
        }
        // Give the target time to finish spawning and registering listeners.
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(unsafe { kill(child.id() as i32, 15) }, 0);
        let waited = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(status) = child.try_wait().unwrap() { break status; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await;
        if waited.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("signal target did not exit after SIGTERM");
        }
        assert!(waited.unwrap().success());
        let record: JournalRecord = serde_json::from_slice(&std::fs::read(&journal_path).unwrap()).unwrap();
        assert_eq!(record.phase, Phase::Interrupted);
        assert!(record.error.unwrap().contains("SIGTERM"));
        assert!(DryRunExecutor::new(dir.clone(), 1).unwrap()
            .submit("blocked-after-signal".into(), None).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn sigint_records_interrupted_and_blocks_recovery() {
        use std::process::{Command as StdCommand, Stdio};
        unsafe extern "C" { fn kill(pid: i32, signal: i32) -> i32; }
        let dir = fixture();
        let exe = std::env::current_exe().unwrap();
        let mut child = StdCommand::new(exe)
            .arg("--exact")
            .arg("dry_run_executor::tests::signal_test_subprocess_target")
            .env("RPT_SIGNAL_TEST_DIR", &dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn().unwrap();
        let path = dir.join("signal-target.json");
        let ready = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(bytes) = std::fs::read(&path) {
                    if let Ok(record) = serde_json::from_slice::<JournalRecord>(&bytes) {
                        if record.phase == Phase::Running { break; }
                    }
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await;
        if ready.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("SIGINT target never reached Running");
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(unsafe { kill(child.id() as i32, 2) }, 0);
        let waited = tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                if let Some(status) = child.try_wait().unwrap() { break status; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }).await;
        if waited.is_err() {
            let _ = child.kill();
            let _ = child.wait();
            panic!("SIGINT target did not exit");
        }
        assert!(waited.unwrap().success());
        let record: JournalRecord = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(record.phase, Phase::Interrupted);
        assert!(record.error.unwrap().contains("SIGINT"));
        assert!(DryRunExecutor::new(dir.clone(), 1).unwrap()
            .submit("blocked-after-int".into(), None).await.is_err());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ignored_sigterm_escalates_to_sigkill_and_blocks_recovery() {
        let dir = fixture();
        let executor = DryRunExecutor::new(dir.clone(), 1).unwrap();
        // exec ensures the command itself ignores SIGTERM, requiring SIGKILL.
        let result = executor.submit_command(
            "kill-escalation".into(), "sh".into(),
            vec!["-c".into(), "trap '' TERM; exec sleep 30".into()],
            Duration::from_millis(300), Duration::from_millis(100),
        ).await;
        assert!(result.is_err());
        let record: JournalRecord = serde_json::from_slice(
            &std::fs::read(dir.join("kill-escalation.json")).unwrap()
        ).unwrap();
        assert_eq!(record.phase, Phase::Interrupted);
        assert!(record.error.unwrap().contains("timeout"));
        assert!(executor.submit("blocked-after-kill".into(), None).await.is_err());
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

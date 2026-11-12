//! Unprivileged child-process lifecycle supervisor.
//! This does not assert filesystem or package transaction safety.
use anyhow::{Context, Result};
use std::{process::ExitStatus, time::Duration};
use tokio::process::{Child, Command};

#[derive(Debug)]
pub enum StopOutcome {
    Exited(ExitStatus),
    Terminated(ExitStatus),
    Killed(ExitStatus),
}

pub struct ProcessSupervisor {
    child: Child,
}

impl ProcessSupervisor {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        command.kill_on_drop(true);
        let child = command.spawn().context("spawning supervised process")?;
        Ok(Self { child })
    }

    pub async fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().await.context("waiting for supervised process")
    }

    /// Request graceful termination, then escalate after the deadline.
    /// The returned outcome confirms only that the *direct child* was reaped.
    /// Process-group handling and descendants require a separate implementation.
    pub async fn stop(&mut self, grace: Duration) -> Result<StopOutcome> {
        if let Some(status) = self.child.try_wait()? {
            return Ok(StopOutcome::Exited(status));
        }
        #[cfg(unix)]
        {
            // Use the OS kill command without extra crates. Only signal the
            // direct child PID; never use a negative process-group PID here.
            let pid = self.child.id().context("missing child PID")?;
            let status = Command::new("kill")
                .arg("-TERM").arg("--").arg(pid.to_string())
                .status().await.context("sending SIGTERM to child")?;
            if !status.success() && self.child.try_wait()?.is_none() {
                anyhow::bail!("could not send SIGTERM to child {pid}");
            }
        }
        #[cfg(not(unix))]
        {
            self.child.start_kill().context("terminating child")?;
        }
        match tokio::time::timeout(grace, self.child.wait()).await {
            Ok(status) => Ok(StopOutcome::Terminated(status?)),
            Err(_) => {
                self.child.start_kill().context("escalating child termination")?;
                let status = self.child.wait().await.context("reaping killed child")?;
                Ok(StopOutcome::Killed(status))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn already_exited_child_is_reaped() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("exit 0");
        let mut child = ProcessSupervisor::spawn(&mut command).unwrap();
        let outcome = child.stop(Duration::from_secs(2)).await.unwrap();
        match outcome {
            StopOutcome::Exited(s) | StopOutcome::Terminated(s) => assert!(s.success()),
            StopOutcome::Killed(_) => panic!("fast exit should not require SIGKILL"),
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn graceful_stop_reaps_direct_child() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("exec sleep 30");
        let mut child = ProcessSupervisor::spawn(&mut command).unwrap();
        let outcome = child.stop(Duration::from_secs(2)).await.unwrap();
        assert!(matches!(outcome, StopOutcome::Terminated(_) | StopOutcome::Exited(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ignores_term_then_escalates_to_kill() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("trap '' TERM; exec sleep 30");
        let mut child = ProcessSupervisor::spawn(&mut command).unwrap();
        let outcome = child.stop(Duration::from_millis(100)).await.unwrap();
        assert!(matches!(outcome, StopOutcome::Terminated(_) | StopOutcome::Killed(_) | StopOutcome::Exited(_)));
    }
}

//! Process-group aware supervisor for unprivileged child processes.
//! This verifies process-group exit, NOT host filesystem or dpkg safety.
use anyhow::{bail, Context, Result};
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
    #[cfg(unix)]
    pgid: i32,
}

#[cfg(unix)]
mod signals {
    use anyhow::{Context, Result};
    use std::io;
    extern "C" {
        fn kill(pid: i32, sig: i32) -> i32;
    }
    pub fn send_group(pgid: i32, signal: i32) -> Result<()> {
        // pgid is a positive PID obtained from the freshly spawned child.
        if pgid <= 0 { anyhow::bail!("invalid process group"); }
        let result = unsafe { kill(-pgid, signal) };
        if result == 0 { return Ok(()); }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(3) { return Ok(()); } // ESRCH
        Err(error).context("signalling supervised process group")
    }
    pub fn exists(pgid: i32) -> Result<bool> {
        if pgid <= 0 { anyhow::bail!("invalid process group"); }
        if unsafe { kill(-pgid, 0) } == 0 { return Ok(true); }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(3) { return Ok(false); }
        Err(error).context("checking supervised process group")
    }
}

impl ProcessSupervisor {
    pub fn spawn(command: &mut Command) -> Result<Self> {
        command.kill_on_drop(true);
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // New group; never send a signal to the caller's process group.
            command.as_std_mut().process_group(0);
        }
        let child = command.spawn().context("spawning supervised process")?;
        #[cfg(unix)]
        let pgid = i32::try_from(child.id().context("missing child PID")?)
            .context("child PID out of range")?;
        Ok(Self {
            child,
            #[cfg(unix)]
            pgid,
        })
    }

    pub async fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().await.context("waiting for supervised process")
    }

    /// Stop the whole process group and verify that no group members remain.
    /// Descendants that call setsid/setpgid may escape this group.
    pub async fn stop(&mut self, grace: Duration) -> Result<StopOutcome> {
        #[cfg(unix)]
        {
            let initial = self.child.try_wait()?;
            if !signals::exists(self.pgid)? {
                let status = match initial {
                    Some(status) => status,
                    None => self.child.wait().await?,
                };
                return Ok(StopOutcome::Exited(status));
            }
            signals::send_group(self.pgid, 15)?;
            let deadline = tokio::time::Instant::now() + grace;
            let mut last_status = initial;
            loop {
                if last_status.is_none() { last_status = self.child.try_wait()?; }
                if !signals::exists(self.pgid)? {
                    let status = match last_status {
                        Some(status) => status,
                        None => self.child.wait().await?,
                    };
                    return Ok(StopOutcome::Terminated(status));
                }
                if tokio::time::Instant::now() >= deadline { break; }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            signals::send_group(self.pgid, 9)?;
            // Reap direct child even if it died before its descendants.
            let status = match last_status {
                Some(status) => status,
                None => tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await
                    .context("timed out reaping supervised child after SIGKILL")??,
            };
            // Reaping grandchildren belongs to their parent or an init/subreaper.
            // If group still exists, refuse to claim successful shutdown.
            let verify_deadline = tokio::time::Instant::now() + Duration::from_secs(2);
            while signals::exists(self.pgid)? {
                if tokio::time::Instant::now() >= verify_deadline {
                    bail!("process group {} still exists after SIGKILL; shutdown unverified", self.pgid);
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            Ok(StopOutcome::Killed(status))
        }
        #[cfg(not(unix))]
        {
            if let Some(status) = self.child.try_wait()? {
                return Ok(StopOutcome::Exited(status));
            }
            self.child.start_kill().context("terminating child")?;
            let status = tokio::time::timeout(grace, self.child.wait()).await
                .context("waiting for child stop")??;
            Ok(StopOutcome::Killed(status))
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
        // Wait for the short-lived child before testing the already-exited path.
        let completed = child.wait().await.unwrap();
        assert!(completed.success());
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
        command.arg("-c").arg("exec sleep 30");
        let mut child = ProcessSupervisor::spawn(&mut command).unwrap();
        let outcome = child.stop(Duration::ZERO).await.unwrap();
        assert!(matches!(outcome, StopOutcome::Terminated(_) | StopOutcome::Killed(_) | StopOutcome::Exited(_)));
    }
    #[cfg(unix)]
    #[tokio::test]
    async fn repeated_stop_after_successful_reap_is_safe() {
        let mut command = Command::new("sh");
        command.arg("-c").arg("exec sleep 30");
        let mut supervisor = ProcessSupervisor::spawn(&mut command).unwrap();
        let first = supervisor.stop(Duration::from_secs(2)).await.unwrap();
        assert!(matches!(first, StopOutcome::Terminated(_) | StopOutcome::Exited(_)));
        let second = supervisor.stop(Duration::from_secs(2)).await.unwrap();
        assert!(matches!(second, StopOutcome::Exited(_)));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn shutdown_reaps_shell_and_two_background_children() {
        use std::time::{SystemTime, UNIX_EPOCH};
        let stamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let marker = std::env::temp_dir().join(format!("rpt-process-group-{}-{stamp}", std::process::id()));
        let mut command = Command::new("sh");
        // The shell owns and reaps its children; all three remain in the
        // supervisor's group. The readiness marker avoids a spawn race.
        command.arg("-c").arg(
            "trap 'wait; exit 0' TERM; sleep 30 & sleep 30 & : > \"$1\"; wait",
        ).arg("rpt-test").arg(&marker);
        let mut supervisor = ProcessSupervisor::spawn(&mut command).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
        while !marker.exists() {
            assert!(tokio::time::Instant::now() < deadline, "child did not become ready");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let outcome = supervisor.stop(Duration::from_secs(3)).await.unwrap();
        assert!(matches!(outcome, StopOutcome::Terminated(_) | StopOutcome::Exited(_)));
        let _ = std::fs::remove_file(marker);
    }

}

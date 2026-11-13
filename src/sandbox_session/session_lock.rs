//! Cooperative single-writer locking across RPT commands and journal paths.
//! Locks are per Docker session and local HOME; direct Docker access bypasses
//! them, so the sandbox is never a production isolation guarantee.
use anyhow::{bail, Context, Result};
use fs2::FileExt;
use std::{fs::{self, File, OpenOptions}, path::PathBuf};

pub(super) fn acquire(session: &str) -> Result<File> {
    if !crate::sandbox_execution::valid_id(session) {
        bail!("invalid sandbox session identifier");
    }
    let home = std::env::var_os("HOME").context("HOME required for sandbox session locking")?;
    let home = fs::canonicalize(PathBuf::from(home)).context("resolving HOME for session locking")?;
    if !home.is_dir() { bail!("HOME must resolve to a directory"); }
    let dir = home.join(".rpt-sandbox-session-locks");
    if !dir.exists() {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&dir) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err).context("creating session lock directory"),
        }
    }
    let meta = fs::symlink_metadata(&dir)?;
    if !meta.file_type().is_dir() { bail!("session lock directory is not real"); }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if meta.permissions().mode() & 0o077 != 0 {
            bail!("session lock directory must be private to current account");
        }
    }
    let path = dir.join(format!("rpt-session-{session}.lock"));
    if let Ok(meta) = fs::symlink_metadata(&path) {
        if !meta.file_type().is_file() {
            bail!("session lock file is not regular");
        }
    }
    let mut options = OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path)?;
    if !file.metadata()?.is_file() { bail!("session lock file is not regular"); }
    file.try_lock_exclusive().context("another RPT operation owns this Docker sandbox session")?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn session_lock_refuses_concurrent_owner_and_recovers_after_release() {
        let id = format!("test-lock-{}", std::process::id());
        let first = acquire(&id).unwrap();
        assert!(acquire(&id).is_err());
        drop(first);
        assert!(acquire(&id).is_ok());
        assert!(acquire("../root").is_err());
    }
}

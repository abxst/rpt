//! Non-mutating transaction CLI; never runs dpkg or mounts filesystems.
use anyhow::{bail, Result};
use clap::Parser;
use rpt::dry_run_executor::DryRunExecutor;
use std::{path::PathBuf, process::ExitCode};
use rpt::transaction_journal::{JournalRecord, Phase};

#[derive(Parser)]
#[command(about = "Simulate a journaled transaction without installing packages")]
struct Args {
    #[arg(long)]
    journal_dir: PathBuf,
    #[arg(long)]
    id: String,
    #[arg(long)]
    simulate_failure: Option<String>,
    #[arg(long, default_value_t = 16)]
    queue_capacity: usize,
    /// Emit machine-readable JSON lines to stderr.
    #[arg(long)]
    json: bool,
}

async fn execute(args: Args) -> Result<u8> {
    if args.id.is_empty()
        || !args.id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("invalid transaction ID: use ASCII letters, digits, '-' or '_'");
    }
    if args.simulate_failure.as_deref().is_some_and(str::is_empty) {
        bail!("simulated failure message cannot be empty");
    }
    let executor = DryRunExecutor::new(args.journal_dir.clone(), args.queue_capacity)?;
    emit_event(args.json, &args.id, "queued", None);
    let id = args.id.clone();
    // Never drop an active transaction future on SIGINT/SIGTERM.
    let mut job = Box::pin(executor.submit(args.id, args.simulate_failure));
    #[cfg(unix)]
    let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    #[cfg(unix)]
    let (result, signal_code) = tokio::select! {
        result = &mut job => (result, None),
        signal = tokio::signal::ctrl_c() => {
            signal?;
            emit_event(args.json, &id, "shutdown_requested", Some("SIGINT; draining transaction"));
            (job.await, Some(130u8))
        }
        signal = term.recv() => {
            if signal.is_none() { bail!("SIGTERM listener closed"); }
            emit_event(args.json, &id, "shutdown_requested", Some("SIGTERM; draining transaction"));
            (job.await, Some(143u8))
        }
    };
    #[cfg(not(unix))]
    let (result, signal_code) = tokio::select! {
        result = &mut job => (result, None),
        signal = tokio::signal::ctrl_c() => {
            signal?;
            emit_event(args.json, &id, "shutdown_requested", Some("interrupt; draining transaction"));
            (job.await, Some(130u8))
        }
    };
    if let Some(code) = signal_code {
        // This check is only sufficient for dry-run: no dpkg or children exist.
        let path = args.journal_dir.join(format!("{id}.json"));
        let bytes = std::fs::read(&path)?;
        let record: JournalRecord = serde_json::from_slice(&bytes)?;
        if record.id != id || !matches!(record.phase, Phase::Succeeded | Phase::Failed) {
            bail!("transaction not verified terminal; manual inspection required");
        }
        if let Err(error) = &result {
            emit_event(args.json, &id, "failed", Some(&format!("{error:#}")));
        }
        emit_event(args.json, &id, "stopped_verified", Some("dry-run terminal journal; no host mutations"));
        return Ok(code);
    }
    match &result {
        Ok(()) => emit_event(args.json, &id, "succeeded", None),
        Err(error) => emit_event(args.json, &id, "failed", Some(&format!("{error:#}"))),
    }
    result.map(|()| 0)
}

fn emit_event(json: bool, id: &str, event: &str, error: Option<&str>) {
    if json {
        eprintln!("{}", serde_json::json!({
            "transaction_id": id,
            "event": event,
            "mode": "dry_run",
            "error": error,
        }));
    } else if let Some(error) = error {
        eprintln!("rpt: dry-run transaction {id} {event}: {error}");
    } else {
        eprintln!("rpt: dry-run transaction {id} {event}");
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    match execute(Args::parse()).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => { eprintln!("rpt: {error:#}"); ExitCode::FAILURE },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parser_accepts_valid_arguments() {
        let args = Args::try_parse_from(["rpt-transaction-dry-run", "--journal-dir", "/tmp/rpt", "--id", "tx1"]).unwrap();
        assert_eq!(args.id, "tx1");
        assert_eq!(args.queue_capacity, 16);
    }
}

//! Non-mutating transaction CLI; never runs dpkg or mounts filesystems.
use anyhow::{bail, Result};
use clap::Parser;
use rpt::dry_run_executor::DryRunExecutor;
use std::{path::PathBuf, process::ExitCode};

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

async fn execute(args: Args) -> Result<()> {
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
    let result = executor.submit(args.id, args.simulate_failure).await;
    match &result {
        Ok(()) => emit_event(args.json, &id, "succeeded", None),
        Err(error) => emit_event(args.json, &id, "failed", Some(&format!("{error:#}"))),
    }
    result
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
        Ok(()) => ExitCode::SUCCESS,
        Err(_) => ExitCode::FAILURE,
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

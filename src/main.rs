use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rpt::{download::Downloader, error::{classify, describe, ErrorCategory}, sources::list_sources, Manifest};
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(name = "rpt", version, about = "Experimental Rust APT-compatible engine (transport MVP)")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Read APT source definitions without changing the system.
    Sources {
        #[arg(long, default_value = "/etc/apt")]
        root: PathBuf,
    },
    /// Concurrently download a verified JSON package manifest.
    Fetch {
        #[arg(long)]
        manifest: PathBuf,
        #[arg(long, default_value = "./rpt-cache")]
        output: PathBuf,
        #[arg(long, default_value_t = 8)]
        workers: usize,
        #[arg(long, default_value_t = 4)]
        segments: usize,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    if let Err(error) = execute(cli).await {
        let category = classify(&error, ErrorCategory::System);
        eprintln!("rpt: error [{category}]: {}", describe(&error));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Sources { root } => {
            for source in list_sources(&root).context(ErrorCategory::System)? {
                println!("{source}\n");
            }
        }
        Command::Fetch { manifest, output, workers, segments } => {
            let input = tokio::fs::read(&manifest).await
                .context("reading manifest")
                .context(ErrorCategory::System)?;
            let manifest: Manifest = serde_json::from_slice(&input)
                .context("parsing manifest")
                .context(ErrorCategory::System)?;
            let downloader = Downloader::new(workers, segments)
                .context(ErrorCategory::System)?;
            downloader.download_all(manifest.packages, &output).await?;
            println!("Downloads verified in {}", output.display());
        }
    }
    Ok(())
}

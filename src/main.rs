use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rpt::{download::Downloader, error::{classify, code, describe, emit, ErrorCategory, LogFormat}, sources::list_sources, Manifest};
use std::{path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(name = "rpt", version, about = "Experimental Rust APT-compatible engine (transport MVP)")]
struct Cli {
    #[arg(long, global = true, value_enum, default_value_t = LogFormat::Text)]
    log_format: LogFormat,
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
    let format = cli.log_format;
    if let Err(error) = execute(cli).await {
        let category = classify(&error, ErrorCategory::System);
        emit(format, "error", category, code(&error, ErrorCategory::System), None, &describe(&error));
        return ExitCode::FAILURE;
    }
    ExitCode::SUCCESS
}

async fn execute(cli: Cli) -> Result<()> {
    let format = cli.log_format;
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
                .context(ErrorCategory::System)?
                .with_log_format(format);
            downloader.download_all(manifest.packages, &output).await?;
            println!("Downloads verified in {}", output.display());
        }
    }
    Ok(())
}

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
    /// Compare Debian package versions (negative / zero / positive output).
    VersionCompare { left: String, right: String },
    /// Inspect a local, uncompressed Packages index (NOT signature verification).
    InspectIndex {
        #[arg(long)] path: PathBuf,
        #[arg(long)] package: Option<String>,
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
        Command::VersionCompare { left, right } => {
            let ordering = rpt::version::compare(&left, &right)
                .context(ErrorCategory::System)?;
            println!("{}", match ordering {
                std::cmp::Ordering::Less => -1,
                std::cmp::Ordering::Equal => 0,
                std::cmp::Ordering::Greater => 1,
            });
        }
        Command::InspectIndex { path, package } => {
            let text = tokio::fs::read_to_string(&path)
                .context("reading Packages index").context(ErrorCategory::System)?;
            let entries = rpt::package_index::entries(&text)
                .context("parsing Packages index").context(ErrorCategory::System)?;
            for entry in entries.into_iter().filter(|entry| package.as_ref().is_none_or(|p| *p == entry.name)) {
                println!("{} {} {} {}", entry.name, entry.version, entry.architecture, entry.filename);
            }
        }
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

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
    /// List available versions from local uncompressed APT indexes (not candidate policy).
    IndexVersions {
        #[arg(long, default_value = "/var/lib/apt/lists")]
        lists_dir: PathBuf,
        #[arg(long)] package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
    },
    /// Concurrently refresh signed repository metadata to an isolated cache.
    Update {
        #[arg(long, default_value = "/etc/apt")]
        root: PathBuf,
        #[arg(long, default_value = "./rpt-lists")]
        output: PathBuf,
        #[arg(long, default_value_t = 8)]
        workers: usize,
        /// APT-compatible quiet level: -q or -qq (for the update command).
        #[arg(short = 'q', long = "quiet", action = clap::ArgAction::Count)]
        quiet: u8,
        /// Disable interactive index progress (also disabled automatically when redirected).
        #[arg(long)]
        no_progress: bool,
        /// Supported apt-style option: Acquire::Retries=N. Others fail closed.
        #[arg(short = 'o', long = "option")]
        option: Vec<String>,
    },
    /// Query versions in the isolated RPT update cache (not apt policy).
    CacheVersions {
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
    },
    /// Read installed version from dpkg status (read-only).
    InstalledVersion {
        #[arg(long, default_value = "/var/lib/dpkg/status")]
        status: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
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
            let text = tokio::fs::read_to_string(&path).await
                .context("reading Packages index").context(ErrorCategory::System)?;
            let entries = rpt::package_index::entries(&text)
                .context("parsing Packages index").context(ErrorCategory::System)?;
            for entry in entries.into_iter().filter(|entry| package.as_ref().is_none_or(|p| *p == entry.name)) {
                println!("{} {} {} {}", entry.name, entry.version, entry.architecture, entry.filename);
            }
        }
        Command::IndexVersions { lists_dir, package, architecture } => {
            let versions = rpt::index_catalog::available_versions(&lists_dir, &package, &architecture)
                .context("scanning APT package indexes").context(ErrorCategory::System)?;
            for version in versions { println!("{version}"); }
        }
        Command::Update { root, output, workers, quiet, no_progress, option } => {
            let mut retries = 2usize;
            for setting in option {
                let (name, value) = setting.split_once('=')
                    .ok_or_else(|| anyhow::anyhow!("APT -o expects Name=Value"))?;
                match name.trim() {
                    "Acquire::Retries" => {
                        retries = value.trim().parse::<usize>()
                            .context("Acquire::Retries must be a nonnegative integer")?;
                    }
                    _ => anyhow::bail!("unsupported APT configuration option: {name}"),
                }
            }
            let settings = rpt::update::UpdateOptions {
                workers, retries, quiet,
                progress: !no_progress && quiet == 0, log_format: format,
            };
            rpt::update::update(&root, &output, settings).await?;
        }
        Command::CacheVersions { cache, package, architecture } => {
            for version in rpt::cache_query::cache_versions(&cache, &package, &architecture)
                .context(ErrorCategory::System)? { println!("{version}"); }
        }
        Command::InstalledVersion { status, package, architecture } => {
            if let Some(installed) = rpt::cache_query::installed_version(&status, &package, &architecture)
                .context(ErrorCategory::System)? { println!("{}", installed.version); }
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

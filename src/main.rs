use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use rpt::{drivers::{self, DriverKind}, download::Downloader, error::{classify, code, describe, emit, ErrorCategory, LogFormat}, sources::list_sources, Manifest};
use std::{path::PathBuf, process::ExitCode, str::FromStr};

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
    /// Discover the package driver for an OS release (discovery is not implementation).
    DriverInfo {
        /// Explicitly inspect one backend, or detect from /etc/os-release.
        #[arg(long)]
        driver: Option<String>,
        #[arg(long, default_value = "/etc/os-release")]
        os_release: PathBuf,
    },
    /// Read APT source definitions without changing the system.
    Sources {
        #[arg(long, default_value = "/etc/apt")]
        root: PathBuf,
    },
    /// Compare Debian package versions (negative / zero / positive output).
    VersionCompare {
        left: String,
        right: String,
        #[arg(long, default_value = "auto")]
        driver: String,
    },
    /// Read-only candidate selection for externally supplied, trusted priorities.
    /// Diagnostic only: does not load APT repositories or authorize installation.
    CandidatePriorityDiagnostic {
        #[arg(long)]
        installed: String,
        #[arg(long)]
        available: String,
        #[arg(long)]
        priority: i32,
    },
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
        #[arg(long, default_value = "auto")]
        driver: String,
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
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
    },
    /// Read installed version from dpkg status (read-only).
    InstalledVersion {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "/var/lib/dpkg/status")]
        status: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
    },
    /// Inspect APT preferences records (read-only; DOES NOT evaluate pin policy).
    PreferencesInspect {
        #[arg(long, default_value = "/etc/apt")]
        apt_root: PathBuf,
    },
    /// List signed, repository-scoped versions; NOT an APT candidate/pinning decision.
    VerifiedOrigins {
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
    },
    /// Reproduce the APT machine-specific 0..99 rollout draw (read-only).
    PhasedDraw {
        #[arg(long)]
        source: String,
        #[arg(long)]
        version: String,
        #[arg(long)]
        machine_id: String,
    },
    /// Inspect authenticated rollout percentages; no machine eligibility decision.
    PhasedInspect {
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
        /// Explicit APT machine ID for deterministic phased eligibility.
        #[arg(long)]
        machine_id: Option<String>,
    },
    /// Read-only restricted pin diagnostic; never selects or installs a candidate.
    PinInspect {
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long, default_value = "/etc/apt")]
        apt_root: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
        /// Explicit APT machine ID for deterministic phased eligibility.
        #[arg(long)]
        machine_id: Option<String>,
    },
    /// Restricted read-only candidate selection; not full APT policy parity.
    CandidateInspect {
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long, default_value = "/var/lib/dpkg/status")]
        status: PathBuf,
        #[arg(long, default_value = "/etc/apt")]
        apt_root: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
        /// Explicit APT machine ID for deterministic phased eligibility.
        #[arg(long)]
        machine_id: Option<String>,
    },
    /// Preview default-priority candidate from RPT cache (NOT apt-cache policy parity).
    PolicyPreview {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long, default_value = "/var/lib/dpkg/status")]
        status: PathBuf,
        #[arg(long, default_value = "/etc/apt")]
        apt_root: PathBuf,
        #[arg(long)]
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
    },
    /// Show highest published package metadata in RPT cache (not apt show parity).
    Show {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
    },
    /// Search names/descriptions in cached Packages index (substring, not regex).
    Search {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        query: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// List highest published versions, optionally filtered by name prefix.
    List {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long, default_value = "")]
        prefix: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Show the declared direct dependencies from the newest cached package.
    Depends {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
    },
    /// Find packages declaring the requested name in Depends or Pre-Depends.
    Rdepends {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        package: String,
        #[arg(long, default_value = "amd64")]
        architecture: String,
        #[arg(long, default_value_t = 30)]
        limit: usize,
    },
    /// Plan a conservative dependency installation, read-only; NEVER runs dpkg.
    PlanInstall {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "/etc/apt")]
        apt_root: PathBuf,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long, default_value = "/var/lib/dpkg/status")]
        status: PathBuf,
        #[arg(long, default_value = "amd64")]
        architecture: String,
        #[arg(long, default_value_t = 200)]
        max_packages: usize,
        package: String,
    },
    /// Joint read-only plan for multiple roots (experimental, NOT APT policy parity).
    PlanInstallBatch {
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/var/lib/dpkg/status")] status: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 200)] max_packages: usize,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Derive a GPG/SHA256-verified download manifest, without resolving dependencies.
    PrepareDownload {
        #[arg(long, default_value = "auto")]
        driver: String,
        #[arg(long, default_value = "./rpt-lists")]
        cache: PathBuf,
        #[arg(long, default_value = "amd64")]
        architecture: String,
        #[arg(long)]
        manifest: PathBuf,
        #[arg(required = true)]
        packages: Vec<String>,
    },
    /// Simulate a journaled transaction; never runs dpkg or mounts filesystems.
    TransactionDryRun {
        #[arg(long)]
        journal_dir: PathBuf,
        #[arg(long)]
        id: String,
        /// Simulate a failure and persist its diagnostic message.
        #[arg(long)]
        fail_message: Option<String>,
    },
    /// Inspect transaction journals without changing or clearing recovery state.
    RecoveryInspect {
        #[arg(long)]
        journal_dir: PathBuf,
    },
    /// Record operator evidence for a blocked transaction; NEVER unlocks the journal.
    RecoveryAttest {
        #[arg(long)] journal_dir: PathBuf,
        #[arg(long)] id: String,
        #[arg(long)] operator: String,
        #[arg(long)] reason: String,
        #[arg(long)] evidence: String,
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

/// Discover the host distro by default instead of silently choosing Debian
/// on Fedora, Alpine or Arch. Unsupported native backends remain fail-closed.
fn resolve_driver(requested: &str) -> Result<DriverKind> {
    if requested == "auto" {
        drivers::detect_file(std::path::Path::new("/etc/os-release"))
    } else {
        DriverKind::from_str(requested)
    }
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
        Command::RecoveryAttest { journal_dir, id, operator, reason, evidence } => {
            let record = rpt::recovery_audit::attest(&journal_dir, &id, &operator, &reason, &evidence)?;
            println!("{}", serde_json::to_string_pretty(&record)?);
        }
        Command::RecoveryInspect { journal_dir } => {
            let report = rpt::recovery_inspector::inspect(&journal_dir)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::CandidatePriorityDiagnostic { installed, available, priority } => {
            let candidate = rpt::policy::choose_priority_candidate(
                &[(available, priority)], Some(&installed),
            )?;
            println!("Candidate: {}", candidate.as_deref().unwrap_or("(none)"));
        }
        Command::TransactionDryRun { journal_dir, id, fail_message } => {
            let executor = rpt::dry_run_executor::DryRunExecutor::new(journal_dir.clone(), 8)?;
            if format == LogFormat::Text {
                eprintln!("rpt: dry-run transaction {id}: waiting for journal lock");
            }
            let result = executor.submit(id.clone(), fail_message).await;
            if let Err(error) = result {
                eprintln!("rpt: transaction {id} failed; inspect journal: rpt-journal-inspect {} --json",
                    journal_dir.display());
                return Err(error);
            }
            match format {
                LogFormat::Text => println!("rpt: dry-run transaction {id} succeeded; no packages installed"),
                LogFormat::Json => println!("{}", serde_json::json!({
                    "event": "transaction_complete", "id": id, "phase": "succeeded",
                    "dry_run": true, "journal_dir": journal_dir
                })),
            }
        }
        Command::DriverInfo { driver, os_release } => {
            let kind = match driver {
                Some(value) => DriverKind::from_str(&value)?,
                None => drivers::detect_file(&os_release)?,
            };
            let backend = drivers::driver(kind);
            println!("Driver: {}", kind.as_str());
            println!("Package extension: .{}", backend.artifact_extension());
            println!("Installed database: {}", backend.installed_database());
            println!("Version comparison: {}", if kind == DriverKind::Deb { "implemented" } else { "not implemented" });
            println!("Read-only package query: {}", if kind == DriverKind::Deb { "partial" } else { "not implemented" });
            println!("Package transactions: NOT IMPLEMENTED");
        }
        Command::VerifiedOrigins { cache, package, architecture } => {
            for entry in rpt::package_provenance::verified_version_origins(&cache, &package, &architecture)? {
                println!("{} {} {} {} {} {} {} {} {} {}",
                    entry.version, entry.architecture, entry.repository_uri, entry.suite,
                    entry.index_path, entry.component,
                    entry.release.origin.as_deref().unwrap_or("-"),
                    entry.release.label.as_deref().unwrap_or("-"),
                    entry.release.archive.as_deref().unwrap_or("-"),
                    entry.release.codename.as_deref().unwrap_or("-"));
            }
        }
        Command::PhasedDraw { source, version, machine_id } => {
            println!("{}", rpt::apt_phasing::rollout_draw(&source, &version, &machine_id)?);
        }
        Command::PhasedInspect { cache, package, architecture, machine_id } => {
            for item in rpt::package_provenance::verified_version_origins(&cache, &package, &architecture)? {
                let rollout = item.phased_update_percentage
                    .map_or("unphased".to_owned(), |value| value.to_string());
                let eligibility = match (item.phased_update_percentage, machine_id.as_deref()) {
                    (Some(percent), Some(machine)) => {
                        if rpt::apt_phasing::eligible(&item.source_package, &item.source_version, machine, percent)? {
                            "included"
                        } else { "held-back" }
                    }
                    (Some(percent), None) if percent < 100 => "unknown",
                    _ => "included",
                };
                println!("{} {} {} {} {} {} {}",
                    item.version, item.repository_uri, item.suite, rollout,
                    item.source_package, item.source_version, eligibility);
            }
        }
        Command::CandidateInspect { cache, status, apt_root, package, architecture, machine_id } => {
            let default_release = rpt::policy::read_default_release(&apt_root)?;
            let records = rpt::apt_preferences::load(&apt_root)?;
            let origins = rpt::package_provenance::verified_version_origins(&cache, &package, &architecture)?;
            let installed = rpt::cache_query::installed_version(&status, &package, &architecture)?
                .map(|entry| entry.version);
            let result = rpt::apt_pin_eval::preview_candidate_with_phasing(
                &package, &origins, &records, installed.as_deref(),
                default_release.as_deref(), machine_id.as_deref(),
            )?;
            println!("Installed: {}", result.installed.as_deref().unwrap_or("(none)"));
            println!("Candidate: {}", result.candidate.as_deref().unwrap_or("(none)"));
            for (version, priority) in result.priorities {
                println!("Version: {} Priority: {}", version, priority);
            }
        }
        Command::PinInspect { cache, apt_root, package, architecture, machine_id } => {
            // This independent diagnostic never changes existing policy-preview behaviour.
            let default_release = rpt::policy::read_default_release(&apt_root)?;
            let records = rpt::apt_preferences::load(&apt_root)?;
            let origins = rpt::package_provenance::verified_version_origins(&cache, &package, &architecture)?;
            for item in rpt::apt_pin_eval::evaluate_with_phasing(
                &package, &origins, &records, default_release.as_deref(), machine_id.as_deref(),
            )? {
                println!("{} {} {} {}", item.version, item.priority, item.repository_uri, item.suite);
            }
        }
        Command::PreferencesInspect { apt_root } => {
            for record in rpt::apt_preferences::load(&apt_root)? {
                println!("Package: {}\nPin: {}\nPin-Priority: {}\n", record.package, record.pin, record.priority);
            }
        }
        Command::VersionCompare { left, right, driver } => {
            let kind = resolve_driver(&driver)?;
            let ordering = drivers::driver(kind).compare_versions(&left, &right)
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
        Command::Update { driver, root, output, workers, quiet, no_progress, option } => {
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
            let kind = resolve_driver(&driver)?;
            drivers::driver(kind).update_indexes(&root, &output, settings).await?;
        }
        Command::CacheVersions { driver, cache, package, architecture } => {
            let kind = resolve_driver(&driver)?;
            for version in drivers::driver(kind).cache_versions(&cache, &package, &architecture)
                .context(ErrorCategory::System)? { println!("{version}"); }
        }
        Command::InstalledVersion { driver, status, package, architecture } => {
            let kind = resolve_driver(&driver)?;
            let installed = drivers::driver(kind)
                .installed_packages(&status, &architecture).context(ErrorCategory::System)?;
            if let Some(item) = installed.get(&package) { println!("{}", item.version); }
        }
        Command::PolicyPreview { driver, cache, status, apt_root, package, architecture } => {
            let kind = resolve_driver(&driver)?;
            let summary = drivers::driver(kind).policy_preview(&cache, &status, &apt_root, &package, &architecture)
                .context(ErrorCategory::System)?;
            println!("{package}:");
            println!("  Installed: {}", summary.installed.as_deref().unwrap_or("(none)"));
            println!("  Candidate (default-priority preview): {}", summary.candidate.as_deref().unwrap_or("(none)"));
            println!("  Available versions:");
            for v in summary.versions { println!("    {v}"); }
            eprintln!("rpt: note: policy-preview does not apply APT pinning, release origin or all configuration");
        }
        Command::Show { driver, cache, package, architecture } => {
            let kind = resolve_driver(&driver)?;
            let item = drivers::driver(kind).show_package(&cache, &architecture, &package)
                .context(ErrorCategory::System)?
                .ok_or_else(|| anyhow::anyhow!("package not found in RPT cache: {package}"))?;
            println!("Package: {}", item.name);
            println!("Version: {}", item.version);
            println!("Architecture: {}", item.architecture);
            println!("Multi-Arch: {}", item.multi_arch);
            if !item.section.is_empty() { println!("Section: {}", item.section); }
            if !item.depends.is_empty() { println!("Depends: {}", item.depends); }
            if !item.description.is_empty() {
                println!("Description: {}", item.description.replace('\n', "\n "));
            }
        }
        Command::Search { driver, cache, query, architecture, limit } => {
            let kind = resolve_driver(&driver)?;
            for item in drivers::driver(kind).search_packages(&cache, &architecture, &query, limit)
                .context(ErrorCategory::System)? {
                println!("{} - {}", item.name,
                    item.description.lines().next().unwrap_or(""));
            }
        }
        Command::List { driver, cache, prefix, architecture, limit } => {
            let kind = resolve_driver(&driver)?;
            for item in drivers::driver(kind).list_packages(&cache, &architecture, &prefix, limit)
                .context(ErrorCategory::System)? {
                println!("{}/cache {} {}", item.name, item.version, item.architecture);
            }
        }
        Command::Depends { driver, cache, package, architecture } => {
            let kind = resolve_driver(&driver)?;
            let item = drivers::driver(kind).show_package(&cache, &architecture, &package)
                .context(ErrorCategory::System)?
                .ok_or_else(|| anyhow::anyhow!("package not found in RPT cache: {package}"))?;
            println!("{} {}", item.name, item.version);
            for (label, value) in [
                ("Pre-Depends", &item.pre_depends), ("Depends", &item.depends),
                ("Recommends", &item.recommends), ("Suggests", &item.suggests),
                ("Provides", &item.provides), ("Conflicts", &item.conflicts),
                ("Breaks", &item.breaks),
            ] {
                if !value.is_empty() { println!("{label}: {value}"); }
            }
        }
        Command::Rdepends { driver, cache, package, architecture, limit } => {
            let kind = resolve_driver(&driver)?;
            for item in drivers::driver(kind).reverse_dependencies(&cache, &architecture, &package, limit)
                .context(ErrorCategory::System)? {
                println!("{} {}", item.name, item.version);
            }
        }
        Command::PlanInstallBatch { cache, status, architecture, max_packages, packages } => {
            let steps = rpt::install_plan::preview_many(
                &cache, &status, &packages, &architecture, max_packages
            ).context(ErrorCategory::System)?;
            println!("RPT JOINT READ-ONLY PLAN (NOT APT POLICY PARITY)");
            for step in &steps {
                println!("  {}:{} {} (installed: {})", step.package, step.architecture,
                    step.version, step.installed.as_deref().unwrap_or("none"));
            }
            println!("Plan preview: {} package(s); NO CHANGES MADE", steps.len());
        }
        Command::PlanInstall { driver, cache, status, apt_root, architecture, max_packages, package } => {
            let kind = resolve_driver(&driver)?;
            let steps = drivers::driver(kind)
                .plan_install_preview(&cache, &status, &apt_root, &package, &architecture, max_packages)
                .context(ErrorCategory::System)?;
            println!("RPT READ-ONLY PLAN (NOT APT RESOLVER): {package}");
            for step in &steps {
                println!("  {} {} (installed: {})", step.package, step.version,
                    step.installed.as_deref().unwrap_or("none"));
            }
            println!("Plan preview: {} package(s); NO CHANGES MADE", steps.len());
        }
        Command::Sources { root } => {
            for source in list_sources(&root).context(ErrorCategory::System)? {
                println!("{source}\n");
            }
        }
        Command::PrepareDownload { driver, cache, architecture, manifest, packages } => {
            let kind = resolve_driver(&driver)?;
            let ready = drivers::driver(kind)
                .prepare_download(&cache, &packages, &architecture)
                .context(ErrorCategory::System)?;
            // Only trusted package filenames and content hashes from authenticated
            // Release -> Packages are exported; no package is installed.
            let data = serde_json::to_vec_pretty(&ready)?;
            tokio::fs::write(&manifest, data).await
                .context("writing verified download manifest")?;
            println!("Manifest ready: {} package(s); no system changes", ready.packages.len());
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

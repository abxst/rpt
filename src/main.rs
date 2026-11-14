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
        /// Opt-in acquisition of signed foreign Packages indexes; read-only.
        #[arg(long = "additional-architecture")]
        additional_architectures: Vec<String>,
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
    /// Read-only, exact foreign-architecture candidate-policy diagnostic.
    PlanInstallForeignPolicy {
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/var/lib/dpkg/status")] status: PathBuf,
        #[arg(long, default_value = "/")] apt_root: PathBuf,
        #[arg(long, default_value = "amd64")] native_architecture: String,
        #[arg(long)] foreign_architecture: String,
        #[arg(long, default_value_t = 200)] max_packages: usize,
        package: String,
    },
    /// Joint candidate-policy-verified plan for multiple roots; never executes dpkg.
    PlanInstallBatchPolicy {
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/var/lib/dpkg/status")] status: PathBuf,
        #[arg(long, default_value = "/")] apt_root: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 200)] max_packages: usize,
        #[arg(long, default_value_t = false)]
        include_recommends: bool,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// JSON audit of a policy-verified multi-root plan; never executes packages.
    PlanTransactionAudit {
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/var/lib/dpkg/status")] status: PathBuf,
        #[arg(long, default_value = "/")] apt_root: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 200)] max_packages: usize,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Read-only conservative reverse-dependency and protected-package removal audit.
    RemovalImpact {
        #[arg(long, default_value = "/var/lib/dpkg/status")] status: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Plan explicit removals against installed dependency graph; no dpkg execution.
    PlanRemove {
        #[arg(long, default_value = "/var/lib/dpkg/status")] status: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = false)] cascade: bool,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Plan orphan auto-package removals from APT extended_states; read-only.
    PlanAutoremove {
        #[arg(long, default_value = "/var/lib/dpkg/status")] status: PathBuf,
        #[arg(long, default_value = "/var/lib/apt/extended_states")] extended_states: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
    },
    /// Write a signed, exact-version manifest for a policy-verified dependency closure.
    PrepareDownloadPlan {
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/var/lib/dpkg/status")] status: PathBuf,
        #[arg(long, default_value = "/")] apt_root: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 200)] max_packages: usize,
        #[arg(long)] output: PathBuf,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Audit downloaded artifacts against the current signed exact-version manifest.
    VerifyDownloadPlan {
        #[arg(long)] manifest: PathBuf,
        #[arg(long, default_value = "./rpt-artifacts")] artifacts: PathBuf,
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
    },
    /// Check provenance and immutable image identity of a sandbox checkpoint.
    SandboxSnapshotVerify {
        #[arg(long)] receipt: PathBuf,
    },
    /// Restore a checkpoint into a NEW isolated Docker session, preserving evidence.
    SandboxSnapshotRestore {
        #[arg(long)] receipt: PathBuf,
        #[arg(long)] new_session: String,
    },
    /// Create a persistent, fully isolated Docker dpkg test root (no host mounts).
    SandboxSessionCreate {
        #[arg(long)] session: String,
        #[arg(long, default_value = "debian:bookworm-slim")] image: String,
    },
    /// Show installed packages inside an RPT-labeled isolated Docker root.
    SandboxSessionInspect {
        #[arg(long)] session: String,
    },
    /// Explicitly delete a persistent isolated Docker test root.
    SandboxSessionDestroy {
        #[arg(long)] session: String,
    },
    /// Apply signed exact-version archives into a persistent isolated Docker root.
    SandboxSessionInstall {
        #[arg(long)] session: String,
        #[arg(long)] id: String,
        #[arg(long)] manifest: PathBuf,
        #[arg(long, default_value = "./rpt-artifacts")] artifacts: PathBuf,
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "./rpt-journal")] journal_dir: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 600)] timeout_seconds: u64,
    },
    /// Remove or purge packages only inside an existing RPT Docker sandbox.
    SandboxSessionRemove {
        #[arg(long)] session: String,
        #[arg(long)] id: String,
        #[arg(long, default_value = "./rpt-journal")] journal_dir: PathBuf,
        #[arg(long, default_value_t = 600)] timeout_seconds: u64,
        #[arg(long, default_value_t = false)] purge: bool,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Preview dependency-checked removal against live sandbox dpkg status.
    SandboxSessionPlanRemove {
        #[arg(long)] session: String,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = false)] cascade: bool,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Preview orphan candidates using live sandbox dpkg/apt auto-manual state.
    SandboxSessionPlanAutoremove {
        #[arg(long)] session: String,
        #[arg(long, default_value = "amd64")] architecture: String,
    },
    /// Execute a freshly checked removal plan only inside the Docker sandbox.
    SandboxSessionRemovePlanned {
        #[arg(long)] session: String,
        #[arg(long)] id: String,
        #[arg(long, default_value = "./rpt-journal")] journal_dir: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = false)] cascade: bool,
        #[arg(long, default_value_t = 600)] timeout_seconds: u64,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Execute freshly checked orphan cleanup only in the Docker sandbox.
    SandboxSessionAutoremovePlanned {
        #[arg(long)] session: String,
        #[arg(long)] id: String,
        #[arg(long, default_value = "./rpt-journal")] journal_dir: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 600)] timeout_seconds: u64,
    },
    /// Preview APT candidate-policy install plan against this sandbox's dpkg state.
    SandboxSessionPlanInstall {
        #[arg(long)] session: String,
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/")] apt_root: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 200)] max_packages: usize,
        #[arg(long, default_value_t = false)]
        include_recommends: bool,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Produce exact authenticated .deb download manifest using sandbox dpkg state.
    SandboxSessionPrepareDownloadPlan {
        #[arg(long)] session: String,
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/")] apt_root: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 200)] max_packages: usize,
        #[arg(long, default_value_t = false)]
        include_recommends: bool,
        #[arg(long)] output: PathBuf,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Create a signed download intent bound to this sandbox dpkg status snapshot.
    SandboxSessionPrepareTransaction {
        #[arg(long)] session: String,
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/")] apt_root: PathBuf,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value_t = 200)] max_packages: usize,
        #[arg(long, default_value_t = false)]
        include_recommends: bool,
        #[arg(long)] output: PathBuf,
        #[arg(required = true)] packages: Vec<String>,
    },
    /// Apply a previously bound, authenticated install/upgrade only inside Docker.
    SandboxSessionApplyTransaction {
        #[arg(long)] session: String,
        #[arg(long)] id: String,
        #[arg(long)] transaction: PathBuf,
        #[arg(long, default_value = "./rpt-artifacts")] artifacts: PathBuf,
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "/")] apt_root: PathBuf,
        #[arg(long, default_value = "./rpt-journal")] journal_dir: PathBuf,
        #[arg(long, default_value_t = 600)] timeout_seconds: u64,
    },
    /// Execute signed plan artifacts only inside an ephemeral, network-isolated Docker container.
    SandboxInstall {
        #[arg(long)] manifest: PathBuf,
        #[arg(long, default_value = "./rpt-artifacts")] artifacts: PathBuf,
        #[arg(long, default_value = "./rpt-lists")] cache: PathBuf,
        #[arg(long, default_value = "./rpt-journal")] journal_dir: PathBuf,
        #[arg(long)] id: String,
        #[arg(long, default_value = "amd64")] architecture: String,
        #[arg(long, default_value = "debian:bookworm-slim")] image: String,
        #[arg(long, default_value_t = 600)] timeout_seconds: u64,
        #[arg(long, value_enum, default_value_t = rpt::sandbox_execution::SandboxLifecycle::Install)]
        lifecycle: rpt::sandbox_execution::SandboxLifecycle,
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
        Command::Update { driver, root, output, workers, quiet, no_progress, additional_architectures, option } => {
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
            if additional_architectures.is_empty() {
                drivers::driver(kind).update_indexes(&root, &output, settings).await?;
            } else {
                if kind != drivers::DriverKind::Deb {
                    anyhow::bail!("additional architecture metadata requires the Debian driver");
                }
                rpt::update::update_with_architectures(
                    &root, &output, settings, &additional_architectures,
                ).await?;
            }
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
        Command::PlanRemove { status, architecture, cascade, packages } => {
            let plan = rpt::removal_planner::remove(&status, &architecture, &packages, cascade)
                .context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::PlanAutoremove { status, extended_states, architecture } => {
            let plan = rpt::removal_planner::autoremove(&status, &extended_states, &architecture)
                .context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::RemovalImpact { status, architecture, packages } => {
            let report = rpt::removal_impact::inspect(&status, &architecture, &packages)
                .context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::PlanTransactionAudit { cache, status, apt_root, architecture, max_packages, packages } => {
            let steps = rpt::install_plan::preview_many_with_policy(
                &cache, &status, &apt_root, &packages, &architecture, max_packages
            ).context(ErrorCategory::System)?;
            let report = rpt::transaction_plan_audit::classify(&steps)
                .context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        Command::PlanInstallForeignPolicy { cache, status, apt_root, native_architecture, foreign_architecture, max_packages, package } => {
            let steps = rpt::install_plan::preview_explicit_architecture_with_policy(
                &cache, &status, &apt_root, &package, &native_architecture,
                &foreign_architecture, max_packages,
            ).context(ErrorCategory::System)?;
            println!("RPT POLICY-VERIFIED FOREIGN-ARCHITECTURE READ-ONLY PLAN");
            for step in &steps {
                println!("  {}:{} {} (installed: {})", step.package, step.architecture,
                    step.version, step.installed.as_deref().unwrap_or("none"));
            }
            println!("Foreign plan preview: {} package(s); NO CHANGES MADE", steps.len());
        }
        Command::PlanInstallBatchPolicy { cache, status, apt_root, architecture, max_packages, include_recommends, packages } => {
            let steps = rpt::install_plan::preview_many_with_policy_options(
                &cache, &status, &apt_root, &packages, &architecture, max_packages,
                include_recommends,
            ).context(ErrorCategory::System)?;
            println!("RPT POLICY-VERIFIED JOINT READ-ONLY PLAN (NOT FULL APT PARITY)");
            for step in &steps {
                println!("  {}:{} {} (installed: {})", step.package, step.architecture,
                    step.version, step.installed.as_deref().unwrap_or("none"));
            }
            println!("Plan preview: {} package(s); NO CHANGES MADE", steps.len());
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
        Command::SandboxSnapshotVerify { receipt } => {
            let snapshot = rpt::sandbox_session::verify_snapshot(&receipt).await
                .context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&snapshot)?);
        }
        Command::SandboxSnapshotRestore { receipt, new_session } => {
            let container = rpt::sandbox_session::restore_snapshot(&receipt, &new_session).await
                .context(ErrorCategory::System)?;
            println!("Recovered sandbox in {container}; original evidence and journal preserved.");
        }
        Command::SandboxSessionCreate { session, image } => {
            let container = rpt::sandbox_session::create(&session, &image).await
                .context(ErrorCategory::System)?;
            println!("Created RPT isolated Docker session: {container}");
        }
        Command::SandboxSessionInspect { session } => {
            let status = rpt::sandbox_session::inspect(&session).await
                .context(ErrorCategory::System)?;
            print!("{status}");
        }
        Command::SandboxSessionDestroy { session } => {
            rpt::sandbox_session::destroy(&session).await
                .context(ErrorCategory::System)?;
            println!("Destroyed isolated RPT Docker session: {session}");
        }
        Command::SandboxSessionInstall { session, id, manifest, artifacts, cache, journal_dir, architecture, timeout_seconds } => {
            rpt::sandbox_session::install(rpt::sandbox_session::SessionInstall {
                session, id, journal_dir, manifest, artifacts, cache, architecture,
                timeout_seconds,
            }).await.context(ErrorCategory::System)?;
            println!("Signed archives applied inside persistent Docker sandbox; host dpkg unchanged.");
        }
        Command::SandboxSessionRemove { session, id, journal_dir, timeout_seconds, purge, packages } => {
            rpt::sandbox_session::remove(
                &session, &journal_dir, &id, &packages, purge, timeout_seconds,
            ).await.context(ErrorCategory::System)?;
            println!("Sandbox dpkg removal transaction completed; host dpkg unchanged.");
        }
        Command::SandboxSessionPlanRemove { session, architecture, cascade, packages } => {
            let plan = rpt::sandbox_session::plan_remove(&session, &architecture, &packages, cascade)
                .await.context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::SandboxSessionPlanAutoremove { session, architecture } => {
            let plan = rpt::sandbox_session::plan_autoremove(&session, &architecture)
                .await.context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::SandboxSessionRemovePlanned { session, id, journal_dir, architecture, cascade, timeout_seconds, packages } => {
            let plan = rpt::sandbox_session::remove_planned(&session, &journal_dir, &id,
                &architecture, &packages, cascade, timeout_seconds)
                .await.context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::SandboxSessionAutoremovePlanned { session, id, journal_dir, architecture, timeout_seconds } => {
            let plan = rpt::sandbox_session::autoremove_planned(&session, &journal_dir, &id,
                &architecture, timeout_seconds)
                .await.context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        Command::SandboxSessionPlanInstall { session, cache, apt_root, architecture, max_packages, include_recommends, packages } => {
            let steps = rpt::sandbox_session::plan_install_with_options(
                &session, &cache, &apt_root, &architecture, max_packages,
                &packages, include_recommends,
            ).await.context(ErrorCategory::System)?;
            let audit = rpt::transaction_plan_audit::classify(&steps)
                .context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&audit)?);
        }
        Command::SandboxSessionPrepareDownloadPlan { session, cache, apt_root, architecture, max_packages, include_recommends, output, packages } => {
            let ready = rpt::sandbox_session::prepare_download_plan_with_options(
                &session, &cache, &apt_root, &architecture, max_packages,
                &packages, include_recommends,
            ).await.context(ErrorCategory::System)?;
            let data = serde_json::to_vec_pretty(&ready)?;
            tokio::fs::write(&output, data).await
                .context("writing session-specific exact download manifest")?;
            println!("Sandbox session exact manifest: {} artifacts; no dpkg changes", ready.packages.len());
        }
        Command::SandboxSessionPrepareTransaction { session, cache, apt_root, architecture, max_packages, include_recommends, output, packages } => {
            let intent = rpt::sandbox_session::prepare_bound_with_options(
                &session, &cache, &apt_root, &architecture, max_packages,
                &packages, include_recommends,
            ).await.context(ErrorCategory::System)?;
            let count = intent.manifest.packages.len();
            tokio::fs::write(&output, serde_json::to_vec_pretty(&intent)?).await
                .context("writing status-bound sandbox transaction intent")?;
            println!("Bound sandbox transaction: {count} authenticated artifacts; no dpkg changes");
        }
        Command::SandboxSessionApplyTransaction { session, id, transaction, artifacts, cache, apt_root, journal_dir, timeout_seconds } => {
            rpt::sandbox_session::apply_bound(rpt::sandbox_session::BoundApply {
                session: &session, transaction: &transaction, artifacts: &artifacts,
                cache: &cache, apt_root: &apt_root, journal: &journal_dir,
                id: &id, timeout_seconds,
            }).await.context(ErrorCategory::System)?;
            println!("Bound sandbox transaction verified, applied and audited; host dpkg unchanged.");
        }
        Command::SandboxInstall { manifest, artifacts, cache, journal_dir, id, architecture, image, timeout_seconds, lifecycle } => {
            rpt::sandbox_execution::install(rpt::sandbox_execution::SandboxInstall {
                manifest, artifacts, signed_cache: cache, journal_dir, id, architecture,
                image, timeout_seconds, lifecycle,
            }).await.context(ErrorCategory::System)?;
            println!("Ephemeral container transaction finished; host dpkg unchanged.");
        }
        Command::VerifyDownloadPlan { manifest, artifacts, cache, architecture } => {
            let verified = rpt::artifact_store::verify(&manifest, &artifacts, &cache, &architecture)
                .context(ErrorCategory::System)?;
            println!("{}", serde_json::to_string_pretty(&verified)?);
        }
        Command::PrepareDownloadPlan { cache, status, apt_root, architecture, max_packages, output, packages } => {
            let steps = rpt::install_plan::preview_many_with_policy(
                &cache, &status, &apt_root, &packages, &architecture, max_packages
            ).context(ErrorCategory::System)?;
            let locked = rpt::package_provenance::prepare_exact(&cache, &steps, &architecture)
                .context(ErrorCategory::System)?;
            let count = locked.packages.len();
            let data = serde_json::to_vec_pretty(&locked)?;
            tokio::fs::write(&output, data).await
                .context("writing exact-version signed download manifest")?;
            println!("Locked download manifest: {count} package(s); no dpkg changes");
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

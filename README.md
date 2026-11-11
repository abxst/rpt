# rpt — Rust Package Transport (experimental)

A clean-room Rust foundation for an APT-compatible package engine. The planned
architecture is an asynchronous download/preparation layer feeding a **single-writer
system transaction queue**. Configuration and package policy aim to remain compatible
with Debian/Ubuntu APT; the engine itself does not depend on libapt-pkg.

## Current implementation (v0.1.0)

- Read-only inventory of traditional `.list` and Deb822 `.sources` APT source files.
- Multi-package concurrent HTTP(S) downloads, optional byte-range segmentation.
- Docker-pull-style animated package progress on interactive terminals, aggregate transfer stats (requests, range segments, retries, cache hits, throughput); plain summary for redirected logs.
- Bounded worker counts, expected-size and SHA-256 verification before publishing files.
- Rust unit tests, CI and automated **pre-release** workflow for Linux amd64.
- In-process bounded FIFO single-worker transaction queue primitive with serialization tests (not connected to dpkg).

**NOT IMPLEMENTED:** signed repository metadata verification, complete APT configuration
and policy compatibility, dependency solver, durable dpkg transaction executor, installation,
upgrade or removal. The sample manifest is illustrative, not installable.
Do not use this release to replace system APT.

## Build

```bash
cargo build --release
cargo test
cargo run -- sources
cargo run -- fetch --manifest your-manifest.json --output ./rpt-cache --workers 8 --segments 4
```

Manifest schema: `packages[]` contains `name`, `url`, `sha256`, `size`, `filename`.
Only trusted manifests should be supplied. Download URLs are not yet bound to signed
APT repository metadata. Output paths must be relative to the chosen cache directory.

## Architecture roadmap

1. Parse apt.conf, Deb822 and sources.list with strict compatible behavior.
2. Implement signed InRelease/Release verification and trusted Packages indexes.
3. Implement Debian version comparison, dependency solver and APT pinning policy.
4. Build persistent FIFO transaction journal and single-writer dpkg executor, with
   OS-level locks, state revalidation and crash recovery.
5. Differential testing against APT in disposable Ubuntu/Debian containers.
6. Benchmarks against APT/apt-fast for download and end-to-end install time.

The release workflow is intentionally prerelease-only until compatibility and
system-mutation testing are complete.

### Error logging

RPT reports failures to stderr as `rpt: error [download]` (network/HTTP/integrity) or `rpt: error [system]` (manifest/configuration, filesystem I/O, task execution). Each failed package gets its own stderr diagnostic; the final failure count and transfer statistics stay on stdout. Exit status is nonzero when any package fails.

Capture logs separately:

```sh
rpt fetch --manifest manifest.json --output ./rpt-cache 1>rpt.stdout.log 2>rpt.stderr.log
```

The system category currently covers pre-install processing only; this transport MVP does not execute dpkg transactions.

### Structured diagnostics and APT migration safety

Use `rpt --log-format json fetch ...` for line-delimited JSON diagnostics on **stderr**.
Each event has `event`, `category`, `code`, `package` (nullable), and `message`.
Text mode includes stable codes such as `DOWNLOAD_HTTP`, `DOWNLOAD_TIMEOUT`,
`DOWNLOAD_NETWORK`, `DOWNLOAD_CHECKSUM`, `DOWNLOAD_INTEGRITY`,
`SYSTEM_IO`, `SYSTEM_CONFIG`, and `SYSTEM_WORKER`.
Statistics remain human-readable stdout; progress is hidden when stderr is not a TTY.

**APT 1:1 compatibility status: NOT ACHIEVED.** RPT still supports only `sources` and
`fetch`; none of `apt update`, `install`, `remove`, `upgrade`, dpkg transaction
behavior, or APT option semantics is implemented. A parity gate in
`tests/apt_parity_gate.rs` verifies unsupported APT commands and options fail
instead of pretending success. The tests do **not** validate APT parity.

`scripts/stage-apt.sh` is a **staging-only** tool for Ubuntu amd64. For instance:

```sh
sudo env RPT_TAG=v0.1.0-alpha.9 bash scripts/stage-apt.sh stage
sudo bash scripts/stage-apt.sh status
sudo bash scripts/stage-apt.sh restore
```

It downloads/verifies the release into `/opt/rpt/rpt`, saves an unchanged
`/opt/rpt/aptbak` copy of system `/usr/bin/apt`, and deliberately **never**
changes `/usr/bin/apt`. `activate` always refuses until a real compatible
APT engine and comprehensive safe integration tests exist. A symlink from
`/usr/bin/apt` to the current RPT would break package management.

### Independent Debian metadata core (alpha.10)

`rpt version-compare 1.0~rc1 1.0` prints `-1` (strict Debian version
ordering including epoch, upstream, revision, tilde, and numeric runs).
CI runs a differential matrix of 625 pairs against read-only
`dpkg --compare-versions`.

`rpt inspect-index --path ./Packages --package bash` parses local,
**uncompressed and not yet authenticated** Debian Packages control stanzas,
including multiline fields. It does not choose a package candidate,
resolve dependencies, verify a Release signature, or install anything.

**Important:** This milestone is not APT compatibility. The original APT
executable must stay in place. Stage only with `scripts/stage-apt.sh`.

### Aggregated index versions and Ubuntu CI oracle (alpha.11)

`rpt index-versions --lists-dir /var/lib/apt/lists --package bash --architecture amd64`
lists unique Debian versions from **uncompressed** binary-amd64 Packages indexes,
newest first. It performs streaming reads rather than parsing every package into RAM.
It is a read-only metadata exploration tool, **not** APT candidate selection: it
has no signature verification, installed state, repository pinning, resolver or
package mutation support.

CI runs `tests/ci_apt_metadata.sh` after refreshing real Ubuntu indexes and
compares version sets of five packages with `apt-cache madison`. Compressed
local indexes are copied/decompressed into a disposable directory for RPT.
The script never changes system package transactions. RPT is still not
compatible with `apt` commands/options and system APT must not be replaced.

### Parallel signed repository update (alpha.12)

`rpt update --root /etc/apt --output ./rpt-lists --workers 8` fetches signed
InRelease files and repository indexes concurrently. The initial supported
source subset is Deb822/legacy binary sources with an explicit absolute
`Signed-By` keyring. All other forms **fail closed**. `gpgv` must exist.
Downloaded index files are matched against SHA256 and byte sizes in a
signature-verified InRelease; unsuccessful updates do not replace the
previous isolated cache directory. This command does **not** update
`/var/lib/apt/lists`, and the cache is not consumed by APT or yet by RPT's
package selection engine. APT is not replaced or called to perform the update.

The current implementation supports Packages.xz, Packages.gz, and uncompressed
Packages on the wire, but stores them without decompression. Repository
metadata date/Valid-Until policy, By-Hash acquisition, apt.conf source options,
all APT transports and key management still require further compatibility work.

### Queued metadata scheduler and index progress (alpha.13)

`rpt update --root /etc/apt --output ./rpt-lists --workers 8 -o Acquire::Retries=2`
runs an in-process, bounded FIFO index-job queue. Each signed repository
InRelease is fetched and checked before its index jobs are added. Multiple
worker tasks consume queued index jobs with a global semaphore capping
simultaneous network transfers across repositories. Each index reports
bytes, speed, ETA, queued/downloading/retrying/verified/failed on an
interactive terminal; `--no-progress` suppresses the display.
The scheduler uses bounded memory for queued jobs, but a full repository's
job descriptors are still held while dispatching.

APT-style **update-only** options implemented: `-q`/`--quiet`, `-qq`,
`-o Acquire::Retries=N`/`--option Acquire::Retries=N` (0–10).
Unimplemented APT options fail closed and must not be treated as supported.
`--log-format json` emits JSON errors/retries on stderr, not JSON summaries.
The summary distinguishes authenticated repositories and indexes;
`index attempts` excludes the preliminary InRelease requests.

Partial repository failures return nonzero and leave the affected repository's
prior cache unchanged. Other repositories may have committed successfully;
the update is not an all-repository atomic transaction. RPT still does NOT
overwrite /var/lib/apt/lists or provide full APT update compatibility.

### Release freshness, By-Hash and isolated cache locking (alpha.14)

Authenticated `InRelease` metadata is checked for `Date` (required; reject if
more than 10 minutes in the future) and `Valid-Until` (reject if expired
or earlier than Date). Metadata without optional `Valid-Until` is currently
allowed, as with APT's standard archive treatment. RPT uses the
content-addressed `by-hash/SHA256/<digest>` URL when `Acquire-By-Hash: yes`
is advertised. It does not silently fall back to mutable filenames when
By-Hash acquisition fails.

`rpt update` acquires an advisory exclusive file lock on
`<output>/.rpt-update.lock` before modifying its isolated cache; a second
process targeting the same output returns nonzero. The lock file is retained
intentionally to avoid inode replacement races. This protects RPT's own
cache only, **not APT's system lock or /var/lib/apt/lists**.

Existing limitations remain: compressed indexes are retained as received;
repository Release-info change confirmation, By-Hash force modes, APT's
full configuration and transports, complete APT pinning and dpkg integration
are not implemented. Do not replace system APT.

### Streaming cache and installed-state queries (alpha.15)

`rpt cache-versions --cache ./rpt-lists --package bash --architecture amd64`
opens RPT's **isolated cache previously populated by a verified `rpt update`**,
streams `index-*.xz`, `index-*.gz` or `index-*.raw`, and deduplicates and
sorts package versions using Debian version ordering. The scanner does not
load complete index contents into memory. This is a read-only query and does
not re-verify cache signatures/hashes against its source (the local cache is
assumed untampered).

`rpt installed-version --status /var/lib/dpkg/status --package bash`
reads dpkg's installed state without invoking or modifying dpkg, and excludes
packages only in residual-config state. CI compares both commands against
`apt-cache madison bash` and `dpkg-query` on Ubuntu 24.04.

These commands do NOT implement `apt-cache policy`, dependency resolution,
candidate selection, multi-arch policy or repository pin priorities.

### Default-priority policy preview (alpha.16)

`rpt policy-preview --cache ./rpt-lists --package bash` reads the isolated
RPT index cache and `/var/lib/dpkg/status`, prints installed and available
versions, and computes a **baseline** candidate for default APT priorities
(repository 500, installed 100), respecting the no-downgrade rule. RPT
refuses to compute a candidate when nonempty APT preference files or an
explicit target-release configuration are detected.

This **is not apt-cache policy parity**: cache may include only a subset of
system repositories, no origin/priority labels, phasing, package holds,
multi-architecture resolution or complete apt.conf semantics are applied.
The preview never installs or changes packages. Do not use its candidate
as the basis for actual package transactions.

Ubuntu CI compares an uncomplicated `bash` candidate with
`apt-cache policy bash`, and unit tests reject pinning and target-release
configurations.

### Read-only show/search/list from isolated cache (alpha.17)

After an authenticated `rpt update --output ./rpt-lists`, the following commands
stream Packages metadata from cached xz, gzip or raw indexes:

- `rpt show --cache ./rpt-lists bash`: highest **published** version of bash in this cache; its fields include description, section and dependencies.
- `rpt search --cache ./rpt-lists --limit 30 bash`: case-insensitive substring search on package names and descriptions, not apt's regular expression search.
- `rpt list --cache ./rpt-lists --prefix python3 --limit 30`: alphabetically sorted package names and latest published versions.

The read-only commands preserve bounded result sets (1–1000) and process index
stanzas one at a time. Results are **not** APT Candidate selections and not
fully equivalent to `apt show/search/list`; package origin, installed state,
pinning, holds and multiarch policy are not shown. Repository cache is not
reverified at query time and must be protected from local tampering.

### Conservative dependency plan preview (alpha.18)

`rpt plan-install --cache ./rpt-lists --package app` is **not** supported;
the correct syntax is `rpt plan-install --cache ./rpt-lists app`. It
constructs an in-memory metadata catalog from RPT's isolated update cache,
reads dpkg installed state once, and creates a read-only tentative dependency
order. No install, remove, upgrade, dpkg execution or disk mutation occurs.

The preview only handles direct named Depends/Pre-Depends and simple Debian
version constraints (<<, <=, =, >=, >>). It refuses unresolved alternatives,
dependency cycles, virtual packages, architecture qualifiers, build profiles,
Conflicts/Breaks, downgrades, and APT preference/target-release configurations.
Recommends/Suggests are shown as metadata but are not solved. This is **not**
a full APT-compatible solver, does not provide package acquisition URLs from
signed provenance, and MUST NOT be used to execute actual package
transactions. The `install`, `upgrade`, `remove` and `purge` commands
continue to fail closed.

Use `rpt plan-install --cache ./rpt-lists --status /var/lib/dpkg/status
--apt-root /etc/apt --max-packages 200 PACKAGE` for a limited preview.

### Signed repository to .deb acquisition (alpha.19)

After a fresh RPT update with an explicit Signed-By keyring:

    rpt update --root /etc/apt --output ./rpt-lists --workers 8
    rpt prepare-download --cache ./rpt-lists --manifest ./packages.json bash
    rpt fetch --manifest ./packages.json --output ./rpt-debs --workers 8 --segments 4

The prepare-download command does NOT select the true APT Candidate or resolve
dependencies. It selects the highest published version of each explicitly
requested name; it never invokes dpkg. It re-verifies the cache's InRelease
signature (gpgv), signed Release freshness and checksum entries, hashes each
cached Packages index again, and extracts SHA256 and Size from the selected
package stanza. Unauthenticated, missing, expired, modified or pre-alpha.19
cache indexes are rejected; run rpt update to rebuild them.

The manifest is compatible with the existing concurrent RPT fetch command,
which verifies .deb checksums before publication in the download destination.
CI includes a live Ubuntu bash .deb acquisition and cache-tampering refusal.

This remains a read-only/download-only experimental subsystem. It does not
provide full APT pinning, alternatives, virtual Provides, multiarch, phased
updates, system dpkg locks, transaction journaling, recovery or install/remove/
upgrade compatibility. Do not replace /usr/bin/apt.

### Modular native package drivers

The distro/package engine now has a `PackageDriver` interface under
`src/drivers/` and a driver registry. The `deb` driver currently routes
existing Debian versioning, dpkg installed-state parsing, signed metadata
update/query, policy preview and read-only planning. `rpm`, `apk`, and
`pacman` drivers are **placeholders that fail closed** — do not mistake
detection for support. Query the intended family with:

    rpt driver-info
    rpt driver-info --driver rpm
    rpt version-compare --driver deb '1.0~rc1' '1.0'

Most package-related commands accept `--driver auto|deb|rpm|apk|pacman` (default: `auto` from `/etc/os-release`) and refuse
non-Debian drivers until implemented. Read
[Package driver architecture](docs/package-drivers.md) for the backend API,
supported operations and future native-package conformance gates. Full
APT behavioral parity remains the **first release prerequisite**, before
native support for RPM, apk-tools or pacman is advertised.

### Read-only APT preferences inspection (post-alpha.20 development)

`rpt preferences-inspect --apt-root /etc/apt` parses simple `Package`, `Pin`, and `Pin-Priority` records in `preferences` and `preferences.d` without changing system state. It intentionally rejects unknown, duplicate or malformed fields rather than silently applying incorrect policy. The parsed data is **not** used for candidate selection or installation. Full APT pinning parity, regular expressions, complex selectors, and repository-specific version priority remain unimplemented. `policy-preview` still refuses active preference files.

### Signed repository-scoped version inventory (experimental)

`rpt verified-origins --cache ./rpt-lists --package bash --architecture amd64` prints one line per matching signed repository/index occurrence: version, architecture, repository URI, suite and index path. It revalidates the `InRelease` signature/freshness and each cached index SHA-256/size before reading metadata. Multiple repositories advertising an identical package version remain separate entries. The output now appends component and four fields parsed from the signed `InRelease` body: `Origin`, `Label`, `Suite` (archive) and `Codename`. Missing fields print `-`. These are authenticated inputs to a future APT pinning evaluator, **not** applied pin priorities. This is intentionally **not** `apt-cache policy`: no pin priority, Release Origin/Label fields, candidate selection or transaction permission is inferred. It requires a fresh signed RPT cache and a valid Signed-By keyring.

### Restricted read-only pin diagnostics (experimental)

`rpt pin-inspect --cache ./rpt-lists --apt-root /etc/apt --package bash` reports the *per-signed-repository-occurrence* priority for a **limited** subset: exact package name, exact `Pin: version X`, or exact `Pin: release a=SUITE`, with supported priorities -1000..1000. The first matching explicit rule applies; unmatched repository entries show default priority 500. The evaluator **rejects** wildcard/general pins, origin matching, complex release selectors, target-release and config indirection, and any unsupported rule even if it targets another package. It does not choose candidates, interpret installed-state priority, handle phased updates or perform package transactions; it is not `apt-cache policy` parity. `policy-preview` remains fail-closed for active preferences.

### Read-only candidate inspection (restricted policy)

`rpt candidate-inspect --cache ./rpt-lists --apt-root /etc/apt --status /var/lib/dpkg/status --package bash` displays effective per-version priorities and a **diagnostic candidate**. The evaluator combines repository occurrence priorities by maximum, includes installed priority 100, selects highest priority then newest Debian version, and prohibits downgrades unless the target priority exceeds 1000. It supports the same strict exact selectors as `pin-inspect`. Complex APT configuration, general pins, wildcard selectors and target-release overrides fail closed. This is **not** full `apt-cache policy` parity and does **not** authorize installing packages.

The restricted diagnostic policy evaluator additionally supports exact signed Release selectors `a=` (archive), `n=` (codename), `o=` (Release Origin), `l=` (Release Label), and `c=` (component), including comma-separated AND conditions. No wildcards, duplicate keys, unknown keys, hostname `Pin: origin`, or package selector globs are accepted. This extends read-only diagnostics only; it does not enable system transactions or claim full APT parity.

The read-only policy evaluator also accepts **general `Package: *` Release pins** with exact signed selectors. Explicit package rules have precedence over general rules, and the first matching record in each class applies. Other wildcard package patterns and general version pins remain unsupported; CI compares this subset with APT. No system transaction is authorized.

The read-only `candidate-inspect` and `pin-inspect` commands now accept a narrowly scoped `APT::Default-Release "SUITE";` declaration in `apt.conf` or `apt.conf.d` and assign signed matching Release archives/codenames priority **990**. Specific package pins override target release; general pins do not. Only one exact declaration is accepted; unknown active APT config statements, indirect includes, globs and environment overrides fail closed. This does not change `policy-preview` or enable transactions.

### Signed phased rollout metadata (fail-closed)

`rpt phased-inspect --cache ./rpt-lists --package bash` prints version, repository, suite, and the authenticated `Phased-Update-Percentage` (`unphased` if absent). The field is parsed from a Packages index only after Release signature and index checksum verification; invalid values outside 0–100 and duplicate fields fail closed. Until RPT implements APT's machine-specific source package/version and machine-ID rollout algorithm, **`candidate-inspect` and `pin-inspect` reject any occurrence with percentage below 100**, instead of incorrectly treating it as an unphased update. A value of 100 is treated as complete rollout. This is a safety and provenance milestone, **not phased update eligibility parity**, and it does not authorize package transactions.

### Experimental deterministic machine-specific rollout

For offline diagnostics, `rpt phased-draw --source bash --version 5.2.21-2ubuntu4 --machine-id 00000000000000000000000000000000` reproduces APT's 0–99 rollout draw using the C++ `seed_seq` / `minstd_rand` / `uniform_int_distribution` algorithm in pure Rust, validated against independent Linux C++ test vectors. The signed Packages `Source:` field (including optional source version) is used, falling back to the binary package identity when absent.

Pass an **explicit** `--machine-id` to `rpt phased-inspect`, `rpt pin-inspect`, or `rpt candidate-inspect` to compute eligibility for a phased rollout below 100%; held-back versions receive priority ceiling **1**. Without an explicit ID, incomplete rollouts continue to fail closed. `phased-inspect` displays source package/version and `included`, `held-back`, or `unknown`. This is a read-only, restricted diagnostic: it does **not** yet parse `APT::Machine-ID`, APT's always/never-include switches, or the chroot/SOURCE_DATE_EPOCH exceptions, and it does not authorize package transactions. Repository source identity disagreements for the same binary version fail closed.

### Experimental dry-run dependency solver: unique alternatives and virtual Provides

`rpt plan` remains **read-only** and cannot install, remove, or upgrade packages. The conservative planner can now resolve `A | B` only when precisely one concrete package is available among the alternatives (or an already installed direct alternative satisfies the constraint). It can also map an unambiguous virtual `Provides` capability to a real package, including `Provides: virtual-name (= version)`. An unversioned virtual capability does not satisfy a versioned dependency.

Multiple available alternatives, multiple virtual providers, unsupported `Provides` syntax, architecture qualifiers, dependency cycles, `Conflicts`/`Breaks`, downgrade requirements, and policy-dependent choices still **fail closed**. The planner uses the cache's latest-version metadata, not the fully authenticated per-origin candidate/pin/phasing policy; consequently its output is **not authorization for package changes** and must not be used as a transaction plan.

### Bounded fail-closed alternative backtracking

The read-only `plan-install` dependency preview now retries other **already enumerated, concrete** alternatives after a branch fails, restoring all prior plan state before the next attempt. It checks the remaining dependency groups in the current package before accepting a branch, caps the search at 4,096 attempted branches, and rejects groups with more than one viable resolution instead of silently choosing an APT preference order. Multiple virtual providers still require a full APT resolver; cycles, policy-dependent choices, removals, Multi-Arch, conflicts and breaks are not solved. This diagnostic is **not safe to execute as a transaction** and never runs dpkg.

### Read-only Conflicts and Breaks guards

The experimental `plan-install` dependency preview now checks simple Debian `Conflicts` and `Breaks` relations (including `<<`, `<=`, `=`, `>=`, `>>`) against packages in its plan and packages already installed. It reads installed reverse relations from `/var/lib/dpkg/status`, rather than assuming that the newest repository metadata describes the installed version. Both directions are checked when the relevant metadata is available, and conflicting branches can be rejected during bounded backtracking. Unsupported relations and any plan requiring removals remain unsupported. This is still a **read-only preview**, not a complete resolver or a safe executable transaction; installed state handling, alternatives, virtual providers, and package policy require further validation.

### Verified policy gate for dependency install plans

`plan-install` now checks the resulting **read-only** plan against the APT candidate evaluator for **every package staged in the plan**. The gate reads supported `APT::Default-Release` and `preferences` settings, verifies GPG-signed Release/index provenance per package, and applies pin priorities and phased-rollout checks. If the candidate differs from the solver's latest-version metadata (for example due to a pin or a phased holdback), RPT **rejects** the plan. This prevents a mismatched version from being paired with the wrong `Depends`, `Provides`, or `Conflicts` fields. Unsupported policy forms or incomplete phasing without an explicit machine-ID policy also fail closed. Unsigned cache fixtures no longer pass a `plan-install` smoke test.

This is a **guard**, not a full candidate-aware resolver: it does not yet load dependency metadata for older pinned candidates, guarantee per-origin metadata consistency, or evaluate all installed no-op packages as policy candidates. `plan-install` remains an informational preview only and never calls dpkg or authorizes changes.

### Candidate-version-aware dependency closure (read-only)

The Debian `plan-install` diagnostic now chooses the repository-verified policy candidate for each discovered concrete package and loads its **exact version** of `Depends`, `Pre-Depends`, `Provides`, `Conflicts` and `Breaks`, rather than assuming the highest published version is selected. Metadata disagreement across repository indexes for the same package/version, an installed-only candidate lacking signed source metadata, and phased updates without an explicit machine ID fail closed. The dependency closure is recomputed for at most 16 rounds; cycles, unsupported alternatives, Multi-Arch and removals are still rejected. This plan remains **informational only**, not a transactional plan or full apt resolver.

### Native ARM64 CI and restricted architecture qualifiers

RPT now selects the native Packages index during `rpt update`: `binary-amd64` on x86_64 and `binary-arm64` on aarch64. The dependency planner recognizes `pkg:native` and `pkg:<selected-architecture>` (for `amd64` or `arm64` when the qualifier exactly matches the selected architecture). Qualifiers `:any`, foreign architecture requests and build-profile/architecture restriction expressions remain **fail-closed**. **This is not full Debian Multi-Arch co-installation support.** On native ARM64 use `--architecture arm64` for read-only package queries and `plan-install`; CLI defaults still target amd64 until the command defaults are generalized.

CI includes a separate `ubuntu-24.04-arm` GitHub-hosted runner (aarch64, **no QEMU**) to run Rust checks, signed Ubuntu Ports index updates, amd64/arm64 architecture guards, and compare ARM64 package versions against native `apt-cache`/`dpkg`. All installation operations remain disabled.

### Read-only dpkg hold, Essential and Protected safety guards

Dependency planning treats both `install ok installed` and `hold ok installed` as installed package states. A planned version change of an installed package marked `hold`, `Essential: yes`, or `Protected: yes` is **rejected**; unchanged packages remain allowed. The solver checks these guards during bounded alternative backtracking and requires installed version/safety metadata to agree, with unknown safety flags failing closed. No force-override is exposed. These protections apply to the experimental read-only dependency planner; **no dpkg operation is executed**. They do not yet implement all APT hold semantics, recovery or transactional safeguards.

### Multi-Arch metadata and narrow native `:any` support

RPT now records and validates the Debian `Multi-Arch` Packages field, with the default `no` and explicit `same`, `foreign`, and `allowed`. Invalid values cause metadata parsing to fail. Read-only `rpt show` reports the parsed value.

In the experimental **native-only** dependency plan, `foo:any` can match a *concrete, native-architecture* `foo` candidate only when its selected metadata explicitly declares `Multi-Arch: allowed`. An unversioned or versioned `:any` dependency on `no`, `same`, or `foreign` is not accepted. Foreign-architecture installation, virtual `Provides:any`, co-installation (`Multi-Arch: same`) and package-manager architecture selection are still not implemented. The parser continues to refuse unsupported architecture/build-profile expressions; **no dpkg changes are made**.

This is a bounded read-only diagnostic, **not** full Debian Multi-Arch resolution. See Debian Policy §5.6.34 for the semantics of the four values.

### Installed architecture identity index (read-only)

The dpkg installed-state reader now retains an explicit (package name, architecture) identity, so two installed copies such as a native and foreign library cannot silently overwrite one another. The legacy per-architecture view is derived from the keyed index and refuses duplicate native/all identities. Installed version queries use the keyed index as well. Experimental dependency planning remains **native-only** and refuses a plan step touching a package name that also has an installed foreign-architecture counterpart; unrelated foreign packages can remain in dpkg status. Full cross-architecture dependency solving, Multi-Arch: same co-installation, per-architecture transactions, file ownership and removals are **not implemented**.

### Repository catalog keyed by binary architecture

The exploratory Packages catalog now retains package identity as `(name, architecture)` before constructing its limited native-only solver view. This distinguishes native `amd64`/`arm64` binary metadata from `Architecture: all`, prevents silent name overwrites and rejects conflicting metadata for identical name/architecture/version entries. When native and `all` entries share a name, the old name-keyed solver view **fails closed** until per-architecture candidate selection is implemented. Foreign-architecture dependencies and co-installation remain unsupported; the change neither performs dpkg transactions nor claims full APT parity.

### Keyed candidate selection in read-only dependency plans

Candidate metadata is now inserted against its exact `(name, architecture)` repository identity **before** the native-only solver projection. RPT rejects candidates that disagree with the selected native vs `Architecture: all` identity and refuses ambiguous same-name native/all catalog entries rather than falling back to an arbitrary package. Signed Release/index validation and policy pinning remain prerequisites to choosing the candidate. This increment does **not** implement cross-architecture solver state, `Multi-Arch: same` co-installation or foreign package transactions; the read-only solver still performs a guarded name-keyed projection.

### Keyed read-only dependency solver (native + Architecture: all)

The experimental solver now resolves concrete candidates and tracks visiting/completed dependency nodes by `(package name, binary architecture)` rather than unqualified names. A plan step carries the chosen architecture, including `Architecture: all`. Signed provenance and APT policy candidate selection continue to operate before plan acceptance. The native-only implementation rejects a package name that is ambiguous between native and `all`, and rejects foreign-architecture resolutions/co-installations. Installed status and conflict metadata retain their conservative name projection until full multiarch dpkg-state handling is implemented; this is **not** full APT parity, and no package transactions are enabled.

### Architecture-keyed installed safety and Conflicts/Breaks

Read-only dependency planning now reads complete dpkg installed safety metadata keyed by `(package, architecture)`, checks its agreement with the installed identity index, and keeps native/`Architecture: all` installed versions separate in the solver. The negative relationship checker uses exact binary keys for planned and installed packages, including reverse `Conflicts`/`Breaks` from installed dpkg status. A mismatch between the installed and candidate binary architecture fails closed, even when package versions match. Foreign installed identities are retained for safety validation and same-name overlap guards; **this does not implement cross-architecture dependency or conflict resolution, foreign transactions, or dpkg writes**.

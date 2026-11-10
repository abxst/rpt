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

`rpt verified-origins --cache ./rpt-lists --package bash --architecture amd64` prints one line per matching signed repository/index occurrence: version, architecture, repository URI, suite and index path. It revalidates the `InRelease` signature/freshness and each cached index SHA-256/size before reading metadata. Multiple repositories advertising an identical package version remain separate entries. This is intentionally **not** `apt-cache policy`: no pin priority, Release Origin/Label fields, candidate selection or transaction permission is inferred. It requires a fresh signed RPT cache and a valid Signed-By keyring.

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

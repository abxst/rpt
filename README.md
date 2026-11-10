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

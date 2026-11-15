# M9–M12 integrated qualification batch (experimental, no host activation)

This batch implements **bounded prerequisites** for all four requested
milestones; it does **not** declare any of them complete, nor make RPT a
production APT replacement. `/usr/bin/apt` must remain unchanged and
`scripts/stage-apt.sh activate` must continue to fail.

## Scope and evidence

| Milestone | Delivered in this batch | Still blocking production |
| --- | --- | --- |
| M9: transaction / recovery | Strict journal phase/diagnostic validation shared by writer and recovery inspector; reject symlink journal directories, malformed snapshot APT mark digests and overlapping roots | Secure VM/microVM maintainer script boundary, fsync/durability proof, true power-cut tests, atomic host boot/rootfs rollback |
| M10: resolver / policy | Deterministic 576-pair Debian version differential; 10 APT pin-priority points (previously 3) plus existing signed APT resolver oracles | Full SAT/virtual-provider ranking, arbitrary removals, conffiles, Multi-Arch install semantics, broad real-distro and negative-case parity |
| M11: APT-style CLI | `rpt apt-simulate-install` emits selected package/version sets in human-readable form or `--json`, with signed candidate policy and zero mutation; CI compares to `apt-get -s` on a shared signed fixture | Actual apt/apt-get/apt-cache option, prompt, exit-code and full action parity; normal `apt install` remains deliberately unsupported |
| M12: qualification / release | `scripts/production-gate.py` reports A–I evidence gaps and ten VM targets; enforces commit binding, positive/negative artifact digests, symlink refusal and no host authorization; CI negative tests; alpha prerelease includes BLOCKED qualification report | Collect/independently authenticate real evidence, all real positive and negative fixtures, canary safety review, performance benchmarks and stable release sign-off |

## Read-only CLI simulation

Run **only with independently authenticated RPT metadata cache** and the
matching APT root and dpkg status, preferably from an isolated fixture:

```sh
rpt apt-simulate-install --cache ./rpt-lists \
  --status ./fixture/var/lib/dpkg/status \
  --apt-root ./fixture/etc/apt --architecture amd64 \
  --json curl ca-certificates
```

The JSON has `simulation_only: true`, `apt_parity_verified: false`,
`host_execution_allowed: false` and `selected` entries that include
package, architecture, version, and prior installed version. There is no
implied permission to install. Unsupported graphs fail, not quietly
fall back to host APT. The signed fixture compares the normalized
selected set against `apt-get -s -y install` and verifies dpkg status is
unchanged. `apt-simulate-install` deliberately does not pretend to
support APT's complete flags or output syntax.

## Production qualification evidence contract (M12)

By default, this command is a *diagnostic* and prints `BLOCKED` with
every missing A–I gate and VM target. It never authorizes host execution.

```sh
python3 scripts/production-gate.py \
  --expected-commit "$(git rev-parse HEAD)"
# To enforce completeness in a separate, privileged review pipeline:
python3 scripts/production-gate.py \
  --expected-commit "$(git rev-parse HEAD)" \
  --manifest /path/to/verified-evidence/manifest.json --require-complete
```

A `--require-complete` invocation returns exit status 3 if evidence
is missing, 2 if evidence is invalid/tampered, and 0 only if all evidence
slots are filled and hash-verified. Even exit 0 means
`EVIDENCE_COMPLETE_REVIEW_REQUIRED`, **not host deployment authorization**.

Example incomplete manifest (test-only, *not* evidence of real passing):

```json
{
  "schema_version": 1,
  "source_commit": "0123456789abcdef0123456789abcdef01234567",
  "gates": {
    "A": { "status": "blocked" },
    "B": { "status": "blocked" }
  },
  "matrix": []
}
```

To claim a gate as passed, replace its entry with

```json
{
  "status": "pass",
  "positive": {"file": "A-positive.log", "sha256": "64-lowercase-hex-characters"},
  "negative": {"file": "A-negative.log", "sha256": "64-lowercase-hex-characters"}
}
```

The two files must be distinct, nonempty, regular files in the manifest
directory, each at most 8 MiB. Their names are basename-only and no
symlinks or repeated filenames are allowed. The gate's `source_commit`
must exactly match the full pinned Git commit SHA.

The ten target IDs are the cross-product of `ubuntu-22.04`,
`ubuntu-24.04`, `ubuntu-26.04`, `debian-12`, `debian-13` and
`amd64`/`arm64`. Every target requires:

```json
{
  "target": "ubuntu-24.04-amd64",
  "isolation": "vm",
  "status": "pass",
  "host_audit": {
    "file": "host-ubuntu-24.04-amd64.log",
    "sha256": "64-lowercase-hex-characters"
  }
}
```

`status: blocked` entries must have `host_audit: null`.
VM isolation is *self-reported* and requires independent verification.
For production review, collect artifacts from the exact pinned source
and tested binary; prove VM identity and host dpkg immutability;
validate positive/negative test outcomes against the reference APT
oracle; externally sign and archive artifacts. SHA-256 witnesses alone
do not prove who ran a test or prevent malicious fabrication.
The script **cannot** assert power-loss safety, authenticate artifacts,
verify a CI provider's attestations, or evaluate a test's semantic
correctness. These checks are a minimum evidence inventory, not a
certificate or automatic deployment policy.

## Developer verification

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo build
bash tests/version-corpus-differential.sh ./target/debug/rpt
bash tests/production-gate-contract.sh scripts/production-gate.py
# Full signed APT/RPT fixture only in disposable VM, never directly on host:
bash tests/signed-plan-differential.sh
```

The release workflow emits an `alpha` artifact with
`PRODUCTION-QUALIFICATION.json` explicitly `BLOCKED`; it must not
be marketed as an interchangeable APT replacement.

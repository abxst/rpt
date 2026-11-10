# APT replacement acceptance criteria

**Priority: behavioral APT replacement first; Rust-specific performance features second.**

RPT is currently an experimental, *read-only/download-only* implementation. The existence of a command or a green build does **not** mean compatibility.

## Release policy

- `alpha` / prerelease: partial implementation with explicit limitations.
- `1.0` (APT replacement candidate): all blocking suites below pass on clean, upgraded, broken-state, and offline Ubuntu test images. No alpha release is advertised as interchangeable with `/usr/bin/apt`.
- The deployment gate `scripts/stage-apt.sh activate` **must remain closed** until these acceptance tests pass in isolated VMs and canary machines.
- Do not move or overwrite `/usr/bin/apt` (or `apt-get`, `apt-cache`) during testing. This project cannot assume a running system's package database is disposable.

## Parity gates

| Gate | Minimum evidence | Status |
|---|---|---|
| A: apt CLI/config | `apt`, `apt-get`, `apt-cache` command/options/exit-code matrix, `apt.conf`, preferences and sources parsing | NOT MET |
| B: trusted metadata | InRelease/Release.gpg; Signed-By, date/expiry and Release-info changes, by-hash, mirrors, proxies, auth, source architecture, incremental updates | PARTIAL |
| C: candidate policy | pin priorities, NotAutomatic, target release, phased updates, held packages, multiarch; compare `apt-cache policy` and candidates | NOT MET |
| D: resolver | alternatives, versioned Provides, Pre-Depends, Conflicts/Breaks/Replaces, cycles, multiarch, Recommends, autoremove; differential `apt-get -s` | NOT MET |
| E: artifact acquisition | signed Release => Packages => .deb SHA256 and size, cache replay and tamper tests, mirror failover, disk limits | PARTIAL |
| F: package transactions | dpkg locks, unpack/configure/deconfigure, maintainer scripts, conffiles, debconf, triggers, root permissions, holds | NOT MET |
| G: failure recovery | interrupted dpkg, blocked scripts, signal/power loss, corrupt downloads, full disk, rollback/recovery and idempotent retries | NOT MET |
| H: system behavior | upgrade/full-upgrade, remove/purge, reinstall, autoremove, clean/autoclean, noninteractive and dry-run parity | NOT MET |
| I: benchmark | parity-complete workload on equal VMs; CPU/RAM/I/O/network/time with regression thresholds | DEFERRED |

Each gate needs **positive and negative** fixtures and reproducible CI evidence. Distinguish tests running against a disposable dpkg database from tests against the host. Mutating tests run only in isolated rootfs/VMs, never directly on the GitHub host runner.

## Differential test design

1. Prepare disposable Ubuntu 24.04 root filesystems with matched apt sources, signing keyrings, config, architectures, package state and cache snapshots.
2. Generate fixture classes: fresh install, same-version installed, downgrade, holds, pins, alternatives, virtual packages, Conflicts, Pre-Depends, packages with conffiles and maintainer scripts, half-configured, broken dependency graph.
3. Collect APT's reference output via `apt-cache policy`, `apt-get -s install/remove/upgrade/full-upgrade/autoremove` and package database snapshots.
4. Run RPT on the same fixture, normalize display-only differences, compare selected versions, install/remove set, dependency order, prompts, exit statuses, disk/network writes and resulting dpkg status.
5. Treat *every* unexplained difference as a failure. Include offline, 404, transient 503, invalid signature, expired Release, inconsistent By-Hash, truncated .deb, interrupted transaction, and disk-full cases.
6. Only after full parity, run repeatable APT-versus-RPT performance benchmarks. The existing parallel downloader is not evidence of faster end-to-end upgrades.

## Current implemented commands

`sources`, `version-compare`, `inspect-index`, `index-versions`, `update`, `cache-versions`, `installed-version`, `policy-preview`, `show`, `search`, `list`, `depends`, `rdepends`, `plan-install`, `prepare-download`, `fetch`.

`plan-install` is a deliberately incomplete read-only planner. `prepare-download` and `fetch` only acquire artifacts. RPT intentionally cannot execute real `install`/`remove`/`upgrade` transactions. **Never use RPT as the system APT executable on the current release.**

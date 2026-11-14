# Native disposable-rootfs install/upgrade (no Docker at execution)

RPT can now prepare and execute a signed, exact-version install/upgrade plan
against a **separate Linux root filesystem**, using real `dpkg --root`.
This is a functional bridge from Docker-only sandbox sessions to native
rootfs execution, not authorization to mutate the running host.

## Preconditions

- Linux amd64/arm64 environment with `dpkg`, `dpkg-deb`, `dpkg-query`,
  `gpgv`, root privileges for apply, and a complete disposable
  Debian/Ubuntu filesystem.
- Rootfs is a **directory under /tmp or /var/tmp**, not /, not a symlink,
  and contains no mounts or bind mounts.
- It contains `/usr/bin/dpkg`, `/bin/sh`, `/var/lib/dpkg/status`.
- The test operator explicitly creates this exact marker **inside the guest
  filesystem** after verifying its location:

    printf 'rpt-disposable-rootfs-v1\n' | sudo tee /tmp/my-guest/.rpt-disposable-rootfs

- Rootfs and any workspaces must be disposable; dpkg maintainer scripts run
  as root in a chroot and are **not a security boundary** against malicious
  scripts. Only trusted test fixtures on a disposable WSL distro or VM.
  This limitation matters even though host dpkg state is kept separate.

## Workflow

Use a **complete** test rootfs with correct architecture. Do not create an
empty folder and expect it to act like an OS; use a prebuilt Ubuntu/Debian
rootfs, e.g. exported from an official image.

    rpt update --root ./apt-config --output ./rpt-lists
    rpt native-rootfs-prepare-transaction \
      --rootfs /tmp/my-guest --cache ./rpt-lists --apt-root ./apt-config \
      --architecture amd64 --output ./native-intent.json demo-package
    rpt fetch --manifest ./native-intent.json --output ./rpt-artifacts
    sudo rpt native-rootfs-apply-transaction \
      --rootfs /tmp/my-guest --id native-001 \
      --transaction ./native-intent.json --artifacts ./rpt-artifacts \
      --cache ./rpt-lists --apt-root ./apt-config \
      --journal-dir ./rpt-native-journal

The same **disposable rootfs** also supports removal after a separate
dependency safety check (never a direct arbitrary dpkg invocation):

    sudo rpt native-rootfs-remove-planned \
      --rootfs /tmp/my-guest --architecture amd64 --id remove-001 \
      --journal-dir ./rpt-native-journal demo-package

Mark an installed guest package automatic or manual using **guest APT**, not
host APT:

    sudo rpt native-rootfs-mark \
      --rootfs /tmp/my-guest --id mark-001 \
      --journal-dir ./rpt-native-journal auto demo-package

For conffile cleanup, add `--purge`. Native purge runs `dpkg --purge`,
reports `mode: purge`, and rejects a successful postflight that leaves
residual `config-files` dpkg state. Unlike purge, normal remove keeps
conffiles. The conservative native purge planner accepts explicitly selected
**installed** binaries and `deinstall ok config-files` entries, including
a mix of both, while still refusing ambiguous identities and any installed
dependency closure that would break survivors. Ordinary `remove` refuses
config-files-only entries; use `--purge` to clear them after removal. Both
operations remain restricted to an explicitly marked, disposable rootfs.

For automatic orphans already marked in the **guest**
`/var/lib/apt/extended_states`:

    sudo rpt native-rootfs-autoremove-planned \
      --rootfs /tmp/my-guest --architecture amd64 --id auto-001 \
      --journal-dir ./rpt-native-journal

The first command derives a verified candidate closure from the guest's dpkg
status. The apply command rechecks the exact rootfs identity, dpkg status
SHA256, guest APT marks snapshot, policy, GPG/Release/index provenance,
and SHA256/size of each archive, stages copies, then runs real dpkg with
explicit `--root`. Within the **same supervised install journal**, it runs
the **guest's** `apt-mark auto` for newly installed dependency packages and
`apt-mark manual` for explicitly requested roots, verifying each mark and
that marking does not alter dpkg status. Dependencies already installed
before this transaction keep their original marks (including `manual`).
Postflight requires all exact installed versions and a clean dpkg audit.

This changes the install contract: a rootfs must include `/usr/bin/apt-mark`
and usable APT state. A failure during marking leaves the guest for inspection
and is not automatically rolled back. A **mark-only install** where no
package archive is needed is still unsupported by the native transaction
executor; use `native-rootfs-mark manual` explicitly for that case.

A journaled and supervised command handles SIGINT/SIGTERM/timeouts, including
process-group shutdown verification. On failure, the rootfs may be broken.
Do not rerun blindly; retain the rootfs and journal for manual inspection.

## Optional WSL2 smoke test (Ubuntu amd64)

Run from the RPT repository in a **disposable Ubuntu WSL2 distro**, as an
ordinary user (sudo is required for the separate guest filesystem):

    bash tests/wsl-native-rootfs-smoke.sh 2>&1 | tee ~/rpt-native-wsl-test.log

It uses `debootstrap` to create a separate Ubuntu rootfs, tests the signed
`hello` installation, removal, reinstallation, guest `apt-mark` manual/auto,
orphan autoremove and purge. A directly requested `hello` package must
become `manual` during install itself. It also installs a script-free local
conffile fixture **only into the disposable guest** and asserts that remove
retains its config while a separate `--purge` removes the residual
config-only state without reinstalling, and checks the **live WSL dpkg
status SHA256** on both success and failure. Workspaces under
`/tmp/rpt-native-wsl.*` are kept for inspection.

On environments where `/tmp` is mounted `nodev`, `debootstrap` can emit
a `test-dev-null: Permission denied` diagnostic even if the rootfs creation
continues successfully. The script reports `nodev` when detectable and
verifies the resulting rootfs; it does **not** remount the host or suppress
the diagnostic. An unsuccessful bootstrap or failed guest checks still fail
the test.

Real Ubuntu 26.04 `resolute` WSL2 v3 passed signed archive
verification, install/remove/reinstall, manual/auto marking, autoremove,
and conffile retention/deletion with unchanged live WSL dpkg status.
The earlier v3 fixture reinstalled a removed package before purging it;
this extended test instead purges a **config-files-only** package directly
after remove. The script is a disposable-rootfs acceptance test, not evidence
that privileged maintainer scripts are confined or that all APT semantics
are supported.

## Scope and limitations

- No Docker process is needed to *apply* the plan. The acceptance test
  exports official images to construct its test rootfs, but **RPT native
  execution does not call Docker**.
- Native **install, explicit upgrade, planned remove, purge, auto/manual
  marking and autoremove**. Full-upgrade and a live-host executor remain unsupported.
  Removal uses the conservative guest dpkg dependency planner; autoremove
  additionally binds guest APT extended states and rejects changed marks.
- Explicitly **refuses the live root /** and arbitrary host paths.
  This is not a flag away from host mutation.
- Not an unprivileged sandbox: chroot does not confine privileged scripts.
- The journal is cooperative and does not substitute for dpkg locking,
  full filesystem snapshots, or reliable automatic rollback. No production
  host execution and no general power-loss recovery is provided.
- RPT does not yet implement full APT policy, conffile prompting semantics,
  Multi-Arch transactions, or all dependency edge cases. Native install marking
  is verified against the signed transaction and the guest APT database, but
  is not a substitute for an all-or-nothing filesystem rollback.

The signed differential integration fixture now runs the RPT native
executor on a separate disposable rootfs and asserts that the VM host's
dpkg state is unchanged.

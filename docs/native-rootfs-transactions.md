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

The native install contract requires a guest `/usr/bin/apt-mark` and
usable APT state. If all requested roots are **already installed at the
verified candidate versions**, `native-rootfs-prepare-transaction` produces
an empty archive plan. That plan can now be applied as a **journaled mark-only
transaction**: it rechecks the exact guest dpkg status snapshot, signed
candidate policy and preflight guest APT marks, and calls only the guest
`apt-mark manual` (no dpkg `--configure`, maintainer scripts, or `.deb`
artifacts). A dummy `--artifacts` directory is not required for this
zero-archive operation.

A failed mark or interrupted install leaves the disposable guest and journal
for inspection; no automatic rollback is promised. A mark-only plan cannot
change installed package versions or bypass the rootfs/path guard.

A journaled and supervised command handles SIGINT/SIGTERM/timeouts, including
process-group shutdown verification. On failure, the rootfs may be broken.
Do not rerun blindly; retain the rootfs and journal for manual inspection.

Use the **read-only recovery diagnostic** to inspect the guest's current
SHA256 dpkg status, `dpkg --root ... --audit` output, and recorded transaction
phases in one JSON report:

    sudo rpt native-rootfs-recovery-inspect \
      --rootfs /tmp/my-guest --journal-dir ./rpt-native-journal

The report exposes `needs_operator_review` when dpkg reports inconsistencies,
journal files are invalid/incomplete, or a transaction has failed or has not
finished. It does not change dpkg, rewrite journal entries, unlock recovery
gates, restore filesystem files, or imply it is safe to retry. This is an
**observational snapshot** (a transaction could change state concurrently),
not a cryptographic snapshot or a rollback guarantee.

## Copy-based native checkpoint and restore to a NEW guest

Before a planned disposable-guest transaction, capture its current filesystem.
This operation acquires the same cooperative journal lock used by RPT
transactions and **refuses to snapshot an unfinished or corrupt journal**:

    sudo rpt native-rootfs-snapshot-create \
      --rootfs /tmp/my-guest \
      --snapshot-rootfs /tmp/my-guest-checkpoint \
      --journal-dir ./rpt-native-journal --id tx001-baseline

The destination must be absent, under /tmp or /var/tmp, outside the source
and journal, and must never contain a mount. The source must be a marked,
mount-free disposable rootfs with a clean dpkg audit. Checkpoints are
**full, copy-based rootfs clones**, using GNU cp's archive and
reflink-if-available mode. RPT hashes the source tree before copying, verifies
the copied tree (regular file bytes, relative names, symlink targets,
mode/uid/gid and device type), and rehashes the source afterward while
holding the cooperative lock. It refuses a changed source, a failed copy,
or a divergent snapshot. Partial copies are left for manual inspection.

A durable, exclusively created checkpoint receipt is written to
`JOURNAL_DIR/native-rootfs-snapshots/tx001-baseline.json`; it contains
source/snapshot path identities, tree hash, dpkg status hash, guest APT
marks hash and creation time. Verify it any time:

    sudo rpt native-rootfs-snapshot-verify \
      --receipt ./rpt-native-journal/native-rootfs-snapshots/tx001-baseline.json

If the original guest later fails a transaction, inspect its dpkg/journal
before manually restoring the checkpoint **to a fresh directory**:

    sudo rpt native-rootfs-snapshot-restore \
      --receipt ./rpt-native-journal/native-rootfs-snapshots/tx001-baseline.json \
      --new-rootfs /tmp/my-guest-restored

The original guest, journal and checkpoint remain untouched. The restored
guest must reproduce the exact tree hash, dpkg status and a clean dpkg audit.
An existing destination or modified checkpoint fails closed. The snapshot
can be created before the failure and restored even when the original's
journal contains an interrupted transaction; it **never clears that gate**.

### Opt-in pre-transaction checkpoint hook

Native `native-rootfs-apply-transaction`, `native-rootfs-remove-planned`
(including `--purge`), `native-rootfs-mark`, and
`native-rootfs-autoremove-planned` also accept
`--checkpoint-rootfs /tmp/NEW-GUEST-BEFORE`. This **opt-in** option
creates and verifies a full guest checkpoint **after plan/preflight checks
but before launching the mutating dpkg/apt-mark command**. The transaction
ID must be 1–57 safe characters. Receipts are written as
`JOURNAL_DIR/native-rootfs-snapshots/<TRANSACTION_ID>-before.json`.

For example:

    sudo rpt native-rootfs-remove-planned \
      --rootfs /tmp/my-guest --architecture amd64 --id remove001 \
      --journal-dir ./rpt-native-journal \
      --checkpoint-rootfs /tmp/pre-remove001 demo-package

If checkpoint creation or verification fails, no package mutation is started.
A nonempty autoremove gets a checkpoint; an empty removal plan has nothing to
mutate and therefore takes no checkpoint. Existing commands **without** this
option behave as before.

An installed-state or APT-marks mismatch detected after checkpoint creation
also aborts the package operation, retaining the copied guest and receipt
for investigation. These operations remain separately journaled; there is
a window after the cooperative checkpoint lock is released and before the
package transaction lock is acquired. RPT rechecks state as part of
transaction execution, but the copy and dpkg commit are **not an atomic
all-or-nothing operation**. Recovery is always manual into a different guest.

**Limitations:** This is a trusted, *offline/disposable* filesystem copy,
not an atomic Btrfs/ZFS/LVM snapshot or an ACID package transaction. The
tree witness does not prove hardlink topology, xattrs, ACLs, application
process state, external device effects or power-loss persistence. The
journal lock coordinates RPT processes only, not arbitrary host writers.
GNU cp and the guest filesystem must be trusted; privileged maintainer
scripts are still not contained by chroot. Never use this to snapshot live
`/` or enable host package mutation. A checkpoint is **explicit** and does
not automatically wrap every RPT transaction.

## Optional WSL2 smoke test (Ubuntu amd64)

Run from the RPT repository in a **disposable Ubuntu WSL2 distro**, as an
ordinary user (sudo is required for the separate guest filesystem):

    bash tests/wsl-native-rootfs-smoke.sh 2>&1 | tee ~/rpt-native-wsl-test.log

It uses `debootstrap` to create a separate Ubuntu rootfs, tests the signed
`hello` installation, removal, reinstallation, guest `apt-mark` manual/auto,
orphan autoremove and purge. A directly requested `hello` package must
become `manual` during install itself; the extended test also marks
`hello` auto, prepares a zero-archive install for the already installed
candidate, then applies it as mark-only while verifying that the guest dpkg
status hash never changes. It also installs a script-free local
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

Real Ubuntu 26.04 `resolute` WSL2 v6 passed signed archive
verification, install/remove/reinstall, automatic manual marking, **journaled
zero-archive mark-only** (`auto` to `manual` without changing dpkg status),
autoremove, and direct purge of **config-files-only** packages, with unchanged
live WSL dpkg status. The new read-only recovery-inspection test needs to pass
independently on WSL to validate its reporting. This remains a disposable-rootfs
acceptance test, not evidence that privileged maintainer scripts are confined
or that all APT semantics are supported.

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
  atomic filesystem snapshots, or reliable automatic rollback. The explicit,
  verified full-tree clone/restore workflow creates a **new guest**; it does
  not provide a crash-atomic commit or restore the live host. No production
  host execution or general power-loss recovery is provided.
- RPT does not yet implement full APT policy, conffile prompting semantics,
  Multi-Arch transactions, or all dependency edge cases. Native install marking
  is verified against the signed transaction and the guest APT database, but
  is not a substitute for an all-or-nothing filesystem rollback.

The signed differential integration fixture now runs the RPT native
executor on a separate disposable rootfs and asserts that the VM host's
dpkg state is unchanged.

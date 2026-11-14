# Native disposable-rootfs install/upgrade (no Docker at execution)

RPT can now prepare and execute a signed, exact-version install/upgrade plan
against a **separate Linux root filesystem**, using real \`dpkg --root\`.
This is a functional bridge from Docker-only sandbox sessions to native
rootfs execution, not authorization to mutate the running host.

## Preconditions

- Linux amd64/arm64 environment with \`dpkg\`, \`dpkg-deb\`, \`dpkg-query\`,
  \`gpgv\`, root privileges for apply, and a complete disposable
  Debian/Ubuntu filesystem.
- Rootfs is a **directory under /tmp or /var/tmp**, not /, not a symlink,
  and contains no mounts or bind mounts.
- It contains \`/usr/bin/dpkg\`, \`/bin/sh\`, \`/var/lib/dpkg/status\`.
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

The first command derives a verified candidate closure from the guest's dpkg
status. The apply command rechecks the exact rootfs identity, dpkg status
SHA256, policy, GPG/Release/index provenance, and SHA256/size of each
archive, stages copies, then runs real dpkg with explicit \`--root\`. Postflight
requires all exact installed versions and a clean dpkg audit.

A journaled and supervised command handles SIGINT/SIGTERM/timeouts, including
process-group shutdown verification. On failure, the rootfs may be broken.
Do not rerun blindly; retain the rootfs and journal for manual inspection.

## Scope and limitations

- No Docker process is needed to *apply* the plan. The acceptance test
  exports official images to construct its test rootfs, but **RPT native
  execution does not call Docker**.
- Native **install and explicit upgrade** only. Remove, purge, auto/manual,
  autoremove, and full-upgrade are not yet implemented in this backend.
- Explicitly **refuses the live root /** and arbitrary host paths.
  This is not a flag away from host mutation.
- Not an unprivileged sandbox: chroot does not confine privileged scripts.
- The journal is cooperative and does not substitute for dpkg locking,
  full filesystem snapshots, or reliable automatic rollback. No production
  host execution and no general power-loss recovery is provided.
- RPT does not yet implement full APT policy, conffile prompting semantics,
  Multi-Arch transactions, or all dependency edge cases.

The signed differential integration fixture now runs the RPT native
executor on a separate disposable rootfs and asserts that the VM host's
dpkg state is unchanged.

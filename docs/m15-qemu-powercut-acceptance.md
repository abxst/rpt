# M15 — Real QEMU guest power-cut and offline disk recovery oracle

**Status: experimental acceptance infrastructure, NOT a production release
gate PASS.** This milestone adds a true independently booted Linux kernel
and virtual block device to the existing Docker and native-rootfs tests.

Previous M8/M13/M14 termination tests kill a guest `dpkg`/RPT process
or its process group on the GitHub runner. That is **not** equivalent to
killing an entire operating system while its filesystem is mounted.

## New workflow

`.github/workflows/qemu-powercut-acceptance.yml` runs on a fresh
`ubuntu-24.04` GitHub Actions VM on every PR or manual dispatch.

1. Install QEMU, `cloud-localds`, `gpgv`, and related test-only
   dependencies on the **disposable runner**.
2. Download Canonical's Ubuntu 24.04 `noble-server-cloudimg-amd64.img`.
   Verify the official `SHA256SUMS.gpg` using the trusted Ubuntu
   `/usr/share/keyrings/ubuntu-cloudimage-keyring.gpg`, then verify
   the exact QCOW2 image SHA-256 from the signed manifest.
3. Generate a fresh ephemeral SSH key and cloud-init NoCloud seed.
   Boot **a separate QEMU guest kernel** with a dedicated virtual disk,
   no graphical interface and QEMU user-mode networking restricted to
   explicitly forwarded loopback SSH. Use KVM when actually accessible;
   TCG otherwise. No silently skipped VM checks.
4. Install harmless local `rpt-powercut` v1.0 through the guest's
   actual `dpkg`, verify clean guest `dpkg --audit` and shut down
   gracefully. Copy the complete, offline QCOW2 disk to a protected v1
   baseline; record its SHA-256.
5. Boot a fresh copy-on-write disk backed by that baseline; ask the
   guest's `systemd` to start `dpkg --install` for v2.0. The
   v2 `postinst` writes a small marker, requests its file flush,
   and deliberately remains alive.
6. **Immediately SIGKILL the entire dedicated QEMU PID** only once
   the guest reports that the postinst marker is present. Never send
   SIGTERM to `dpkg` and never target the runner's own process group.
   The harness validates the PID and exact test disk path before kill.
7. Restart the exact interrupted QCOW2 as a new QEMU instance, assert
   that guest `dpkg --audit` is nonempty, v2.0 is not fully installed
   and the postinst marker survived. An unexpectedly clean/full install
   is a **failed** fault-injection test, not success.
8. Shut down the failed VM. Preserve its failed disk and digest; boot
   a **different**, new QCOW2 overlay backed by the untouched v1 disk.
   Verify restored v1.0 installed, `dpkg --audit` empty, no v2 marker,
   baseline digest unchanged, failed overlay untouched by restoration
   and runner dpkg/APT state unchanged.
9. Save the evidence schema plus serial logs as a GitHub workflow
   artifact, then validate the evidence contract with
   `scripts/validate-powercut-evidence.py`. No artifact can set
   `production_ready: true` or claim RPT's transaction is complete.

## How to run

```sh
# Only in an expendable VM runner, never on the live package host:
sudo apt-get install qemu-system-x86 qemu-utils cloud-image-utils ubuntu-keyring
bash -n tests/qemu-powercut-oracle.sh
bash tests/qemu-powercut-oracle.sh
```

The test deliberately mutates the *runner's* packages only when
installing its QEMU prerequisites. After setup, it records dpkg status,
APT extended marks, holds and audit, and asserts they remain unchanged
throughout the QEMU transaction test. The guest uses its own virtual disk;
nothing binds the host's `/var/lib/dpkg` into that guest.

## Integrity and scope limitations

- `SIGKILL QEMU` is an abrupt guest power cut, **not** a physical
  power failure of the CI runner's host disk or storage controller.
  QEMU `cache=writeback`, Linux buffering, and CI's backing disk
  durability are not independently certified.
- The independent v1 disk gives a defensible **offline fresh-disk
  restore** of the narrow sample `dpkg` workload. It is **not**
  an atomic rollback of arbitrary installed host packages, services,
  external databases, or boot selection.
- **This M15 oracle runs raw `dpkg` inside QEMU**, not RPT's signed
  native-rootfs executor. It tests that the VM/power-cut infrastructure
  can observe a real interrupted package and recover by selecting a new
  disk. It does not prove RPT writes correct journal or checkpoint
  phases under a VM power cut. That integration remains open.
- It currently covers one Ubuntu 24.04 amd64 guest and one failpoint
  (postinst entered). More guest distributions and cut points
  (unpack, configure, trigger, fsync, boot) remain required.
- The evidence JSON is self-reported by the harness. A hash is not an
  attestation of runner identity, exact source commit, or actual power
  loss. GitHub workflow records and serial artifacts need independent
  verification before being included in any acceptance review.
- GitHub does **not guarantee** nested KVM support on hosted runners.
  The harness explicitly reports whether KVM or fully emulated QEMU
  TCG was used; it never returns PASS without executing the guest.

## Still required for production

1. Implement an RPT-native signed intent, journal and verified
   checkpoint *within a dedicated QEMU guest disk*, rather than only
   a standalone raw dpkg fault-injection transaction.
2. Add ordered failure points across the full package lifecycle and
   real powered-off qcow images with integrity/durability witnesses.
3. Design transactional OS boot selection, service restart and
   rollback to new disks with precise operator controls.
4. Meet APT parity gates A–I for behavior, policy, resolver, full
   Multi-Arch, package transactions and benchmark certification.

`scripts/production-gate.py` continues to report `BLOCKED`, and
`scripts/stage-apt.sh activate` continues to reject replacement of
`/usr/bin/apt`.

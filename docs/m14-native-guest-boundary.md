# M14 — Native guest host-boundary preflight (experimental)

RPT's existing native `dpkg --root` test runner executes only against an
explicitly marked disposable guest under `/tmp` or `/var/tmp`. **Chroot is
not a security boundary**, even with a clean checkpoint. M14 hardens the
preflight against a specific host-aliasing failure class; it is **not a VM
isolation implementation or power-loss recovery certification**.

## New fail-closed checks

- Resolve `var/lib/dpkg/status`, `usr/bin/dpkg`, and `bin/sh` from
  the host namespace before any guest dpkg operation; all resolved targets
  must remain under the canonical guest rootfs. An ordinary relative
  `/bin -> usr/bin` / `sh -> dash` usrmerged layout is accepted.
  A planted absolute `/bin/sh` symlink resolving to host `/bin/sh`
  is rejected, even if it could behave differently inside a chroot.
- `var/lib/dpkg/status` must be a regular, direct guest file with no
  external hardlink alias. Host-side `dpkg --root=...` and RPT snapshot
  operations must never follow it through a guest-to-host symlink.
- Recursively examine all entries in the guest's `var/lib/dpkg` and
  existing `var/lib/apt` state trees. Refuse symlinks and special
  files in those trees; for regular hardlinks, count the distinct
  observed paths per `(device,inode)` and compare against each inode's
  actual `st_nlink`. Internal hardlink groups may pass, but any
  unobserved outside alias fails closed. The traversal is bounded to
  100,000 entries and 64 levels.
- Bind read-only dpkg-status/apt-mark readers to the same guarded
  guest-only pathname and regular-file requirements.
- These checks execute on *every* native guest rootfs preflight,
  including signed plan preparation, checkpoint creation, verified
  restore, and recovery inspection. They do not unlock failed journals.

## Reproducible negative CI

`tests/native-guest-boundary.sh` is invoked by the existing signed
APT/RPT integration suite **only inside its disposable VM workspace**.
It clones the already authenticated, restored guest and injects:

| Injection | Expected behavior |
| --- | --- |
| Guest dpkg status hardlinked to an external host-visible file | Planning and checkpoint creation refused |
| Guest dpkg status replaced by a symlink to an external file | Same refusal |
| Symlink planted within guest dpkg info directory | Same refusal |
| Guest APT extended state hardlinked to an external file | Same refusal |
| Guest `/bin/sh` resolves to the actual host `/bin/sh` | Same refusal |

Every case asserts no generated plan, no newly created checkpoint,
external test inode unchanged and unchanged host dpkg/APT hashes.
After restoring the guest's original state, read-only recovery inspection
must return a clean dpkg audit. Unit tests separately verify that
ordinary internal usrmerge symlinks and hardlinks within the scanned
state tree remain valid.

### Limitations and next experiments

These are **pathname and inode hygiene checks**, not a defense against
arbitrary root-executed Debian maintainer scripts. They cannot prevent
host access by a privileged guest script using a compromised syscall
interface, open file descriptor, kernel feature, filesystem alias
outside the protected state directories, or a race with an external
writer between preflight and execution. File replacements after preflight
also remain a TOCTOU risk.

M14's broader production gates remain open:

1. Run untrusted maintainer scripts only in a true isolated VM/microVM
   with independent kernel and disposable virtual block device.
2. Implement verified block-device flush, boot selection, rollback and
   service-state policy; full-rootfs copy/checksum is not a crash-atomic
   storage transaction.
3. Inject guest **power cuts** at multiple dpkg/boot phases and verify
   disk state, journal recovery and rollback after remount/reboot.
4. Require independently authenticated VM evidence for the exact
   source/binary and preserve the production qualification BLOCKED state
   until all critical APT compatibility gates pass.

Do **not** run the native guest transaction flow against a real
production host, replace `/usr/bin/apt`, or interpret these new checks
as production approval.

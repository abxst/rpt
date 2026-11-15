# M8 — Signed native full-upgrade failure/termination recovery oracle

This is an **acceptance test**, not a production package executor. RPT still
refuses live `/`. The native `dpkg --root` code and its privileged maintainer
scripts **must run inside a disposable VM**; a chroot is not a security
boundary and a copied checkpoint is not atomic or power-loss-safe.

## Test entry point

`tests/signed-plan-differential.sh` builds a temporary GPG-signed APT
repository, installs v1 into a disposable guest, and runs the M7 combined
remove+signed-upgrade transaction. On successful M7 restoration, it invokes:

```bash
bash tests/native-full-upgrade-chaos.sh "$root" "$arch" ./target/debug/rpt
```

Do **not** run the chaos script directly against a host filesystem.
It requires M7's previously verified, independent v1 restored guest, cache,
and authenticated artifacts. It checks that its workspace is a temporary
`/tmp/rpt-signed.*` directory and only creates new marked guest copies
below that path. Use CI on disposable GitHub-hosted VMs, or an equally
disposable test VM. The parent test cleans up the rootfs copies after
completion. CI checks shell syntax separately; the full e2e run requires
Docker and sudo inside the disposable VM.

## Injected failures

| Test | Trigger in disposable guest | Expected journal | Expected recovery |
| --- | --- | --- | --- |
| `prerm-failure` | Obsolete v1 `prerm` sees guest-only `/etc/rpt-ci-fail-remove`, exits 43 before removal | `failed` | Operator review required |
| `sigterm-postinst` | New v2 `postinst` reports entry into its bounded sleep; runner sends SIGTERM to RPT's dedicated session leader | `interrupted` | RPT verifies process shutdown, then requires manual review |
| `sigkill-postinst` | Same marker; runner kills **only the dedicated test session**, including guest `dpkg` and scripts | `running` | No cleanup handler can run; journal remains unfinished and requires manual review |
| Tamper | After a valid SIGKILL checkpoint has been used for safe restore, mutate its dpkg status | Receipt verification fails | Restore is refused and cannot create a new guest |

All three transaction cases independently:

1. Copy M7's restored v1 baseline to a **new** disposable native guest.
2. Generate a separate signed candidate/removal intent bound to that guest.
3. Approve the SHA-256 of the exact intent and supply authenticated artifacts.
4. Require RPT to seal a full-tree checkpoint **before** any guest mutation.
5. Inspect the immutable journal + guest dpkg status without unlocking/retrying.
6. Verify the checkpoint receipt and restore into a **fresh** guest path.
7. Assert the restored installed v1/obsolete inventory, clean dpkg audit,
   absence of changes to the failed original guest, and unchanged host
   `/var/lib/dpkg/status`, APT extended state, holds and audit.

The postinst test-only hold is opt-in via a file **inside the guest**, not an
environment variable or a fault hook in production RPT code. It makes
signal timing reliable without allowing arbitrary host-process injection.
A dedicated `setsid` session ensures that SIGKILL cannot target the
runner's own process group. The script fails if it cannot positively verify
the dedicated session identity.

## M13 metadata fidelity extension (v2 checkpoints)

Every M8 signed fault scenario additionally creates a pair of **hardlinked**
guest files and attaches a `user.rpt.checkpoint` extended attribute. The
verified fresh recovery root must preserve both inode alias identity and
the original attribute value.

Negative tests deliberately mutate only a checkpoint's extended attribute
value (`prerm-failure`), break its hardlink relationship without changing
payload bytes (`sigterm-postinst`), or corrupt dpkg status
(`sigkill-postinst`). Each changed checkpoint must fail both
`snapshot-verify` and `snapshot-restore` *before* a destination is
created. After restoring the original xattr, the first receipt must
verify again.

V2 receipts and the `rpt-native-tree-v2` hash bind mtime, Linux xattrs,
hardlink topology, ownership, mode, bytes, symlinks and special file
identity; older v1 receipts are refused because their witness omitted
important metadata. See [native rootfs transactions](native-rootfs-transactions.md).

This is only an offline guest **consistency witness**. It does not
authenticate who created the receipt, synchronize arbitrary external
writers, flush all filesystem dirty data against power loss, contain a
privileged maintainer script, or make the guest execution atomic.

## What this proves—and what it does not

M8 verifies logical recovery behavior for three realistic failure points
in signed combined transactions. It tests journal failure semantics and
checkpoint integrity. **It does not demonstrate crash-atomic commit,
file-system durability under power loss, safe execution of untrusted
maintainer scripts, automatic rollback, or full APT solver parity.**

Remaining production P0 gates:

- Execute guest maintainer scripts in a real isolation boundary
  (disposable VM/microVM); mount namespace/chroot alone is insufficient.
- Define a fsync/transaction durability model for package payload, dpkg
  database, APT marks and journal; perform real VM power-cut tests.
- Establish a rollback/boot-selection strategy for the actual host and
  its services; never promote the current copy-based restore to automatic
  host rollback.
- Expand APT compatibility/differential acceptance, including policy,
  Multi-Arch and conflict/removal decisions beyond the bounded M7 solver.
- Gate production canary rollout on explicit acceptance evidence and a
  negative-path run for the exact release artifact.

No host package executor is enabled by this milestone.

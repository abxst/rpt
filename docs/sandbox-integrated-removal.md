# Live dpkg removal planning in persistent RPT sandboxes

RPT can now inspect installed dependency relationships in the **actual persistent
Docker test session**, rather than planning only against a host-provided
`/var/lib/dpkg/status` file. This enables both a read-only preview and a
separate, freshly calculated execution action in the isolated guest.

```sh
rpt sandbox-session-plan-remove --session dev01 --architecture amd64 libsample
rpt sandbox-session-plan-remove --session dev01 --architecture amd64 --cascade libsample

rpt sandbox-session-remove-planned --session dev01 --id tx-remove-1 \
  --journal-dir ./rpt-journal --architecture amd64 libsample

rpt sandbox-session-plan-autoremove --session dev01 --architecture amd64
rpt sandbox-session-autoremove-planned --session dev01 --id tx-auto-2 \
  --journal-dir ./rpt-journal --architecture amd64
```

Implementation details:

- Reads `/var/lib/dpkg/status` and, for autoremove, APT
  `/var/lib/apt/extended_states` *from the sandbox container* using
  `docker exec`. Missing auto/manual state fails closed.
- Parses and validates an **in-memory snapshot**; no host dpkg files or temp
  status files are needed.
- Plans alternatives, version constraints and virtual `Provides` with existing
  conservative removal resolver; held/Essential/Protected packages block the
  proposed transaction.
- Recomputes the removal closure immediately before calling the RPT
  journal-supervised executor. Inside the container, the executor checks the
  SHA-256 of the current dpkg status (and extended_states for autoremove)
  against the exact snapshot used for planning before invoking dpkg.
- Runs `dpkg --remove --` on only the planned binary identities. No
  `--force-*` flags, host-root mounts or host package mutations are used.
- A transaction failure stops the container for operator investigation.
  Successful transaction state remains in the same Docker session.

**These are development-only transactions.** Full APT removal semantics and
protection rules are not implemented. `removable: true` is a restricted
diagnostic, not production authorization. The digest preflight detects ordinary
stale snapshots, but is not an atomic lock against arbitrary concurrent Docker
exec calls; direct Docker access must be restricted. The older
`sandbox-session-remove` raw dpkg operation remains available for manual
sandbox experiments; use the `*-planned` commands for integrated planning.

Autoremove requires reliable APT auto/manual marks inside the container.
Packages absent from `extended_states` are treated as manual. Missing marks
are never inferred to mean a package is safe to remove. No persistent host
transaction execution is exposed.


## Journaled manual/automatic package marks

APT manual/automatic state is now editable **inside the isolated Docker
sandbox only**, without manually copying or editing extended_states:

~~~sh
rpt sandbox-session-mark --session dev01 --id mark-lib-auto \
  --journal-dir ./rpt-journal auto libsample

rpt sandbox-session-mark --session dev01 --id mark-lib-manual \
  --journal-dir ./rpt-journal manual libsample

rpt sandbox-session-plan-autoremove --session dev01 --architecture amd64
~~~

The command resolves all requested names against the guest's installed
binary identities before mutation. Ambiguous Multi-Arch names require an
explicit architecture; unknown, duplicate and malformed requests are rejected.
A missing initial APT extended_states file is allowed, while nonregular files
and read errors fail closed.

The operation uses the existing cross-process session lock, verified
pre-transaction Docker checkpoint, durable journal and supervised execution.
Immediately before apt-mark, it verifies guest dpkg status and extended_states
against their captured SHA-256 digests. Afterwards it confirms the new marks,
checks that dpkg status remains byte-for-byte unchanged, and runs dpkg --audit.
A failure stops the sandbox and preserves forensic evidence.

Marking a package automatic does not immediately remove it: the subsequent
autoremove plan still enforces dependency and protected-package checks.
Automatically marking new dependency packages during an install requires
future binding to the exact signed resolver plan and is not yet implemented.
Host APT/dpkg files remain untouched.

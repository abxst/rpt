# Cross-process single-writer locks for persistent Docker sandboxes

Every RPT-managed mutation of a named persistent sandbox now acquires an
exclusive advisory lock for the entire operation: create, destroy, install,
upgrade, remove and purge, including planned and raw sandbox-only paths.

The lock is stable per sandbox identity under the invoking account's
HOME/.rpt-sandbox-session-locks directory (private mode 0700 on Unix).
Lock files are created mode 0600. RPT rejects symlink/nonregular lock
paths and refuses concurrent ownership rather than waiting indefinitely.

This lock is independent of the transaction journal directory: two RPT
commands using different --journal-dir values still cannot concurrently
mutate the same Docker sandbox while one holds its session lock.

The existing journal remains responsible for durable Prepared/Running/
Succeeded/Failed/Interrupted records and recovery gating. Session locking
does NOT replace dpkg's database locks or the bound plan's SHA256 status
preflight and version postconditions.

CI holds a genuine signed v1.0 -> v2.0 upgrade open using a sleeping
maintainer postinst, then runs an independent RPT removal command against
the same sandbox with another journal path. The competing command must
fail with a session-ownership error; the upgrade must complete.

## Limitations

This is a *cooperating-process* lock scoped to one local HOME. Running RPT
as a different Unix account with a different HOME, or invoking Docker
directly, bypasses this lock. Do not consider it an atomic or distributed
lock. Direct daemon access must be restricted and the Docker host should
be disposable. SIGKILL can still leave a daemon-owned process running;
an unfinished journal requires operator inspection. No native host dpkg
mutation is enabled; production APT parity and rollback remain incomplete.

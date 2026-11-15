# M29 — Cooperative shared leases during signed planning

`rpt update` has an exclusive advisory lock at `<cache>/.rpt-update.lock` across staged repository index swaps. Signed candidate-policy planners previously verified their signed cache, then could read a partially swapped directory if another cooperative update began. M29 introduces `acquire_cache_read_lease(cache)` using the same filesystem lock with `fs2::FileExt::try_lock_shared`.

Lease lifetime extends across the native and foreign package dependency-policy closure and direct system-upgrade discovery. Both legacy manifest preparation and exact signed artifact manifest revalidation also hold the lease. Missing, symlinked, or replaced lock inodes are rejected. A busy updater yields a deterministic error; readers do not wait indefinitely for a lock that may be held by a stalled process.

Tests: two concurrent shared readers block an exclusive writer until the last drops; a missing or symlinked lock is rejected. A signed local APT/RPT differential fixture holds the exclusive update lock in an independent process and asserts the native install simulation and upgrade discovery refuse to proceed.

This is a **cooperative advisory lock**, not an anti-tamper protection against unrelated processes, nor an atomic transaction for filesystem reads by an adversary. GPG, SHA256 and source-policy checks remain mandatory. It does not authorize host dpkg execution or claim APT compatibility.

# M32 — Harden exclusive metadata update cache lease

RPT update previously opened `<cache>/.rpt-update.lock` with `create(true)` and followed a symlink if one was planted before the open. This undermined the shared reader lock introduced in M29 and could cause unexpected files to be locked. M32 uses a single `acquire_update_lock` helper:

- Require an existing ordinary directory, rejecting a symlink final cache root;
- On a missing lock, atomically create only a new regular file using `create_new(true)` and fail if another process races to create it;
- On an existing lock, reject symlinks and nonregular files, compare device and inode before opening and after opening;
- Acquire the exclusive advisory file lock without blocking and then check that the path still names the same inode.

Unit tests verify normal creation/reuse, another lock-holder exclusion, rejecting symlink cache roots and lock files, and that a symlink target is not modified. The normal signed update differential, ARM64 and disposable rootfs tests continue to exercise the updater end-to-end.

This does not protect against an adversary who can freely rename the cache root or lock **after** validation; it is defense in depth for a cooperatively managed cache, not a hostile multiuser trust boundary. Real host transactions remain unsupported.

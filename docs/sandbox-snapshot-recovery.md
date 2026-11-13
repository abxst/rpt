# Docker sandbox snapshot & recovery (development-only)

Every journal-supervised RPT **persistent sandbox mutation** now first
creates a Docker image checkpoint with --pause=true, before installing,
upgrading, removing or purging packages. It independently launches the
committed image with no network to confirm a clean dpkg audit and an exact
SHA-256 match with the source dpkg status. If checkpoint verification fails,
RPT refuses the package transaction rather than running dpkg without recovery
evidence.

A durable JSON receipt is written to:

    JOURNAL_DIR/sandbox-snapshots/SESSION--TRANSACTION_ID.json

Receipts record the source sandbox and container IDs, transaction ID,
immutable Docker image SHA-256 ID, original dpkg status SHA-256 and creation
time. Receipts are created exclusively and fsynced, never overwritten.

## Inspect and restore

    rpt sandbox-snapshot-verify \
      --receipt ./rpt-journal/sandbox-snapshots/dev01--tx001.json

    rpt sandbox-snapshot-restore \
      --receipt ./rpt-journal/sandbox-snapshots/dev01--tx001.json \
      --new-session recovered01

The restore command first checks the image's RPT labels and immutable ID,
then creates a **new** network-isolated and capability-restricted RPT Docker
sandbox. It compares its dpkg status SHA-256 to the snapshot and requires a
clean dpkg audit. It refuses to overwrite the original sandbox. A failed
verification removes only the new container.

The crashed/failed original container, snapshot image and journal are left
unchanged for forensic inspection. Restoring does NOT transition an unfinished
journal to Succeeded or authorize retry. Use the existing recovery inspector
and human-reviewed recovery attestation workflow to handle any unfinished
record separately. Do not delete the original before saving required evidence.

## SIGKILL integration test

CI builds two signed .deb package versions with an actual maintainer postinst.
It starts a 1.0 to 2.0 upgrade, waits until the 2.0 postinst runs, sends
SIGKILL to RPT, forcibly stops the original Docker container, verifies the
running journal and checkpoint receipt, restores a clean v1.0 container
under a different session name, checks the newer dependency is absent, and
proves the journal's unfinished-transaction gate has not been cleared.

## Important limitations

This is a Docker **development sandbox checkpoint**, not a general-purpose
Linux snapshot or production rollback mechanism. Docker commit snapshots a
container's writable filesystem, not external volumes; RPT refuses containers
with Docker mounts to avoid claiming those are recoverable. Direct Docker
access, other Unix accounts and other Docker hosts can bypass cooperating
RPT locks. A power loss or Docker daemon failure can invalidate a commit
before its receipt is fsynced; orphaned images may require manual cleanup.

Container-internal maintainer scripts may affect external services not
captured by Docker commit; network is disabled by this sandbox but the Docker
daemon is privileged. A clean dpkg database is necessary, not sufficient, for
arbitrary application consistency. Snapshot image storage can accumulate
until manually pruned; do not remove images still named in receipts.

Never use this workflow to mutate the host dpkg database. Full APT parity,
production recovery semantics and safe host execution are not implemented.

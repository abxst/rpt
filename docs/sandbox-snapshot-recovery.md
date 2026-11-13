# Crash checkpoints and explicit recovery for RPT Docker sandboxes

Every status-bound sandbox install/upgrade now captures a full Docker rootfs
checkpoint BEFORE dpkg can modify the guest. Docker commit pauses the sandbox
during checkpoint creation. RPT rejects containers with external mounts,
validates Docker's immutable image ID, records the container ID and SHA-256
of the exact pre-transaction dpkg status, and fsyncs checkpoint metadata
under the transaction journal's sandbox-checkpoints directory.

The checkpoint covers the container's writable filesystem. It does NOT cover
volumes or external side effects; the sandbox forbids all mounts and networking.

## Failure investigation

    rpt sandbox-session-recovery-inspect \
      --session dev01 --id upgrade-001 --journal-dir ./rpt-journal

This command emits JSON containing journal phase, original container identity,
checkpoint image, running state and whether manual restoration is eligible.

After a crash, STOP THE DAMAGED CONTAINER explicitly and confirm it is quiescent:

    docker stop --time 3 rpt-session-dev01

Then restore the checkpoint:

    rpt sandbox-session-recovery-restore \
      --session dev01 --id upgrade-001 --journal-dir ./rpt-journal

RPT refuses to restore a running sandbox, a successful transaction, a
different container, a missing checkpoint image, a replayed restoration or
invalid metadata. It acquires the per-session single-writer lock before acting.

Recovery renames the stopped, failed container to a unique quarantine name,
recreates the original sandbox name from the immutable checkpoint image with
network disabled and all capabilities dropped, and checks the restored dpkg
status SHA-256 against the recorded pre-transaction digest. On failure, RPT
attempts to remove the new container and rename the quarantine back. It
retains the original failed container, immutable image and metadata for
forensics and manual cleanup, rather than silently deleting the evidence.

The journal record is NOT rewritten by this restore and may still read
Running/Interrupted. That original recovery gate remains locked until a
separate operator review; the existing recovery-attest records operator
evidence without bypassing an unfinished transaction. No automatic retry
of dpkg/maintainer scripts occurs.

## CI crash exercise

CI runs a real signed demo upgrade from 1.0 to 2.0 where postinst sleeps.
It sends SIGKILL to RPT after dpkg enters a partially upgraded state,
verifies the original journal stays Running, refuses restore while Docker
is running, explicitly stops the guest, restores the checkpoint, verifies
the exact original 1.0 version and a clean dpkg audit, and confirms the
damaged container is preserved under the quarantine name.

## Boundaries

- This is disposable Docker rootfs recovery, NOT host recovery or full APT
  transaction rollback. Docker commit does not roll back external systems.
- A SIGKILL can leave a daemon-owned docker exec process running. Never run
  restore before the guest is stopped and the process state is investigated.
- Direct Docker commands and RPT running under a different HOME can bypass
  cooperative RPT session locks. Use a dedicated development Docker daemon.
- The journal recovery gate is intentionally not lifted by snapshot restore;
  claiming full recovery without operator review would be unsafe.
- Checkpoint images, quarantined containers and metadata use disk space and
  require explicit retention/garbage-collection policy outside this milestone.

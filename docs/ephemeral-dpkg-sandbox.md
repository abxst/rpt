# Ephemeral Docker dpkg transaction execution (experimental)

RPT now performs genuine dpkg unpack, configure and trigger processing in an
**ephemeral Docker container**, not on the host. This is a disposable integration
sandbox, not a replacement for APT or a production transaction executor.

## End-to-end workflow

First run the existing signed repository update and exact-version package
download workflow, then pre-pull a Debian/Ubuntu image containing dpkg.

```sh
docker pull debian:bookworm-slim
rpt sandbox-install \
  --manifest ./locked-manifest.json \
  --artifacts ./rpt-artifacts \
  --cache ./rpt-lists \
  --architecture amd64 \
  --image debian:bookworm-slim \
  --journal-dir ./rpt-journal \
  --id test-001 \
  --timeout-seconds 600
```

- Revalidates signed repository provenance and all downloaded artifact hashes.
- Makes a private verified copy of each archive before container execution.
- Uses a disposable Docker container with no network, dropped capabilities,
  no-new-privileges, memory/PID/CPU limits and **only a read-only archive mount**.
  Host dpkg, /etc, /usr, /var and the Docker socket are not mounted.
- First compares Package/Version/Architecture fields embedded in each .deb
  with the exact signed plan identity; any mismatch fails before dpkg install.
- Optional --lifecycle install-remove or --lifecycle install-purge tests dpkg
  removal/purge of the newly installed packages in that SAME disposable container.
  This does not execute an arbitrary removal plan or modify a persistent root.
- Runs dpkg install, pending configure, pending triggers and dpkg audit inside
  the container. The container's writable filesystem is discarded on exit.
- Uses the existing single-writer transaction journal and supervised timeout/
  signal handling for the Docker client. After completion/interruption the
  runner explicitly attempts to remove any daemon-owned container.
- Fails closed on missing/unverified artifacts, unsupported architecture and
  journal recovery warnings.

## Lifecycle smoke tests

Run either operation using the same verified manifest and a new transaction ID:

```sh
rpt sandbox-install --manifest ./locked-manifest.json \
  --artifacts ./rpt-artifacts --cache ./rpt-lists \
  --journal-dir ./rpt-journal --id smoke-remove \
  --lifecycle install-remove

rpt sandbox-install --manifest ./locked-manifest.json \
  --artifacts ./rpt-artifacts --cache ./rpt-lists \
  --journal-dir ./rpt-journal --id smoke-purge \
  --lifecycle install-purge
```

## Limitations

The image must be available locally; the runner uses Docker --pull=never.
The image's installed package set is not the host's installed package set.
Maintainer scripts or dependencies may require facilities denied by the sandbox
and fail even when installation on a real host would work. APT parity, removal,
autoremove, conffile migration, reliable rollback and durable host execution
are **not implemented**. In particular, interrupted Docker clients do not prove
that the Docker daemon stopped the container; inspect residual rpt-sbx-* Docker
containers and the journal before recovery. A SIGKILL of RPT can bypass cleanup.

Running Docker carries daemon-level privileges. Use only a disposable runner
and trusted OCI images. Docker isolation must not be assumed to withstand
malicious kernel-exploit payloads. Never use this command on a privileged
production Docker daemon.

This command **does execute actual dpkg** but only inside its disposable
container. It never commits package changes to the host.

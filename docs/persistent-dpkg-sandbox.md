# Persistent Docker dpkg test sessions

RPT supports a persistent **container** package database for integration testing.
Multiple RPT commands can now observe package installation, removal and purge
in the same disposable Debian/Ubuntu root. The host root and host dpkg database
are never bind-mounted into that container.

## Usage

Preload an image that contains dpkg:

```sh
docker pull debian:bookworm-slim
rpt sandbox-session-create --session dev01 --image debian:bookworm-slim
```

Reuse the previously generated signed exact-version manifest and downloaded
artifacts from prepare-download-plan and fetch:

```sh
rpt sandbox-session-install \
  --session dev01 --id tx-install-1 --manifest ./locked-manifest.json \
  --artifacts ./rpt-artifacts --cache ./rpt-lists \
  --journal-dir ./rpt-journal --architecture amd64

rpt sandbox-session-inspect --session dev01

rpt sandbox-session-remove \
  --session dev01 --id tx-remove-2 --journal-dir ./rpt-journal \
  package-name

rpt sandbox-session-remove \
  --session dev01 --id tx-purge-3 --journal-dir ./rpt-journal \
  --purge package-name

rpt sandbox-session-destroy --session dev01
```

## Implementation boundaries

- Session names are restricted and prefixed by rpt-session-. RPT inspects the
  Docker label before attaching to or destroying a container.
- Container has no network, dropped capabilities, no-new-privileges and resource
  limits. No host paths or Docker socket are bind-mounted; only docker cp copies
  **already authenticated and SHA256-verified** artifacts into the guest.
- Each install rechecks package control Package/Version/Architecture fields
  against the signed plan before invoking dpkg. Dpkg itself processes
  configuration and triggers in the guest.
- Remove/purge delegates dependency safety decisions to guest dpkg. RPT does
  not yet integrate a full APT removal resolver with this operation.
- RPT serializes individual mutating commands with its cross-process journal.
  Failures attempt to stop the guest, preserving its filesystem for inspection.
  Unexpected SIGKILL may still leave daemon-owned processes running.
- A persistent session is **not a safe replacement** for native package
  transactions. Downgrades, auto/manual marks, conffile strategy, resolver
  completeness, filesystem snapshot/rollback and robust production recovery
  remain incomplete.
- Docker daemon privileges and kernel escape risk remain: use only an
  expendable, private development Docker host, never a production daemon.
- All session data is deleted when sandbox-session-destroy removes its Docker
  container; no host package changes are ever made.

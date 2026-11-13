# Bound install and upgrade in persistent Docker sandboxes

This milestone links candidate policy, signed artifacts, dependency ordering,
and real dpkg install/upgrade execution in a persistent isolated Docker root.
No host dpkg is invoked.

## Workflow

    rpt sandbox-session-create --session dev01 --image debian:bookworm-slim
    rpt sandbox-session-prepare-transaction --session dev01 \
      --cache ./rpt-lists --apt-root / --architecture amd64 \
      --output ./bound.json curl git
    rpt fetch --manifest ./bound.json --output ./rpt-artifacts
    rpt sandbox-session-apply-transaction --session dev01 --id upgrade-001 \
      --transaction ./bound.json --artifacts ./rpt-artifacts \
      --cache ./rpt-lists --apt-root / --journal-dir ./rpt-journal

The bound JSON is directly consumable by the existing RPT fetch command.
It records the sandbox session, exact dpkg status SHA256, selected dependency
versions, prior installed versions and GPG/Release/SHA256-proven archives.

Before installing, RPT rechecks the status digest, re-evaluates candidate
policy and artifact identities, verifies downloaded bytes, stages private
copies and checks the status digest again inside the Docker guest before dpkg.
The guest verifies embedded deb control fields, then executes unpack,
configuration and triggers. Every changed package must match its exact
expected version and architecture, and dpkg audit must be clean, before the
journal can record Succeeded.

CI exercises a real signed upgrade from demo 1.0 to 2.0 with a new configured
Pre-Depends helper, plus stale and replayed plan refusal cases.

## Explicit limits

Sandbox only: no host dpkg, host root or Docker socket binds. Not full APT
policy or Multi-Arch parity. State hash binding is not an atomic lock against
arbitrary concurrent direct Docker commands. The shared RPT journal locks
cooperating transactions using the same journal directory. On failure the
container is stopped; full filesystem rollback is not guaranteed. These
facilities are development-only, not authorization for production execution.

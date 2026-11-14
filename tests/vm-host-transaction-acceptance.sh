#!/usr/bin/env bash
# Disposable VM host, real dpkg install/upgrade/crash/recovery only inside Docker.
set -euo pipefail
guest="${RPT_SANDBOX_IMAGE:-debian:bookworm-slim}"
test "$(id -u)" -ne 0 || { echo "run as unprivileged VM user" >&2; exit 1; }
test -f /var/lib/dpkg/status
command -v docker >/dev/null
docker info >/dev/null
command -v dpkg-query >/dev/null
original_status="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)"
original_extended="missing"
if [[ -f /var/lib/apt/extended_states ]]; then
  original_extended="$(sha256sum /var/lib/apt/extended_states | cut -d' ' -f1)"
fi
initial_holds="$(apt-mark showhold | LC_ALL=C sort)"
initial_audit="$(dpkg --audit)"
verify_host_unchanged() {
  local status extended holds audit
  status="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)"
  extended="missing"
  if [[ -f /var/lib/apt/extended_states ]]; then
    extended="$(sha256sum /var/lib/apt/extended_states | cut -d' ' -f1)"
  fi
  holds="$(apt-mark showhold | LC_ALL=C sort)"
  audit="$(dpkg --audit)"
  if [[ "$status" != "$original_status" ||
        "$extended" != "$original_extended" ||
        "$holds" != "$initial_holds" || "$audit" != "$initial_audit" ]]; then
    echo 'VM host dpkg/apt state changed during isolated transaction acceptance' >&2
    return 1
  fi
}
trap verify_host_unchanged EXIT
echo "Checking sandbox transactions on VM-hosted $guest guest"
RPT_SANDBOX_IMAGE="$guest" bash tests/signed-plan-differential.sh
verify_host_unchanged
echo "PASS VM-hosted $guest signed transaction/recovery, host package state unchanged"

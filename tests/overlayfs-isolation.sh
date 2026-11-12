#!/usr/bin/env bash
# Privileged, opt-in integration test. Run only on a disposable Linux VM.
set -Eeuo pipefail
if [[ "${EUID}" -ne 0 ]]; then
  echo "Run as root on a disposable Linux VM (requires CAP_SYS_ADMIN)" >&2
  exit 2
fi
probe="${1:-./target/debug/rpt-overlay-probe}"
probe="$(realpath "$probe")"
tmp="$(mktemp -d /var/tmp/rpt-overlay-it.XXXXXXXX)"
cleanup() {
  # Never recursively delete a mounted tree.
  if mountpoint -q "$tmp/stage/probe/merged"; then
    echo "ERROR: staging mount escaped the private namespace: $tmp" >&2
    return 1
  fi
  rm -rf -- "$tmp"
}
trap cleanup EXIT
mkdir -p "$tmp/lower" "$tmp/stage"
printf 'original\n' > "$tmp/lower/modified"
printf 'remove-me\n' > "$tmp/lower/deleted"
printf 'untouched\n' > "$tmp/lower/untouched"
RPT_OVERLAY_INTEGRATION_FIXTURE=1 "$probe" probe "$tmp/lower" "$tmp/stage"
test "$(cat "$tmp/lower/modified")" = original
test -f "$tmp/lower/deleted"
test "$(cat "$tmp/lower/untouched")" = untouched
test ! -e "$tmp/lower/created"
test ! -e "$tmp/lower/new-directory"
! mountpoint -q "$tmp/stage/probe/merged"
echo 'PASS: copy-up, whiteout, new files, lower unchanged, mount namespace cleanup'

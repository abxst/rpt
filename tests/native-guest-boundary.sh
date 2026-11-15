#!/usr/bin/env bash
# M14: guest package-state aliasing must never pass an RPT native-rootfs preflight.
# Run only in the signed disposable VM fixture after M7's clean restored guest.
set -euo pipefail
work="${1:?signed fixture workspace}"
arch="${2:?native guest architecture}"
rpt="${3:-./target/debug/rpt}"
[[ "$work" == /tmp/rpt-signed.* && -d "$work" && ! -L "$work" ]]
test "$(id -u)" -ne 0
test "$arch" = "$(dpkg --print-architecture)"
baseline="$work/native-combined-restored"
test -d "$baseline"
guest="$work/native-boundary-guest"
journal="$work/native-boundary-journal"
outside="$work/native-boundary-external-status"
sudo cp -a -- "$baseline" "$guest"
sudo cp -- "$guest/var/lib/dpkg/status" "$outside"
host_sha="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)"
host_marks=missing
[[ ! -f /var/lib/apt/extended_states ]] ||
  host_marks="$(sha256sum /var/lib/apt/extended_states | cut -d' ' -f1)"
baseline_sha="$(sudo sha256sum "$guest/var/lib/dpkg/status" | cut -d' ' -f1)"
external_sha="$(sudo sha256sum "$outside" | cut -d' ' -f1)"
plan="$work/native-boundary-plan.json"
check_refusal() {
  local label="$1"
  if "$rpt" native-rootfs-prepare-transaction \
      --rootfs "$guest" --cache "$work/rpt-cache-v2" \
      --apt-root "$work/rpt-apt" --architecture "$arch" \
      --output "$plan" rpt-sandbox-demo \
      >"$work/native-boundary-$label.log" 2>&1; then
    echo "M14 accepted unsafe guest package state ($label)" >&2
    exit 1
  fi
  test ! -e "$plan"
  if sudo "$rpt" native-rootfs-snapshot-create \
      --rootfs "$guest" --journal-dir "$journal" --id "$label" \
      --snapshot-rootfs "$work/native-boundary-$label-copy" \
      >"$work/native-boundary-$label-snapshot.log" 2>&1; then
    echo "M14 checkpointed unsafe guest package state ($label)" >&2
    exit 1
  fi
  test ! -e "$work/native-boundary-$label-copy"
  test "$(sudo sha256sum "$outside" | cut -d' ' -f1)" = "$external_sha"
  test "$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)" = "$host_sha"
  local marks=missing
  [[ ! -f /var/lib/apt/extended_states ]] ||
    marks="$(sha256sum /var/lib/apt/extended_states | cut -d' ' -f1)"
  test "$marks" = "$host_marks"
  echo "PASS M14 $label host-aliased guest rejected without host or snapshot mutation"
}
status="$guest/var/lib/dpkg/status"
sudo mv "$status" "$status.m14-original"
# Hardlink to a host-visible file outside the guest; this could otherwise
# mutate shared inode contents via dpkg --root.
sudo ln "$outside" "$status"
check_refusal external-hardlink-status
sudo rm -- "$status"
# A symlink through guest dpkg status to a host-visible path is rejected.
sudo ln -s "$outside" "$status"
check_refusal external-symlink-status
sudo rm -- "$status"
sudo mv "$status.m14-original" "$status"

# A package database alias nested in /var/lib/dpkg/info is not safe even
# if its top-level status file has not been altered.
sudo ln -s "$work" "$guest/var/lib/dpkg/info/rpt-m14-host-alias"
check_refusal nested-dpkg-symlink
sudo rm -- "$guest/var/lib/dpkg/info/rpt-m14-host-alias"

# APT extended state can be hardlinked out of the guest independently of
# dpkg status. Save and restore the guest state before moving on.
marks="$guest/var/lib/apt/extended_states"
sudo mkdir -p "$guest/var/lib/apt"
had_marks=false
if sudo test -e "$marks"; then
  had_marks=true
  sudo mv "$marks" "$marks.m14-original"
fi
sudo ln "$outside" "$marks"
check_refusal external-hardlink-marks
sudo rm -- "$marks"
if [[ "$had_marks" == true ]]; then
  sudo mv "$marks.m14-original" "$marks"
fi

# Absolute guest links are intentionally not interpreted as host paths.
# Valid usrmerged relative /bin -> usr/bin and sh -> dash remain allowed.
sudo mv "$guest/bin/sh" "$guest/bin/sh.m14-original"
sudo ln -s /bin/sh "$guest/bin/sh"
check_refusal absolute-host-shell
sudo rm -- "$guest/bin/sh"
sudo mv "$guest/bin/sh.m14-original" "$guest/bin/sh"

test "$(sudo sha256sum "$guest/var/lib/dpkg/status" | cut -d' ' -f1)" = "$baseline_sha"
test "$(sudo sha256sum "$outside" | cut -d' ' -f1)" = "$external_sha"
sudo "$rpt" native-rootfs-recovery-inspect --rootfs "$guest" \
  --journal-dir "$work/native-combined-journal" \
  >"$work/native-boundary-final-recovery.json"
python3 - "$work/native-boundary-final-recovery.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["dpkg_audit"] == "", r
assert r["needs_operator_review"] is False, r
PY
echo 'PASS M14 guest boundary negative acceptance and unchanged host dpkg/APT'

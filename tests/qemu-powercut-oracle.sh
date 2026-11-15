#!/usr/bin/env bash
# M15: actual QEMU VM kill -9 during a dpkg postinst. No host packages changed
# after test prerequisites are installed. NOT a real power-fail of host storage,
# not RPT guest transaction execution and NOT production rollback certification.
set -euo pipefail
umask 077
if [[ "$(id -u)" == 0 ]]; then
  echo 'M15 must run as an unprivileged account on a disposable CI VM' >&2
  exit 2
fi
for cmd in qemu-system-x86_64 qemu-img cloud-localds curl gpgv ssh scp ssh-keygen dpkg-deb python3 timeout; do
  command -v "$cmd" >/dev/null || { echo "missing M15 dependency: $cmd" >&2; exit 2; }
done
rpt_guest_bin="${RPT_QEMU_BIN:?M31 requires RPT_QEMU_BIN to point to the current native rpt binary}"
[[ -f "$rpt_guest_bin" && -x "$rpt_guest_bin" && ! -L "$rpt_guest_bin" ]] || {
  echo "M31 guest audit binary is missing, non-executable or symlinked" >&2; exit 2;
}
rpt_guest_dry_run="${rpt_guest_bin%/*}/rpt-transaction-dry-run"
rpt_guest_inspect="${rpt_guest_bin%/*}/rpt-journal-inspect"
for test_binary in "$rpt_guest_dry_run" "$rpt_guest_inspect"; do
  [[ -f "$test_binary" && -x "$test_binary" && ! -L "$test_binary" ]] || {
    echo "M33 RPT dry-run journal binary missing: $test_binary" >&2; exit 2;
  }
done
test -f /usr/share/keyrings/ubuntu-cloudimage-keyring.gpg || {
  echo 'Missing trusted Canonical cloud image signing keyring' >&2; exit 2;
}
work="$(mktemp -d /tmp/rpt-m15-powercut.XXXXXXXX)"
evidence="$work/evidence"
mkdir -p "$evidence"
qemu_pid=''
current_disk=''
ssh_port=2222
accel=tcg
cpu=max
if [[ -r /dev/kvm && -w /dev/kvm ]]; then
  accel=kvm
  cpu=host
fi
echo "M15 QEMU accelerator: $accel (no simulation or skipped test)"
host_status_before="$(sha256sum /var/lib/dpkg/status | awk '{print $1}')"
host_marks_before=missing
if [[ -f /var/lib/apt/extended_states ]]; then
  host_marks_before="$(sha256sum /var/lib/apt/extended_states | awk '{print $1}')"
fi
host_holds_before="$(apt-mark showhold | LC_ALL=C sort)"
host_audit_before="$(dpkg --audit)"
host_unchanged() {
  [[ "$(sha256sum /var/lib/dpkg/status | awk '{print $1}')" == "$host_status_before" ]] &&
  [[ "$(apt-mark showhold | LC_ALL=C sort)" == "$host_holds_before" ]] &&
  [[ "$(dpkg --audit)" == "$host_audit_before" ]] || return 1
  local current_marks=missing
  [[ ! -f /var/lib/apt/extended_states ]] ||
    current_marks="$(sha256sum /var/lib/apt/extended_states | awk '{print $1}')"
  [[ "$current_marks" == "$host_marks_before" ]]
}
guard_pid() {
  [[ "$qemu_pid" =~ ^[1-9][0-9]*$ ]] || return 1
  [[ -r "/proc/$qemu_pid/cmdline" ]] || return 1
  local commandline
  commandline="$(tr '\0' ' ' <"/proc/$qemu_pid/cmdline")"
  [[ "$commandline" == *qemu-system-x86_64* && "$commandline" == *"$current_disk"* ]]
}
terminate_vm() {
  if [[ -n "$qemu_pid" ]] && guard_pid; then
    kill -KILL "$qemu_pid" || :
  fi
  qemu_pid=''
}
cleanup() {
  local rc=$?
  trap - EXIT
  terminate_vm
  mkdir -p "$RUNNER_TEMP/rpt-m15-evidence"
  cp -a "$evidence/." "$RUNNER_TEMP/rpt-m15-evidence/" || :
  if ! host_unchanged; then
    echo 'M15 modified runner dpkg/APT state; abort' >&2
    rc=1
  fi
  rm -rf -- "$work"
  exit "$rc"
}
trap cleanup EXIT
: "${RUNNER_TEMP:?M15 needs GitHub runner temp directory}"
ssh-keygen -q -t ed25519 -N '' -f "$work/sshkey"
cat > "$work/user-data" <<EOF
#cloud-config
users:
  - name: ubuntu
    groups: [adm, sudo]
    shell: /bin/bash
    sudo: ALL=(ALL) NOPASSWD:ALL
    lock_passwd: true
    ssh_authorized_keys:
      - $(cat "$work/sshkey.pub")
ssh_pwauth: false
disable_root: true
EOF
printf 'instance-id: rpt-m15-v1\nlocal-hostname: rpt-m15\n' >"$work/meta-data"
cloud-localds "$work/seed.iso" "$work/user-data" "$work/meta-data"
ssh_guest() {
  ssh -q -i "$work/sshkey" -p "$ssh_port" -o BatchMode=yes \
    -o ConnectTimeout=4 -o StrictHostKeyChecking=accept-new \
    -o UserKnownHostsFile="$work/known_hosts" ubuntu@127.0.0.1 "$@"
}
scp_guest() {
  scp -q -i "$work/sshkey" -P "$ssh_port" -o BatchMode=yes \
    -o ConnectTimeout=6 -o StrictHostKeyChecking=accept-new \
    -o UserKnownHostsFile="$work/known_hosts" "$1" ubuntu@127.0.0.1:"$2"
}
start_vm() {
  local disk="$1" phase="$2"
  [[ -z "$qemu_pid" ]] || { echo 'VM already active' >&2; exit 1; }
  [[ -f "$disk" ]] || { echo 'VM disk missing' >&2; exit 1; }
  current_disk="$disk"
  rm -f "$work/qemu.pid"
  qemu-system-x86_64 -daemonize -pidfile "$work/qemu.pid" \
    -machine "type=q35,accel=$accel" -cpu "$cpu" -smp 2 -m 2048 \
    -display none -monitor none -serial "file:$evidence/serial-$phase.log" \
    -drive "if=virtio,format=qcow2,file=$disk,cache=writeback" \
    -drive "if=virtio,format=raw,media=cdrom,readonly=on,file=$work/seed.iso" \
    -netdev "user,id=net0,restrict=on,hostfwd=tcp:127.0.0.1:$ssh_port-:22" \
    -device virtio-net-pci,netdev=net0
  qemu_pid="$(cat "$work/qemu.pid")"
  guard_pid || { echo 'Unable to prove QEMU PID belongs to test disk' >&2; exit 1; }
  local ready=false
  for attempt in $(seq 1 180); do
    if ssh_guest 'true' >/dev/null 2>&1; then
      ready=true
      break
    fi
    guard_pid || { echo "QEMU exited while booting $phase" >&2; exit 1; }
    sleep 2
  done
  [[ "$ready" == true ]] || { echo "M15 guest SSH not ready: $phase" >&2; exit 1; }
  ssh_guest 'sudo cloud-init status --wait >/dev/null && sudo test -f /var/lib/dpkg/status'
  echo "PASS M15 boot $phase"
}
poweroff_vm() {
  guard_pid || { echo 'Cannot prove QEMU identity before shutdown' >&2; exit 1; }
  ssh_guest 'sudo systemctl poweroff' >/dev/null 2>&1 || :
  for attempt in $(seq 1 80); do
    if ! kill -0 "$qemu_pid" 2>/dev/null; then
      qemu_pid=''
      return 0
    fi
    sleep 1
  done
  echo 'M15 guest did not shut down cleanly; cannot checkpoint baseline' >&2
  exit 1
}
powercut_vm() {
  guard_pid || { echo 'Cannot prove QEMU identity before SIGKILL' >&2; exit 1; }
  echo "M15 SIGKILL of dedicated QEMU PID $qemu_pid during guest postinst"
  kill -KILL "$qemu_pid"
  for attempt in $(seq 1 30); do
    if ! kill -0 "$qemu_pid" 2>/dev/null; then
      qemu_pid=''
      return 0
    fi
    sleep 1
  done
  echo 'QEMU did not terminate after hard power cut' >&2
  exit 1
}
create_package() {
  local version="$1" mode="$2"
  local root="$work/pkg-$version"
  mkdir -p "$root/DEBIAN" "$root/usr/share/rpt-powercut"
  # The CI harness uses umask 077 for SSH/private evidence, but Debian
  # control directories MUST be world-traversable (>=0755) for dpkg-deb.
  chmod 0755 "$root" "$root/DEBIAN" "$root/usr" "$root/usr/share" \
    "$root/usr/share/rpt-powercut"
  cat > "$root/DEBIAN/control" <<EOF
Package: rpt-powercut
Version: $version
Architecture: all
Maintainer: RPT VM Oracle <rpt-ci@example.invalid>
Description: Disposable QEMU guest power-cut fixture
EOF
  printf '%s\n' "$version" > "$root/usr/share/rpt-powercut/version"
  chmod 0644 "$root/DEBIAN/control" "$root/usr/share/rpt-powercut/version"
  if [[ "$mode" == hung ]]; then
    cat > "$root/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -eu
if [ "$1" = configure ]; then
  mkdir -p /var/lib/rpt-powercut
  printf 'entered-postinst\n' > /var/lib/rpt-powercut/entered
  sync -f /var/lib/rpt-powercut/entered
  # An ephemeral /run readiness marker is written only after fsync, so the
  # hypervisor is killed at a deterministic, fully entered postinst point.
  printf 'postinst-flushed\\n' > /run/rpt-powercut-ready
  sleep 900
fi
POSTINST
  else
    cat > "$root/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -eu
exit 0
POSTINST
  fi
  chmod 0755 "$root/DEBIAN/postinst"
  dpkg-deb --build --root-owner-group "$root" "$work/rpt-powercut-$version.deb" >/dev/null
}
echo 'M15 fetching independently signed Canonical cloud image manifest'
image=noble-server-cloudimg-amd64.img
base_url=https://cloud-images.ubuntu.com/noble/current
for name in SHA256SUMS SHA256SUMS.gpg "$image"; do
  curl --fail --location --silent --show-error --retry 3 \
    --connect-timeout 15 --max-time 300 "$base_url/$name" -o "$work/$name"
done
gpgv --keyring /usr/share/keyrings/ubuntu-cloudimage-keyring.gpg \
  "$work/SHA256SUMS.gpg" "$work/SHA256SUMS" >"$evidence/cloud-signature.txt" 2>&1
# Match the exact image filename in either GNU text (two spaces) or
# binary (space+asterisk) digest notation; a prefix match is not sufficient.
grep -E "^[0-9a-f]{64} [ *]$image$" "$work/SHA256SUMS" >"$work/image-only.sha256"
test "$(wc -l <"$work/image-only.sha256")" -eq 1
(cd "$work" && sha256sum -c image-only.sha256)
image_sha="$(sha256sum "$work/$image" | awk '{print $1}')"
qemu-img info "$work/$image" >/dev/null
create_package 1.0 clean
create_package 2.0 hung
cp --reflink=auto "$work/$image" "$work/initial.qcow2"
qemu-img resize "$work/initial.qcow2" 8G
start_vm "$work/initial.qcow2" baseline
# Cloud images may mount /tmp as tmpfs; an installer copied there can vanish
# after the first clean shutdown, before the second VM boot. Keep our
# archives on the persistent guest root filesystem instead.
scp_guest "$work/rpt-powercut-1.0.deb" /home/ubuntu/rpt-powercut-1.0.deb
scp_guest "$work/rpt-powercut-2.0.deb" /home/ubuntu/rpt-powercut-2.0.deb
# Carry the exact PR-head RPT binary into the independent QEMU disk before
# sealing the baseline. Neither apt nor dpkg on the CI runner is replaced.
scp_guest "$rpt_guest_bin" /home/ubuntu/rpt-audit
scp_guest "$rpt_guest_dry_run" /home/ubuntu/rpt-dry-run
scp_guest "$rpt_guest_inspect" /home/ubuntu/rpt-journal-inspect
ssh_guest 'chmod 0755 /home/ubuntu/rpt-audit /home/ubuntu/rpt-dry-run /home/ubuntu/rpt-journal-inspect && /home/ubuntu/rpt-audit --version'
ssh_guest 'sudo dpkg --install /home/ubuntu/rpt-powercut-1.0.deb && test -z "$(sudo dpkg --audit)"'
ssh_guest 'sudo dpkg-query -s rpt-powercut' >"$evidence/baseline-dpkg.txt"
grep -Fxq 'Status: install ok installed' "$evidence/baseline-dpkg.txt"
grep -Fxq 'Version: 1.0' "$evidence/baseline-dpkg.txt"
poweroff_vm
cp --reflink=auto "$work/initial.qcow2" "$work/baseline.qcow2"
baseline_sha="$(sha256sum "$work/baseline.qcow2" | awk '{print $1}')"
qemu-img create -f qcow2 -F qcow2 -b "$work/baseline.qcow2" "$work/crashed.qcow2" >/dev/null
start_vm "$work/crashed.qcow2" crash
ssh_guest 'test -s /home/ubuntu/rpt-powercut-2.0.deb && sudo dpkg-deb -f /home/ubuntu/rpt-powercut-2.0.deb Version | grep -Fxq 2.0'
# M33: persist an RPT OWNED dry-run Running journal before the same QEMU
# hard cut that interrupts real, independent guest dpkg postinst.
ssh_guest 'sudo systemd-run --unit=rpt-dryrun-powercut --collect /home/ubuntu/rpt-dry-run --journal-dir /var/lib/rpt-powercut-journal --id m33-powercut --pause-after-running-ms 180000' \
  >"$evidence/started-rpt-dry-run.txt"
rpt_running=false
for attempt in $(seq 1 40); do
  if ssh_guest 'sudo grep -Fq running /var/lib/rpt-powercut-journal/m33-powercut.json' >/dev/null 2>&1; then
    rpt_running=true
    break
  fi
  guard_pid || { echo 'QEMU exited before durable RPT Running journal' >&2; exit 1; }
  sleep 1
done
if [[ "$rpt_running" != true ]]; then
  ssh_guest 'sudo journalctl -u rpt-dryrun-powercut.service --no-pager -n 80 || true' >"$evidence/failed-rpt-dry-run-service.txt" 2>&1 || :
  echo 'M33 RPT did not persist Running before QEMU cut; fault not tested' >&2; exit 1
fi
ssh_guest 'sudo systemd-run --unit=rpt-powercut-upgrade --collect /usr/bin/dpkg --install /home/ubuntu/rpt-powercut-2.0.deb' \
  >"$evidence/started-upgrade.txt"
entered=false
for attempt in $(seq 1 35); do
  if ssh_guest 'sudo test -f /run/rpt-powercut-ready' >/dev/null 2>&1; then
    entered=true
    break
  fi
  guard_pid || { echo 'QEMU exited before postinst marker' >&2; exit 1; }
  if ssh_guest 'sudo systemctl is-failed --quiet rpt-powercut-upgrade.service' >/dev/null 2>&1; then
    break
  fi
  sleep 1
done
if [[ "$entered" != true ]]; then
  ssh_guest 'sudo systemctl status rpt-powercut-upgrade.service --no-pager -l || true' \
    >"$evidence/failed-upgrade-service.txt" 2>&1 || :
  ssh_guest 'sudo journalctl -u rpt-powercut-upgrade.service --no-pager -n 100 || true' \
    >"$evidence/failed-upgrade-journal.txt" 2>&1 || :
  echo 'Guest dpkg did not pass postinst fsync; no power cut was tested; inspect service journal artifact' >&2
  exit 1
fi
ssh_guest 'sudo cat /var/lib/rpt-powercut/entered' | grep -Fxq entered-postinst
powercut_vm
# This was a forced process death, not ACPI shutdown or SIGTERM to dpkg.
crashed_sha_before_reboot="$(sha256sum "$work/crashed.qcow2" | awk '{print $1}')"
start_vm "$work/crashed.qcow2" after-kill
ssh_guest 'sudo dpkg-query -s rpt-powercut' >"$evidence/after-kill-dpkg.txt"
ssh_guest 'sudo dpkg --audit' >"$evidence/after-kill-audit.txt"
# A real kernel/power-cut must leave RPT's own *dry-run* journal blocked.
# This is not a dpkg transaction executed by RPT.
if ssh_guest 'sudo /home/ubuntu/rpt-journal-inspect /var/lib/rpt-powercut-journal --json' \
    >"$evidence/after-kill-rpt-journal.json" 2>"$evidence/after-kill-rpt-journal-error.txt"; then
  echo 'M33 inspector accepted a Running RPT journal after hard QEMU SIGKILL' >&2; exit 1
fi
python3 - "$evidence/after-kill-rpt-journal.json" <<'PY'
import json, sys
with open(sys.argv[1]) as stream:
    records = json.load(stream)
assert any(x["id"] == "m33-powercut" and x["phase"] == "running"
           for x in records), records
PY
test -s "$evidence/after-kill-audit.txt" || {
  echo 'Power-cut guest unexpectedly has clean dpkg; fault not proven' >&2; exit 1;
}
# The actual RPT executable must independently recognize the same *broken*
# guest status. It is an audit, NOT a claim that RPT ran the transaction.
ssh_guest 'sudo /home/ubuntu/rpt-audit dpkg-audit --status /var/lib/dpkg/status' \
    >"$evidence/after-kill-rpt-dpkg-health.json"
if ssh_guest 'sudo /home/ubuntu/rpt-audit dpkg-audit --status /var/lib/dpkg/status --require-clean' \
    >/dev/null 2>&1; then
  echo 'M31 RPT accepted an interrupted dpkg guest after actual QEMU SIGKILL' >&2; exit 1
fi
python3 - "$evidence/after-kill-rpt-dpkg-health.json" <<'PY'
import json, sys
with open(sys.argv[1]) as stream:
    report = json.load(stream)
assert report["healthy"] is False and report["issues"], report
assert report["read_only"] is True
assert report["host_execution_allowed"] is False
assert len(report["status_sha256"]) == 64
PY
if grep -Fxq 'Status: install ok installed' "$evidence/after-kill-dpkg.txt" &&
    grep -Fxq 'Version: 2.0' "$evidence/after-kill-dpkg.txt"; then
  echo 'Power-cut guest appears fully upgraded; fault was not observed' >&2; exit 1
fi
ssh_guest 'sudo test -f /var/lib/rpt-powercut/entered'
poweroff_vm
crashed_sha="$(sha256sum "$work/crashed.qcow2" | awk '{print $1}')"
test "$crashed_sha" != "$crashed_sha_before_reboot"
test "$(sha256sum "$work/baseline.qcow2" | awk '{print $1}')" = "$baseline_sha"
# Recovery is into a NEW overlay backed by the independently preserved v1
# virtual disk; never mutate/repair the failed overlay or original baseline.
qemu-img create -f qcow2 -F qcow2 -b "$work/baseline.qcow2" "$work/recovered.qcow2" >/dev/null
start_vm "$work/recovered.qcow2" recovered
ssh_guest 'sudo dpkg-query -s rpt-powercut' >"$evidence/recovered-dpkg.txt"
ssh_guest 'sudo dpkg --audit' >"$evidence/recovered-audit.txt"
grep -Fxq 'Status: install ok installed' "$evidence/recovered-dpkg.txt"
grep -Fxq 'Version: 1.0' "$evidence/recovered-dpkg.txt"
test ! -s "$evidence/recovered-audit.txt"
# A different fresh disk restored from the pre-fault baseline contains no
# RPT dry-run journal. No unsafe in-place recovery of the failed journal.
ssh_guest 'sudo test ! -e /var/lib/rpt-powercut-journal'
# The identical RPT executable must now accept the independently restored
# v1 baseline and still explicitly deny host package execution.
ssh_guest 'sudo /home/ubuntu/rpt-audit dpkg-audit --status /var/lib/dpkg/status --require-clean' \
    >"$evidence/recovered-rpt-dpkg-health.json"
python3 - "$evidence/recovered-rpt-dpkg-health.json" <<'PY'
import json, sys
with open(sys.argv[1]) as stream:
    report = json.load(stream)
assert report["healthy"] is True and report["issues"] == [], report
assert report["read_only"] is True
assert report["host_execution_allowed"] is False
assert len(report["status_sha256"]) == 64
PY
ssh_guest 'sudo test ! -e /var/lib/rpt-powercut/entered'
poweroff_vm
test "$(sha256sum "$work/crashed.qcow2" | awk '{print $1}')" = "$crashed_sha"
test "$(sha256sum "$work/baseline.qcow2" | awk '{print $1}')" = "$baseline_sha"
host_unchanged
python3 - "$evidence/result.json" "$image_sha" "$baseline_sha" "$crashed_sha" "$accel" <<'PY'
import json, sys
from pathlib import Path
destination, image_sha, baseline_sha, failed_sha, acceleration = sys.argv[1:]
result = {
    "schema_version": 1,
    "case": "qemu-sigkill-dpkg-postinst",
    "guest": "ubuntu-24.04-amd64",
    "vm_acceleration": acceleration,
    "image_sha256": image_sha,
    "baseline_sha256": baseline_sha,
    "failed_disk_sha256": failed_sha,
    "cut_signal": "SIGKILL-QEMU",
    "failed_dpkg_audit_nonempty": True,
    "failed_full_upgrade": False,
    "restore_to_fresh_disk": True,
    "restored_version": "1.0",
    "restored_dpkg_audit_empty": True,
    "baseline_disk_unchanged": True,
    "failed_disk_unchanged_by_restore": True,
    "host_dpkg_unchanged": True,
    "rpt_guest_transaction_executed": False,
    "production_ready": False
}
Path(destination).write_text(json.dumps(result, indent=2, sort_keys=True) + "\n")
PY
python3 scripts/validate-powercut-evidence.py "$evidence/result.json"
echo 'PASS M15/M31/M33 real QEMU SIGKILL: blocked RPT dry-run Running journal, broken dpkg audit, independent clean disk restore, runner unchanged'

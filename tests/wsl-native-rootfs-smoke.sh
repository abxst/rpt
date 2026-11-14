#!/usr/bin/env bash
# WSL2-only test: signed native dpkg --root in a disposable Ubuntu filesystem.
# No mutation of the live WSL dpkg database; privileged chroot is not a sandbox.
set -euo pipefail
if [[ "$(id -u)" == 0 ]]; then
  echo "Run as the normal WSL user; the script uses sudo only for the guest rootfs." >&2
  exit 2
fi
if ! grep -qi microsoft /proc/sys/kernel/osrelease; then
  echo "This acceptance script is restricted to WSL2." >&2
  exit 2
fi
source /etc/os-release
if [[ "${ID:-}" != ubuntu || -z "${VERSION_CODENAME:-}" ]]; then
  echo "An Ubuntu WSL distro with VERSION_CODENAME is required." >&2
  exit 2
fi
if [[ "$(dpkg --print-architecture)" != amd64 ]]; then
  echo "WSL smoke fixture requires Ubuntu amd64." >&2
  exit 2
fi
for binary in cargo debootstrap gpgv dpkg-query dpkg dpkg-deb sha256sum python3; do
  command -v "$binary" >/dev/null || {
    echo "Missing prerequisite $binary." >&2
    exit 2
  }
done
keyring=/usr/share/keyrings/ubuntu-archive-keyring.gpg
test -f "$keyring"
sudo -n true || sudo true

work="$(mktemp -d /tmp/rpt-native-wsl.XXXXXXXX)"
guest="$work/guest"
journal="$work/journal"
echo "Disposable test workspace: $work"
echo "Test data is intentionally retained for manual inspection."
mkdir -p "$work/apt"
printf 'deb [signed-by=%s] https://archive.ubuntu.com/ubuntu %s main\n' \
  "$keyring" "$VERSION_CODENAME" > "$work/apt/sources.list"
host_before="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)"
sudo debootstrap --variant=minbase --arch=amd64 \
  "$VERSION_CODENAME" "$guest" https://archive.ubuntu.com/ubuntu
printf 'rpt-disposable-rootfs-v1\n' \
  | sudo tee "$guest/.rpt-disposable-rootfs" >/dev/null

cargo generate-lockfile
cargo build --locked --bin rpt
rpt=./target/debug/rpt
"$rpt" update --root "$work/apt" --output "$work/lists" --no-progress
"$rpt" native-rootfs-prepare-transaction \
  --rootfs "$guest" --cache "$work/lists" --apt-root "$work/apt" \
  --architecture amd64 --output "$work/install.json" hello
python3 - "$work/install.json" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['packages'], 'No verified hello archives to install'
assert not x['executable']
PY
"$rpt" fetch --manifest "$work/install.json" --output "$work/artifacts" \
  --workers 2 --segments 1
sudo "$rpt" native-rootfs-apply-transaction \
  --rootfs "$guest" --id wsl-install --transaction "$work/install.json" \
  --artifacts "$work/artifacts" --cache "$work/lists" --apt-root "$work/apt" \
  --journal-dir "$journal" --timeout-seconds 300
sudo dpkg-query --admindir="$guest/var/lib/dpkg" -W \
  '-f=${Status}|${Version}\n' hello
sudo "$rpt" native-rootfs-remove-planned \
  --rootfs "$guest" --id wsl-remove --journal-dir "$journal" \
  --architecture amd64 hello
test -z "$(sudo dpkg --root="$guest" --audit)"
test "$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)" = "$host_before"
grep -Fq '"phase": "succeeded"' "$journal/wsl-install.json"
grep -Fq '"phase": "succeeded"' "$journal/wsl-remove.json"
echo "PASS signed RPT install/remove on disposable Ubuntu WSL rootfs; live dpkg unchanged"
echo "Inspect test artifacts: $work"

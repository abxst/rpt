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
# Authenticate once without a noisy, expected sudo -n failure in fresh WSL.
sudo -v

work="$(mktemp -d /tmp/rpt-native-wsl.XXXXXXXX)"
guest="$work/guest"
journal="$work/journal"
echo "Disposable test workspace: $work"
echo "Test data is intentionally retained for manual inspection."
# debootstrap's test-dev-null probe can print Permission denied when /tmp
# has the nodev mount flag, even if the rootfs is installed successfully.
# Diagnose the mount, never remount the host and never hide real failures.
if command -v findmnt >/dev/null 2>&1; then
  mount_opts="$(findmnt -n -T "$work" -o OPTIONS || true)"
  if [[ ",$mount_opts," == *,nodev,* ]]; then
    echo "NOTE: test root filesystem is on a nodev mount; debootstrap may print a test-dev-null warning." >&2
  fi
fi
mkdir -p "$work/apt"
printf 'deb [signed-by=%s] https://archive.ubuntu.com/ubuntu %s main\n' \
  "$keyring" "$VERSION_CODENAME" > "$work/apt/sources.list"
host_before="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)"
# Even on a failed fixture, verify that WSL's live dpkg status was not
# modified. Never remove the guest rootfs automatically after a failure.
check_live_dpkg_on_exit() {
  local result=$?
  trap - EXIT
  local actual
  actual="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)" || result=1
  if [[ "$actual" != "$host_before" ]]; then
    echo "FAIL: live WSL dpkg status differs from the pre-test snapshot!" >&2
    result=1
  fi
  exit "$result"
}
trap check_live_dpkg_on_exit EXIT
sudo debootstrap --variant=minbase --arch=amd64 \
  "$VERSION_CODENAME" "$guest" https://archive.ubuntu.com/ubuntu
printf 'rpt-disposable-rootfs-v1\n' \
  | sudo tee "$guest/.rpt-disposable-rootfs" >/dev/null
# Refuse an incomplete base system before exposing it to native execution.
sudo test -s "$guest/var/lib/dpkg/status"
sudo test -x "$guest/usr/bin/dpkg"
sudo test -x "$guest/bin/sh"
sudo test -x "$guest/usr/bin/apt-mark"

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

# Re-plan from the guest's changed dpkg status and reinstall signed bytes.
"$rpt" native-rootfs-prepare-transaction \
  --rootfs "$guest" --cache "$work/lists" --apt-root "$work/apt" \
  --architecture amd64 --output "$work/reinstall.json" hello
sudo "$rpt" native-rootfs-apply-transaction \
  --rootfs "$guest" --id wsl-reinstall --transaction "$work/reinstall.json" \
  --artifacts "$work/artifacts" --cache "$work/lists" --apt-root "$work/apt" \
  --journal-dir "$journal" --timeout-seconds 300

# Verify native APT auto/manual transitions with the *guest* apt-mark.
sudo "$rpt" native-rootfs-mark \
  --rootfs "$guest" --id wsl-mark-manual --journal-dir "$journal" manual hello
sudo chroot "$guest" /usr/bin/apt-mark showmanual hello | grep -Fxq hello
sudo "$rpt" native-rootfs-mark \
  --rootfs "$guest" --id wsl-mark-auto --journal-dir "$journal" auto hello
sudo chroot "$guest" /usr/bin/apt-mark showauto hello | grep -Fxq hello
sudo "$rpt" native-rootfs-autoremove-planned \
  --rootfs "$guest" --id wsl-autoremove --journal-dir "$journal" \
  --architecture amd64 --timeout-seconds 300

# Exercise purge after reinstallation, distinct from remove/autoremove.
"$rpt" native-rootfs-prepare-transaction \
  --rootfs "$guest" --cache "$work/lists" --apt-root "$work/apt" \
  --architecture amd64 --output "$work/purge-install.json" hello
sudo "$rpt" native-rootfs-apply-transaction \
  --rootfs "$guest" --id wsl-purge-install --transaction "$work/purge-install.json" \
  --artifacts "$work/artifacts" --cache "$work/lists" --apt-root "$work/apt" \
  --journal-dir "$journal" --timeout-seconds 300
sudo "$rpt" native-rootfs-remove-planned \
  --rootfs "$guest" --id wsl-purge --journal-dir "$journal" \
  --architecture amd64 --purge hello | tee "$work/hello-purge.log"
grep -Fq '"mode": "purge"' "$work/hello-purge.log"

# hello has no conffile: also exercise a *local, known, script-free*
# conffile fixture on the separate guest to prove remove != purge. Signed
# archive validation is already covered by hello and CI signed fixtures.
fixture="$work/conffile-fixture"
mkdir -p "$fixture/DEBIAN" "$fixture/etc/rpt-wsl-fixture"
cat > "$fixture/DEBIAN/control" <<'CONTROL'
Package: rpt-wsl-conffile
Version: 1.0
Architecture: all
Maintainer: RPT Test <rpt-test@example.invalid>
Description: disposable WSL conffile removal regression fixture
CONTROL
printf '/etc/rpt-wsl-fixture/config.conf\n' > "$fixture/DEBIAN/conffiles"
printf 'enabled=true\n' > "$fixture/etc/rpt-wsl-fixture/config.conf"
fixture_deb="$work/rpt-wsl-conffile_1.0_all.deb"
dpkg-deb --build --root-owner-group "$fixture" "$fixture_deb" >/dev/null
sudo dpkg --root="$guest" --install "$fixture_deb"
sudo "$rpt" native-rootfs-remove-planned \
  --rootfs "$guest" --id wsl-conffile-remove --journal-dir "$journal" \
  --architecture amd64 rpt-wsl-conffile
sudo test -f "$guest/etc/rpt-wsl-fixture/config.conf"
test "$(sudo dpkg-query --admindir="$guest/var/lib/dpkg" \
  -W '-f=${Status}' rpt-wsl-conffile)" = 'deinstall ok config-files'
# Plain removal must reject an already removed binary; only --purge may
# select its residual config-files state and execute dpkg --purge.
if sudo "$rpt" native-rootfs-remove-planned \
  --rootfs "$guest" --id wsl-conffile-double-remove --journal-dir "$journal" \
  --architecture amd64 rpt-wsl-conffile >"$work/double-remove.log" 2>&1; then
  echo "FAIL: native remove unexpectedly accepted config-files-only binary" >&2
  exit 1
fi
sudo "$rpt" native-rootfs-remove-planned \
  --rootfs "$guest" --id wsl-conffile-purge --journal-dir "$journal" \
  --architecture amd64 --purge rpt-wsl-conffile | tee "$work/conffile-purge.log"
grep -Fq '"mode": "purge"' "$work/conffile-purge.log"
sudo test ! -e "$guest/etc/rpt-wsl-fixture/config.conf"
purged_fixture="$(sudo dpkg-query --admindir="$guest/var/lib/dpkg" \
  -W '-f=${Status}' rpt-wsl-conffile 2>/dev/null || true)"
test -z "$purged_fixture" || test "$purged_fixture" = 'unknown ok not-installed'

test -z "$(sudo dpkg --root="$guest" --audit)"
status_after="$(sudo dpkg-query --admindir="$guest/var/lib/dpkg" -W '-f=${Status}' hello 2>/dev/null || true)"
test "$status_after" != 'install ok installed'
test "$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)" = "$host_before"
for transaction in wsl-install wsl-remove wsl-reinstall wsl-mark-manual \
  wsl-mark-auto wsl-autoremove wsl-purge-install wsl-purge \
  wsl-conffile-remove wsl-conffile-purge; do
  grep -Fq '"phase": "succeeded"' "$journal/$transaction.json"
done
echo "PASS native WSL signed lifecycle, remove then purge config-files-only and unchanged live dpkg"
echo "Inspect test artifacts: $work"

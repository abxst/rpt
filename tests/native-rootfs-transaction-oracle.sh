#!/usr/bin/env bash
# Independent native dpkg --root oracle on a disposable VM guest filesystem.
# NOT an RPT native-host package executor; no host root or dpkg DB is mounted.
set -euo pipefail
guest_image="${RPT_NATIVE_ROOT_IMAGE:-ubuntu:22.04}"
rpt="${1:-./target/debug/rpt}"
case "$guest_image" in ubuntu:22.04|ubuntu:24.04) ;; *) exit 2;; esac
test "$(id -u)" -ne 0
test "$(dpkg --print-architecture)" = amd64
docker info >/dev/null
sudo -n true
"$rpt" version-compare --driver deb 1.0 2.0 | grep -Fxq -- '-1'
work="$(mktemp -d /tmp/rpt-native-vm-root.XXXXXXXX)"
guest="$work/guest"
container="rpt-native-oracle-$$"
status_before="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)"
extended_before="missing"
[[ ! -f /var/lib/apt/extended_states ]] ||
  extended_before="$(sha256sum /var/lib/apt/extended_states | cut -d' ' -f1)"
holds_before="$(apt-mark showhold | LC_ALL=C sort)"
audit_before="$(dpkg --audit)"
check_host() {
  test "$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)" = "$status_before"
  local extended="missing"
  [[ ! -f /var/lib/apt/extended_states ]] ||
    extended="$(sha256sum /var/lib/apt/extended_states | cut -d' ' -f1)"
  test "$extended" = "$extended_before"
  test "$(apt-mark showhold | LC_ALL=C sort)" = "$holds_before"
  test "$(dpkg --audit)" = "$audit_before"
}
cleanup() {
  local rc=$?
  trap - EXIT
  docker rm -f "$container" >/dev/null 2>&1 || :
  if [[ -d "$work" && ! -L "$work" && "$work" == /tmp/rpt-native-vm-root.* ]]; then
    sudo rm -rf -- "$work"
  else
    rc=1
    echo "Unsafe native-root workspace: $work" >&2
  fi
  if ! check_host; then
    rc=1
    echo 'CI VM host dpkg/APT state changed' >&2
  fi
  exit "$rc"
}
trap cleanup EXIT

docker pull "$guest_image" >/dev/null
docker create --name "$container" --network=none --entrypoint /bin/sh \
  "$guest_image" -c true >/dev/null
sudo mkdir -p "$guest"
docker export "$container" | sudo tar -xpf - -C "$guest"
docker rm "$container" >/dev/null
sudo chroot "$guest" /bin/sh -ec 'test -x /usr/bin/dpkg && test -d /var/lib/dpkg && test -x /bin/sh'
test "$(sudo chroot "$guest" dpkg --print-architecture)" = amd64

artifacts="$work/artifacts"
mkdir -p "$artifacts"
make_control() {
  local folder="$1" name="$2" version="$3"
  mkdir -p "$folder/DEBIAN" "$folder/usr/share/rpt-native-oracle"
  cat >"$folder/DEBIAN/control" <<EOF
Package: $name
Version: $version
Architecture: all
Maintainer: RPT VM CI <rpt-vm@example.invalid>
Description: ephemeral Linux VM dpkg rootfs oracle
EOF
}
make_fixture() {
  local version="$1" behavior="$2" folder="$work/pkg-$1"
  make_control "$folder" rpt-native-fixture "$version"
  mkdir -p "$folder/etc"
  printf 'upstream-default-%s\n' "$version" >"$folder/etc/rpt-native-fixture.conf"
  echo '/etc/rpt-native-fixture.conf' >"$folder/DEBIAN/conffiles"
  printf 'payload-%s\n' "$version" >"$folder/usr/share/rpt-native-oracle/version"
  if [[ "$version" != 1.0 ]]; then
    printf 'Pre-Depends: rpt-native-helper (>= 1.0)\n' >>"$folder/DEBIAN/control"
  fi
  if [[ "$behavior" == success ]]; then
    cat >"$folder/DEBIAN/postinst" <<EOF
#!/bin/sh
set -eu
if [ "\$1" = configure ]; then
  echo 'fixture-configured-$version' >> /var/log/rpt-native-events
fi
EOF
  elif [[ "$behavior" == failure ]]; then
    cat >"$folder/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -eu
echo fixture-failed-postinst >> /var/log/rpt-native-events
exit 42
EOF
  else
    cat >"$folder/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -eu
echo fixture-crash-postinst >> /var/log/rpt-native-events
exec sleep 8
EOF
  fi
  chmod 0755 "$folder/DEBIAN/postinst"
  dpkg-deb --build --root-owner-group "$folder" \
    "$artifacts/rpt-native-fixture_${version}_all.deb" >/dev/null
}
make_control "$work/helper" rpt-native-helper 1.0
cat >"$work/helper/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -eu
if [ "$1" = configure ]; then echo helper-configured >> /var/log/rpt-native-events; fi
EOF
chmod 0755 "$work/helper/DEBIAN/postinst"
dpkg-deb --build --root-owner-group "$work/helper" \
  "$artifacts/rpt-native-helper_1.0_all.deb" >/dev/null
make_fixture 1.0 success
make_fixture 2.0 success
make_fixture 3.0 failure
make_fixture 4.0 crash

version_in_guest() {
  sudo dpkg-query --admindir="$guest/var/lib/dpkg" \
    -W -f='${Status}|${Version}' rpt-native-fixture
}
sudo dpkg --root="$guest" --install "$artifacts/rpt-native-fixture_1.0_all.deb"
test "$(version_in_guest)" = 'install ok installed|1.0'
echo 'admin-local-change' | sudo tee -a "$guest/etc/rpt-native-fixture.conf" >/dev/null
sudo cp -a -- "$guest" "$work/pre-upgrade"
sudo dpkg --root="$guest" --install "$artifacts/rpt-native-helper_1.0_all.deb"
sudo dpkg --root="$guest" --force-confold --install "$artifacts/rpt-native-fixture_2.0_all.deb"
test "$(version_in_guest)" = 'install ok installed|2.0'
grep -Fxq 'admin-local-change' "$guest/etc/rpt-native-fixture.conf"
grep -Fxq 'upstream-default-2.0' "$guest/etc/rpt-native-fixture.conf.dpkg-dist"
test -z "$(sudo dpkg --root="$guest" --audit)"
sudo cat "$guest/var/log/rpt-native-events" >"$work/events"
python3 - "$work/events" <<'PY'
import sys
events=open(sys.argv[1]).read().splitlines()
assert events.index('helper-configured') < events.index('fixture-configured-2.0')
assert 'fixture-configured-1.0' in events
PY
check_host

# Failure in a real guest maintainer script must be observable, then restored.
sudo cp -a -- "$guest" "$work/pre-failure"
if sudo dpkg --root="$guest" --force-confold \
  --install "$artifacts/rpt-native-fixture_3.0_all.deb" >"$work/failure.log" 2>&1; then
  echo 'Expected postinst failure was ignored' >&2; exit 1
fi
test -n "$(sudo dpkg --root="$guest" --audit)"
sudo mv -- "$guest" "$work/quarantine-failure"
sudo cp -a -- "$work/pre-failure" "$guest"
test "$(version_in_guest)" = 'install ok installed|2.0'
test -z "$(sudo dpkg --root="$guest" --audit)"
grep -Fxq 'admin-local-change' "$guest/etc/rpt-native-fixture.conf"
check_host

# SIGKILL while postinst runs; preserve guest evidence and restore snapshot.
sudo cp -a -- "$guest" "$work/pre-crash"
set +e
sudo timeout --signal=KILL --kill-after=1 2 \
  dpkg --root="$guest" --force-confold \
  --install "$artifacts/rpt-native-fixture_4.0_all.deb" >"$work/crash.log" 2>&1
crash_rc=$?
set -e
if [[ $crash_rc == 0 ]]; then echo 'Expected SIGKILL, got success' >&2; exit 1; fi
grep -Fxq 'fixture-crash-postinst' "$guest/var/log/rpt-native-events"
test -n "$(sudo dpkg --root="$guest" --audit)"
# The known test postinst is a bounded sleep; do not restore until it exits.
sleep 8
sudo mv -- "$guest" "$work/quarantine-crash"
sudo cp -a -- "$work/pre-crash" "$guest"
test "$(version_in_guest)" = 'install ok installed|2.0'
test -z "$(sudo dpkg --root="$guest" --audit)"
grep -Fxq 'admin-local-change' "$guest/etc/rpt-native-fixture.conf"
test -f "$work/quarantine-crash/var/log/rpt-native-events"
check_host
echo 'PASS native-root dpkg semantics, maintainer scripts, Pre-Depends, conffiles, failed/SIGKILL restore, host unchanged'

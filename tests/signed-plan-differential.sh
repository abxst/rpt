#!/usr/bin/env bash
# Read-only, same-repository APT/RPT dependency-plan differential.
set -euo pipefail
# Dedicated VM acceptance matrix can replace the disposable guest distro;
# ordinary CI remains pinned to the known-good Debian Bookworm baseline.
sandbox_image="${RPT_SANDBOX_IMAGE:-debian:bookworm-slim}"
case "$sandbox_image" in
  debian:bookworm-slim|ubuntu:22.04|ubuntu:24.04) ;;
  *) echo "unsupported CI sandbox guest image: $sandbox_image" >&2; exit 1 ;;
esac
root="$(mktemp -d /tmp/rpt-signed.XXXXXXXX)"
server_pid=""
upgrade_pid=""
crash_pid=""
session="ci-test"
cleanup() {
  if [[ -n "$crash_pid" ]]; then kill -9 "$crash_pid" 2>/dev/null || :; wait "$crash_pid" 2>/dev/null || :; fi
  docker rm -f rpt-session-crash-ci rpt-session-restored-ci >/dev/null 2>&1 || :
  if [[ -n "$upgrade_pid" ]]; then kill "$upgrade_pid" 2>/dev/null || :; wait "$upgrade_pid" 2>/dev/null || :; fi
  docker rm -f "rpt-session-$session" >/dev/null 2>&1 || :
  if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || :; wait "$server_pid" 2>/dev/null || :; fi
  if [[ -d "$root" && ! -L "$root" && "$root" == /tmp/rpt-signed.* ]]; then
    sudo rm -rf -- "$root/native-guest" "$root/native-journal" \
      "$root/native-mark-guest" "$root/native-mark-journal" \
      "$root/native-checkpoint" "$root/native-restored" \
      "$root/native-auto-remove" "$root/native-auto-mark" \
      "$root/native-upgrade-guest" "$root/native-auto-journal"
  fi
  rm -rf "$root"
}
trap cleanup EXIT
command -v gpg >/dev/null
command -v gpgv >/dev/null
command -v python3 >/dev/null
mkdir -p "$root/www/dists/fixture/main/binary-$(dpkg --print-architecture)" "$root/gnupg" \
  "$root/apt/lists/partial" "$root/apt/cache/archives/partial" "$root/apt/etc/empty" \
  "$root/rpt-apt/sources.list.d" "$root/rpt-cache"
chmod 700 "$root/gnupg"
arch="$(dpkg --print-architecture)"
index="$root/www/dists/fixture/main/binary-$arch/Packages"
cat > "$index" <<'PACKAGES'
Package: rpt-app
Version: 1.0
Architecture: all
Depends: rpt-lib (>= 2)
Filename: pool/rpt-app_1.0_all.deb
Size: 1
Description: signed differential app

Package: rpt-lib
Version: 2.0
Architecture: all
Filename: pool/rpt-lib_2.0_all.deb
Size: 1
Description: signed differential library

Package: rpt-pre-app
Version: 1.0
Architecture: all
Pre-Depends: rpt-foundation (>= 2)
Filename: pool/rpt-pre-app_1.0_all.deb
Size: 1
Description: signed differential predepends

Package: rpt-foundation
Version: 2.0
Architecture: all
Filename: pool/rpt-foundation_2.0_all.deb
Size: 1
Description: signed differential foundation

Package: rpt-broken
Version: 1.0
Architecture: all
Depends: rpt-missing (>= 1)
Filename: pool/rpt-broken_1.0_all.deb
Size: 1
Description: signed differential unsatisfiable package


Package: rpt-alternative
Version: 1.0
Architecture: all
Depends: rpt-nonexistent | rpt-lib (>= 2)
Filename: pool/rpt-alternative_1.0_all.deb
Size: 1
Description: fallback alternative

Package: rpt-provider
Version: 1.0
Architecture: all
Provides: rpt-virtual-api (= 2)
Filename: pool/rpt-provider_1.0_all.deb
Size: 1
Description: versioned virtual provider

Package: rpt-virtual-app
Version: 1.0
Architecture: all
Depends: rpt-virtual-api (>= 2)
Filename: pool/rpt-virtual-app_1.0_all.deb
Size: 1
Description: versioned virtual dependency

Package: rpt-conflict
Version: 1.0
Architecture: all
Conflicts: rpt-lib
Filename: pool/rpt-conflict_1.0_all.deb
Size: 1
Description: conflicts with library

Package: rpt-version-mismatch
Version: 1.0
Architecture: all
Depends: rpt-lib (>= 3)
Filename: pool/rpt-version-mismatch_1.0_all.deb
Size: 1
Description: unsatisfied version constraint

Package: rpt-cycle-a
Version: 1.0
Architecture: all
Depends: rpt-cycle-b
Filename: pool/rpt-cycle-a_1.0_all.deb
Size: 1
Description: cycle first node

Package: rpt-cycle-b
Version: 1.0
Architecture: all
Depends: rpt-cycle-a
Filename: pool/rpt-cycle-b_1.0_all.deb
Size: 1
Description: cycle second node


Package: rpt-self-conflict
Version: 1.0
Architecture: all
Depends: rpt-lib (>= 2)
Conflicts: rpt-lib
Filename: pool/rpt-self-conflict_1.0_all.deb
Size: 1
Description: impossible dependency and conflict combination

Package: rpt-self-breaks
Version: 1.0
Architecture: all
Depends: rpt-lib (>= 2)
Breaks: rpt-lib
Filename: pool/rpt-self-breaks_1.0_all.deb
Size: 1
Description: impossible dependency and breaks combination

Package: rpt-alternative-conflict
Version: 1.0
Architecture: all
Depends: rpt-lib | rpt-foundation
Conflicts: rpt-lib
Filename: pool/rpt-alternative-conflict_1.0_all.deb
Size: 1
Description: must backtrack to nonconflicting alternative

Package: rpt-alternative-breaks
Version: 1.0
Architecture: all
Depends: rpt-lib | rpt-foundation
Breaks: rpt-lib
Filename: pool/rpt-alternative-breaks_1.0_all.deb
Size: 1
Description: must backtrack to nonbreaking alternative

Package: rpt-order-app
Version: 1.0
Architecture: all
Depends: rpt-zed (>= 1) | rpt-alpha (>= 1)
Filename: pool/rpt-order-app_1.0_all.deb
Size: 1
Description: prefer first viable declared dependency, not alphabetically first

Package: rpt-zed
Version: 1.0
Architecture: all
Filename: pool/rpt-zed_1.0_all.deb
Size: 1
Description: first declared dependency

Package: rpt-alpha
Version: 1.0
Architecture: all
Filename: pool/rpt-alpha_1.0_all.deb
Size: 1
Description: second declared dependency but alphabetically first

Package: rpt-joint-app
Version: 1.0
Architecture: all
Depends: rpt-joint-first | rpt-joint-fallback
Filename: pool/rpt-joint-app_1.0_all.deb
Size: 1
Description: joint requested-root dependency fixture

Package: rpt-joint-first
Version: 1.0
Architecture: all
Conflicts: rpt-joint-blocker
Filename: pool/rpt-joint-first_1.0_all.deb
Size: 1
Description: preferred dependency incompatible with requested root

Package: rpt-joint-fallback
Version: 1.0
Architecture: all
Filename: pool/rpt-joint-fallback_1.0_all.deb
Size: 1
Description: safe alternative dependency

Package: rpt-joint-blocker
Version: 1.0
Architecture: all
Filename: pool/rpt-joint-blocker_1.0_all.deb
Size: 1
Description: explicitly requested mandatory root

Package: rpt-global-first
Version: 1.0
Architecture: all
Depends: rpt-global-preferred | rpt-global-fallback
Filename: pool/rpt-global-first_1.0_all.deb
Size: 1
Description: earlier root with locally viable but globally bad alternative

Package: rpt-global-preferred
Version: 1.0
Architecture: all
Conflicts: rpt-global-blocker
Filename: pool/rpt-global-preferred_1.0_all.deb
Size: 1
Description: preferred dependency conflicts with later root transitive dependency

Package: rpt-global-fallback
Version: 1.0
Architecture: all
Filename: pool/rpt-global-fallback_1.0_all.deb
Size: 1
Description: dependency alternative compatible with full requested graph

Package: rpt-global-second
Version: 1.0
Architecture: all
Depends: rpt-global-blocker
Filename: pool/rpt-global-second_1.0_all.deb
Size: 1
Description: later mandatory root whose dependency exposes global conflict

Package: rpt-global-blocker
Version: 1.0
Architecture: all
Filename: pool/rpt-global-blocker_1.0_all.deb
Size: 1
Description: blocker indirectly required by later root

Package: rpt-multi-virtual-app
Version: 1.0
Architecture: all
Depends: rpt-multi-virtual-api (>= 2)
Filename: pool/rpt-multi-virtual-app_1.0_all.deb
Size: 1
Description: multiple virtual-provider dependency with forced unique choice

Package: rpt-multi-virtual-a
Version: 1.0
Architecture: all
Provides: rpt-multi-virtual-api (= 2)
Conflicts: rpt-multi-virtual-blocker
Filename: pool/rpt-multi-virtual-a_1.0_all.deb
Size: 1
Description: unsuitable virtual provider in joint install

Package: rpt-multi-virtual-b
Version: 1.0
Architecture: all
Provides: rpt-multi-virtual-api (= 2)
Filename: pool/rpt-multi-virtual-b_1.0_all.deb
Size: 1
Description: sole viable virtual provider in joint install

Package: rpt-multi-virtual-blocker
Version: 1.0
Architecture: all
Filename: pool/rpt-multi-virtual-blocker_1.0_all.deb
Size: 1
Description: jointly requested package restricting virtual provider choices

Package: rpt-installed-virtual-app
Version: 1.0
Architecture: all
Depends: rpt-local-virtual-service (>= 2)
Filename: pool/rpt-installed-virtual-app_1.0_all.deb
Size: 1
Description: signed installed-provider dependency parity fixture

Package: rpt-installed-virtual-bad
Version: 1.0
Architecture: all
Depends: rpt-local-virtual-service (>= 3)
Filename: pool/rpt-installed-virtual-bad_1.0_all.deb
Size: 1
Description: installed provider version too old for dependency

Package: rpt-soft-app
Version: 1.0
Architecture: all
Recommends: rpt-soft-extra (>= 1)
Filename: pool/rpt-soft-app_1.0_all.deb
Size: 1
Description: recommends optional addon

Package: rpt-soft-extra
Version: 1.0
Architecture: all
Filename: pool/rpt-soft-extra_1.0_all.deb
Size: 1
Description: optional addon for Recommends parity

PACKAGES
# Each synthetic pool artifact contains one byte; sign the real digest in Packages.
payload_sha="$(printf 'x' | sha256sum | cut -d' ' -f1)"
sed -i "/^Size: 1$/a SHA256: $payload_sha" "$index"
while IFS= read -r filename; do
  mkdir -p "$root/www/$(dirname "$filename")"
  printf 'x' > "$root/www/$filename"
done < <(sed -n 's/^Filename: //p' "$index")
# Build a real Debian archive, alongside one-byte download-only fixtures.
mkdir -p "$root/sandbox-pkg/DEBIAN" "$root/sandbox-pkg/usr/share/rpt-test"
cat > "$root/sandbox-pkg/DEBIAN/control" <<'CONTROL'
Package: rpt-sandbox-demo
Version: 1.0
Architecture: all
Maintainer: RPT CI <rpt-test@example.invalid>
Description: isolated real dpkg installation fixture
CONTROL
printf 'sandbox install succeeded\n' > "$root/sandbox-pkg/usr/share/rpt-test/installed.txt"
# Real conffile fixture: remove must preserve it; purge must delete it.
mkdir -p "$root/sandbox-pkg/etc/rpt-test"
printf 'enabled=true\n' > "$root/sandbox-pkg/etc/rpt-test/rpt-demo.conf"
printf '/etc/rpt-test/rpt-demo.conf\n' > "$root/sandbox-pkg/DEBIAN/conffiles"
dpkg-deb --build --root-owner-group "$root/sandbox-pkg" \
  "$root/www/pool/rpt-sandbox-demo_1.0_all.deb" >/dev/null
demo_artifact="$root/www/pool/rpt-sandbox-demo_1.0_all.deb"
demo_hash="$(sha256sum "$demo_artifact" | cut -d' ' -f1)"
demo_size="$(wc -c < "$demo_artifact" | tr -d ' ')"
cat >> "$index" <<EOF
Package: rpt-sandbox-demo
Version: 1.0
Architecture: all
Filename: pool/rpt-sandbox-demo_1.0_all.deb
SHA256: $demo_hash
Size: $demo_size
Description: real sandbox dpkg package

Package: rpt-lie
Version: 1.0
Architecture: all
Filename: pool/rpt-sandbox-demo_1.0_all.deb
SHA256: $demo_hash
Size: $demo_size
Description: intentionally inconsistent archive control fixture

EOF
# Signed install graph for native APT mark parity:
# root -> new dependency + pre-existing explicitly manual dependency.
# These real packages have no maintainer scripts and are never installed
# onto the VM host.
for pkg in rpt-mark-auto rpt-mark-preserved rpt-mark-root; do
  mkdir -p "$root/$pkg/DEBIAN" "$root/$pkg/usr/share/rpt-mark-test"
  printf '%s\n' "$pkg" > "$root/$pkg/usr/share/rpt-mark-test/$pkg.txt"
  depends=""
  if [[ "$pkg" == rpt-mark-root ]]; then
    depends=$'\nDepends: rpt-mark-auto (= 1.0), rpt-mark-preserved (= 1.0)'
  fi
  cat > "$root/$pkg/DEBIAN/control" <<CONTROL
Package: $pkg
Version: 1.0
Architecture: all
Maintainer: RPT CI <rpt-test@example.invalid>
Description: native automatic versus manual dependency mark fixture$depends
CONTROL
  deb="pool/$pkg"_1.0_all.deb
  dpkg-deb --build --root-owner-group "$root/$pkg" "$root/www/$deb" >/dev/null
  sha="$(sha256sum "$root/www/$deb" | cut -d' ' -f1)"
  length="$(wc -c < "$root/www/$deb" | tr -d ' ')"
  cat >> "$index" <<EOF
Package: $pkg
Version: 1.0
Architecture: all
Filename: $deb
SHA256: $sha
Size: $length
EOF
  if [[ "$pkg" == rpt-mark-root ]]; then
    printf 'Depends: rpt-mark-auto (= 1.0), rpt-mark-preserved (= 1.0)\n' >> "$index"
  fi
  printf 'Description: signed native APT mark graph fixture\n\n' >> "$index"
done

gpg --homedir "$root/gnupg" --batch --pinentry-mode loopback --passphrase '' \
  --quick-generate-key 'RPT differential <rpt-diff@example.invalid>' ed25519 sign 0 >/dev/null 2>&1
gpg --homedir "$root/gnupg" --batch --export 'RPT differential <rpt-diff@example.invalid>' > "$root/keyring.gpg"
release="$root/www/dists/fixture/Release"
hash="$(sha256sum "$index" | cut -d' ' -f1)"
size="$(wc -c < "$index" | tr -d ' ')"
cat > "$release" <<EOF
Origin: RPT Differential
Label: RPT Differential
Suite: fixture
Codename: fixture
Date: $(LC_ALL=C date -Ru)
Architectures: $arch
Components: main
SHA256:
 $hash $size main/binary-$arch/Packages
EOF
gpg --homedir "$root/gnupg" --batch --yes --pinentry-mode loopback --passphrase '' \
  --clearsign --digest-algo SHA256 --output "$root/www/dists/fixture/InRelease" "$release"
gpgv --keyring "$root/keyring.gpg" "$root/www/dists/fixture/InRelease" >/dev/null 2>&1
# Bind to loopback and use an OS-assigned port; avoid global APT state.
python3 - "$root" <<'PY'
import socket, sys
with socket.socket() as s:
    s.bind(("127.0.0.1", 0))
    open(sys.argv[1] + "/port", "w").write(str(s.getsockname()[1]))
PY
port="$(cat "$root/port")"
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$root/www" >"$root/http.log" 2>&1 &
server_pid="$!"
for _ in {1..40}; do
  if python3 - "$port" <<'PY'
import sys, urllib.request
try: urllib.request.urlopen("http://127.0.0.1:" + sys.argv[1] + "/dists/fixture/InRelease", timeout=.3)
except Exception: sys.exit(1)
PY
  then break; fi
  sleep .1
done
uri="http://127.0.0.1:$port"
printf 'deb [signed-by=%s] %s fixture main\n' "$root/keyring.gpg" "$uri" > "$root/apt/etc/sources.list"
cp "$root/apt/etc/sources.list" "$root/rpt-apt/sources.list"
: > "$root/apt/status"
opts=(
  -o "Dir::State::status=$root/apt/status"
  -o "Dir::State::lists=$root/apt/lists"
  -o "Dir::Cache=$root/apt/cache"
  -o "Dir::Etc::sourcelist=$root/apt/etc/sources.list"
  -o "Dir::Etc::sourceparts=$root/apt/etc/empty"
  -o "APT::Get::List-Cleanup=0"
  -o "Debug::NoLocking=1"
)
apt-get "${opts[@]}" update >"$root/apt-update.log"
./target/debug/rpt update --root "$root/rpt-apt" --output "$root/rpt-cache" --no-progress >"$root/rpt-update.log"
# Verify the policy-selected dependency closure maps to exact signed bytes.
./target/debug/rpt prepare-download-plan \
  --cache "$root/rpt-cache" --status "$root/apt/status" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/locked-manifest.json" rpt-app
python3 - "$root/locked-manifest.json" "$payload_sha" <<'PY'
import json, sys
plan=json.load(open(sys.argv[1]))
assert not plan['executable']
assert [(p['name'], p['version'], p['architecture']) for p in plan['packages']] == [
    ('rpt-lib','2.0','all'), ('rpt-app','1.0','all')]
assert all(p['sha256'] == sys.argv[2] and p['size'] == 1 for p in plan['packages'])
PY
./target/debug/rpt fetch --manifest "$root/locked-manifest.json" \
  --output "$root/downloaded" --workers 2 --segments 1 >"$root/rpt-fetch.log"
test "$(sha256sum "$root/downloaded/pool/rpt-lib_2.0_all.deb" | cut -d' ' -f1)" = "$payload_sha"
test "$(sha256sum "$root/downloaded/pool/rpt-app_1.0_all.deb" | cut -d' ' -f1)" = "$payload_sha"
./target/debug/rpt verify-download-plan --manifest "$root/locked-manifest.json" \
  --artifacts "$root/downloaded" --cache "$root/rpt-cache" --architecture "$arch" > "$root/verified.json"
python3 - "$root/verified.json" <<'PY'
import json,sys
report=json.load(open(sys.argv[1]))
assert report['verified'] == 2 and report['bytes'] == 2 and not report['executable']
PY


# Run a real dpkg transaction in a disposable isolated container.
command -v docker >/dev/null
docker pull "$sandbox_image" >/dev/null
./target/debug/rpt prepare-download-plan \
  --cache "$root/rpt-cache" --status "$root/apt/status" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/sandbox-manifest.json" rpt-sandbox-demo
./target/debug/rpt fetch --manifest "$root/sandbox-manifest.json" \
  --output "$root/sandbox-artifacts" --workers 1 --segments 1 >"$root/sandbox-fetch.log"
./target/debug/rpt sandbox-install \
  --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --architecture "$arch" --image "$sandbox_image" \
  --journal-dir "$root/sandbox-journal" --id sandbox-ci --timeout-seconds 180
grep -Fq '"phase": "succeeded"' "$root/sandbox-journal/sandbox-ci.json"
# RPT native backend: real dpkg --root, outside Docker, on a marked
# disposable root filesystem. Compare the VM host dpkg state separately.
sudo -n true
native_guest="$root/native-guest"
sudo mkdir -p "$native_guest"
native_container="rpt-native-export-$(basename "$root")"
docker create --name "$native_container" --network=none \
  --entrypoint /bin/sh "$sandbox_image" -c true >/dev/null
docker export "$native_container" | sudo tar -xpf - -C "$native_guest"
docker rm "$native_container" >/dev/null
printf 'rpt-disposable-rootfs-v1\n' \
  | sudo tee "$native_guest/.rpt-disposable-rootfs" >/dev/null
if ./target/debug/rpt native-rootfs-prepare-transaction --rootfs / \
    --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
    --output "$root/native-invalid.json" rpt-sandbox-demo >/dev/null 2>&1; then
  echo "Native rootfs planner incorrectly accepted live /" >&2; exit 1
fi
./target/debug/rpt native-rootfs-prepare-transaction \
  --rootfs "$native_guest" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/native-intent.json" rpt-sandbox-demo
python3 - "$root/native-intent.json" "$native_guest" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['session']=='native-rootfs:'+sys.argv[2]
assert [(p['name'],p['version']) for p in x['packages']]==[('rpt-sandbox-demo','1.0')]
assert x['executable'] is False
PY
python3 - "$root/native-intent.json" "$root/native-tampered.json" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]))
x['status_sha256']='0'*64
json.dump(x,open(sys.argv[2],'w'))
PY
if sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$native_guest" --id native-tampered \
  --transaction "$root/native-tampered.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-journal" >/dev/null 2>&1; then
  echo "Native rootfs executor accepted tampered status hash" >&2; exit 1
fi
sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$native_guest" --id native-install \
  --transaction "$root/native-intent.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-journal" --timeout-seconds 180
grep -Fq '"phase": "succeeded"' "$root/native-journal/native-install.json"
test "$(sudo dpkg-query --admindir="$native_guest/var/lib/dpkg" -W \
  '-f=${Status}|${Version}|${Architecture}' rpt-sandbox-demo)" = \
  'install ok installed|1.0|all'
test -z "$(sudo dpkg --root="$native_guest" --audit)"
if sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$native_guest" --id native-replay \
  --transaction "$root/native-intent.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-journal" >/dev/null 2>&1; then
  echo "Native rootfs executor accepted replay after status change" >&2; exit 1
fi
./target/debug/rpt native-rootfs-prepare-transaction \
  --rootfs "$native_guest" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/native-noop.json" rpt-sandbox-demo
python3 - "$root/native-noop.json" <<'PY'
import json,sys
assert json.load(open(sys.argv[1]))['packages']==[]
PY
# Out-of-Docker remove and purge use guest dependency safety checks.
if sudo ./target/debug/rpt native-rootfs-remove-planned \
  --rootfs "$native_guest" --id native-unsafe \
  --journal-dir "$root/native-journal" --architecture "$arch" dpkg \
  >/dev/null 2>&1; then
  echo "Native rootfs removal accepted an Essential package" >&2; exit 1
fi
sudo ./target/debug/rpt native-rootfs-remove-planned \
  --rootfs "$native_guest" --id native-remove \
  --checkpoint-rootfs "$root/native-auto-remove" \
  --journal-dir "$root/native-journal" --architecture "$arch" rpt-sandbox-demo
remove_receipt="$root/native-journal/native-rootfs-snapshots/native-remove-before.json"
sudo ./target/debug/rpt native-rootfs-snapshot-verify \
  --receipt "$remove_receipt" >"$root/native-auto-remove-verify.json"
test "$(sudo dpkg-query --admindir="$root/native-auto-remove/var/lib/dpkg" \
  -W '-f=${Status}' rpt-sandbox-demo)" = 'install ok installed'
grep -Fq '"phase": "succeeded"' "$root/native-journal/native-remove.json"
# Remove leaves configuration and a dpkg config-files record.
test -f "$native_guest/etc/rpt-test/rpt-demo.conf"
test "$(sudo dpkg-query --admindir="$native_guest/var/lib/dpkg" \
  -W '-f=${Status}' rpt-sandbox-demo)" = 'deinstall ok config-files'
if sudo ./target/debug/rpt native-rootfs-remove-planned \
  --rootfs "$native_guest" --id native-repeat-remove \
  --journal-dir "$root/native-journal" --architecture "$arch" rpt-sandbox-demo \
  >/dev/null 2>&1; then
  echo "Native remove accepted a config-files-only package without --purge" >&2; exit 1
fi
sudo ./target/debug/rpt native-rootfs-remove-planned \
  --rootfs "$native_guest" --id native-config-only-purge \
  --journal-dir "$root/native-journal" --architecture "$arch" \
  --purge rpt-sandbox-demo > "$root/native-config-only-purge.log"
grep -Fq '"mode": "purge"' "$root/native-config-only-purge.log"
grep -Fq '"phase": "succeeded"' "$root/native-journal/native-config-only-purge.json"
test ! -e "$native_guest/etc/rpt-test/rpt-demo.conf"
config_only_state="$(sudo dpkg-query --admindir="$native_guest/var/lib/dpkg" \
  -W '-f=${Status}' rpt-sandbox-demo 2>/dev/null || true)"
test -z "$config_only_state" || test "$config_only_state" = 'unknown ok not-installed'
./target/debug/rpt native-rootfs-prepare-transaction \
  --rootfs "$native_guest" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/native-reinstall.json" rpt-sandbox-demo
sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$native_guest" --id native-reinstall \
  --transaction "$root/native-reinstall.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-journal" --timeout-seconds 180
# Exercise real APT marking outside Docker using the native RPT backend.
sudo ./target/debug/rpt native-rootfs-mark \
  --rootfs "$native_guest" --id native-mark-manual \
  --journal-dir "$root/native-journal" manual rpt-sandbox-demo
sudo chroot "$native_guest" /usr/bin/apt-mark showmanual rpt-sandbox-demo \
  | grep -Fxq rpt-sandbox-demo
./target/debug/rpt plan-autoremove \
  --status "$native_guest/var/lib/dpkg/status" \
  --extended-states "$native_guest/var/lib/apt/extended_states" \
  --architecture "$arch" > "$root/native-after-manual.json"
python3 - "$root/native-after-manual.json" <<'PY'
import json,sys
assert 'rpt-sandbox-demo:all' not in json.load(open(sys.argv[1]))['remove']
PY
sudo ./target/debug/rpt native-rootfs-mark \
  --rootfs "$native_guest" --id native-mark-auto \
  --checkpoint-rootfs "$root/native-auto-mark" \
  --journal-dir "$root/native-journal" auto rpt-sandbox-demo
sudo ./target/debug/rpt native-rootfs-snapshot-verify \
  --receipt "$root/native-journal/native-rootfs-snapshots/native-mark-auto-before.json" \
  >"$root/native-auto-mark-verify.json"
sudo chroot "$root/native-auto-mark" /usr/bin/apt-mark showmanual rpt-sandbox-demo \
  | grep -Fxq rpt-sandbox-demo
sudo chroot "$native_guest" /usr/bin/apt-mark showauto rpt-sandbox-demo \
  | grep -Fxq rpt-sandbox-demo
# Debian slim exports may mark apt itself automatic. This acceptance fixture
# reuses the guest for later signed installs, so retain the package manager
# instead of letting its unrelated preexisting auto mark remove apt-mark.
sudo chroot "$native_guest" /usr/bin/apt-mark manual apt
sudo ./target/debug/rpt native-rootfs-autoremove-planned \
  --rootfs "$native_guest" --id native-autoremove \
  --journal-dir "$root/native-journal" --architecture "$arch" --timeout-seconds 180
grep -Fq '"phase": "succeeded"' "$root/native-journal/native-autoremove.json"
sudo test -x "$native_guest/usr/bin/apt-mark"
./target/debug/rpt native-rootfs-prepare-transaction \
  --rootfs "$native_guest" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/native-purge-install.json" rpt-sandbox-demo
sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$native_guest" --id native-purge-install \
  --transaction "$root/native-purge-install.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-journal" --timeout-seconds 180
sudo ./target/debug/rpt native-rootfs-remove-planned \
  --rootfs "$native_guest" --id native-purge \
  --journal-dir "$root/native-journal" --architecture "$arch" \
  --purge rpt-sandbox-demo
grep -Fq '"phase": "succeeded"' "$root/native-journal/native-purge.json"
test ! -e "$native_guest/etc/rpt-test/rpt-demo.conf"
purged_state="$(sudo dpkg-query --admindir="$native_guest/var/lib/dpkg" \
  -W '-f=${Status}' rpt-sandbox-demo 2>/dev/null || true)"
test -z "$purged_state" || test "$purged_state" = 'unknown ok not-installed'
test -z "$(sudo dpkg --root="$native_guest" --audit)"
test ! -e /var/lib/dpkg/info/rpt-sandbox-demo.list
echo "PASS native rootfs install, remove, reinstall, autoremove, purge and no host changes"
# Fresh signed dependency install has two different mark outcomes. The
# already installed manual dependency must not be silently demoted to auto.
mark_guest="$root/native-mark-guest"
sudo mkdir -p "$mark_guest"
mark_container="rpt-mark-export-$(basename "$root")"
docker create --name "$mark_container" --network=none \
  --entrypoint /bin/sh "$sandbox_image" -c true >/dev/null
docker export "$mark_container" | sudo tar -xpf - -C "$mark_guest"
docker rm "$mark_container" >/dev/null
printf 'rpt-disposable-rootfs-v1\n' \
  | sudo tee "$mark_guest/.rpt-disposable-rootfs" >/dev/null
sudo dpkg --root="$mark_guest" --install \
  "$root/www/pool/rpt-mark-preserved_1.0_all.deb"
sudo chroot "$mark_guest" /usr/bin/apt-mark manual rpt-mark-preserved
sudo chroot "$mark_guest" /usr/bin/apt-mark showmanual rpt-mark-preserved \
  | grep -Fxq rpt-mark-preserved
./target/debug/rpt native-rootfs-prepare-transaction \
  --rootfs "$mark_guest" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/native-mark-intent.json" rpt-mark-root
python3 - "$root/native-mark-intent.json" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]))
assert [(p['name'],p['version']) for p in x['packages']] == [
  ('rpt-mark-auto','1.0'), ('rpt-mark-root','1.0')]
PY
./target/debug/rpt fetch --manifest "$root/native-mark-intent.json" \
  --output "$root/native-mark-artifacts" --workers 2 --segments 1
sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$mark_guest" --id native-mark-install \
  --transaction "$root/native-mark-intent.json" \
  --artifacts "$root/native-mark-artifacts" \
  --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-mark-journal" --timeout-seconds 180
sudo chroot "$mark_guest" /usr/bin/apt-mark showmanual rpt-mark-root \
  | grep -Fxq rpt-mark-root
sudo chroot "$mark_guest" /usr/bin/apt-mark showauto rpt-mark-auto \
  | grep -Fxq rpt-mark-auto
sudo chroot "$mark_guest" /usr/bin/apt-mark showmanual rpt-mark-preserved \
  | grep -Fxq rpt-mark-preserved
test -z "$(sudo dpkg --root="$mark_guest" --audit)"
grep -Fq '"phase": "succeeded"' "$root/native-mark-journal/native-mark-install.json"
echo "PASS native signed install auto dependency/manual root, preserved preinstalled manual dependency"
# No-op install: the requested root already has the signed candidate version,
# but its APT mark is auto. A bound, zero-archive transaction must make it
# manual, leave dpkg status bit-for-bit unchanged, and need no artifact dir.
sudo chroot "$mark_guest" /usr/bin/apt-mark auto rpt-mark-root
sudo chroot "$mark_guest" /usr/bin/apt-mark showauto rpt-mark-root \
  | grep -Fxq rpt-mark-root
./target/debug/rpt native-rootfs-prepare-transaction \
  --rootfs "$mark_guest" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/native-mark-only.json" rpt-mark-root
python3 - "$root/native-mark-only.json" "$root/native-mark-only-tampered.json" <<'PY'
import json,sys
data=json.load(open(sys.argv[1]))
assert data['packages']==[] and data['steps']==[]
assert data['requested']==['rpt-mark-root'] and data['executable'] is False
data['status_sha256']='0'*64
json.dump(data,open(sys.argv[2],'w'))
PY
if sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$mark_guest" --id native-mark-only-tampered \
  --transaction "$root/native-mark-only-tampered.json" \
  --artifacts "$root/empty-marks-no-artifacts" \
  --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-mark-journal" --timeout-seconds 180 \
  >"$root/native-mark-only-tampered.log" 2>&1; then
  echo "Native mark-only accepted tampered dpkg status" >&2; exit 1
fi
sudo chroot "$mark_guest" /usr/bin/apt-mark showauto rpt-mark-root \
  | grep -Fxq rpt-mark-root
mark_only_before="$(sudo sha256sum "$mark_guest/var/lib/dpkg/status" | cut -d' ' -f1)"
sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$mark_guest" --id native-mark-only \
  --transaction "$root/native-mark-only.json" \
  --artifacts "$root/empty-marks-no-artifacts" \
  --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-mark-journal" --timeout-seconds 180 \
  >"$root/native-mark-only.log"
grep -Fq 'mark-only transaction verified and applied' "$root/native-mark-only.log"
grep -Fq '"phase": "succeeded"' "$root/native-mark-journal/native-mark-only.json"
sudo chroot "$mark_guest" /usr/bin/apt-mark showmanual rpt-mark-root \
  | grep -Fxq rpt-mark-root
sudo chroot "$mark_guest" /usr/bin/apt-mark showmanual rpt-mark-preserved \
  | grep -Fxq rpt-mark-preserved
test "$(sudo sha256sum "$mark_guest/var/lib/dpkg/status" | cut -d' ' -f1)" = "$mark_only_before"
test -z "$(sudo dpkg --root="$mark_guest" --audit)"
echo "PASS signed native zero-archive mark-only; no dpkg changes or artifact directory"

# Read-only recovery diagnosis: no journal unlock, no package changes, and
# malformed/unfinished metadata is reported rather than silently discarded.
sudo ./target/debug/rpt native-rootfs-recovery-inspect \
  --rootfs "$mark_guest" --journal-dir "$root/native-mark-journal" \
  >"$root/native-healthy-recovery.json"
python3 - "$root/native-healthy-recovery.json" <<'PY'
import json, sys
data=json.load(open(sys.argv[1]))
assert data["dpkg_audit"] == ""
assert len(data["dpkg_status_sha256"]) == 64
assert data["journal"]["records"]
assert data["needs_operator_review"] is False
PY
mkdir -p "$root/native-recovery-unfinished"
printf '%s\n' '{"id":"pending","phase":"interrupted","error":"SIGTERM"}' \
  >"$root/native-recovery-unfinished/pending.json"
sudo ./target/debug/rpt native-rootfs-recovery-inspect \
  --rootfs "$mark_guest" --journal-dir "$root/native-recovery-unfinished" \
  >"$root/native-interrupted-recovery.json"
python3 - "$root/native-interrupted-recovery.json" <<'PY'
import json, sys
data=json.load(open(sys.argv[1]))
assert data["needs_operator_review"]
assert data["journal"]["records"][0]["phase"] == "interrupted"
assert data["journal"]["requires_manual_recovery"]
PY
grep -Fq '"phase":"interrupted"' "$root/native-recovery-unfinished/pending.json"
if sudo ./target/debug/rpt native-rootfs-recovery-inspect \
  --rootfs / --journal-dir "$root/native-mark-journal" \
  >/dev/null 2>&1; then
  echo "Native recovery inspector unexpectedly accepted live /" >&2; exit 1
fi
echo "PASS native read-only recovery report, interrupted journal and live-root refusal"

# Full disposable rootfs snapshot and restore to a NEW directory; source
# remains mutable only in isolation, snapshot bytes are rehashed against
# a durable receipt and an interrupted original is never auto-unlocked.
sudo ./target/debug/rpt native-rootfs-snapshot-create \
  --rootfs "$mark_guest" --snapshot-rootfs "$root/native-checkpoint" \
  --journal-dir "$root/native-mark-journal" --id native-pre-fault \
  >"$root/native-checkpoint-create.json"
snapshot_receipt="$root/native-mark-journal/native-rootfs-snapshots/native-pre-fault.json"
sudo ./target/debug/rpt native-rootfs-snapshot-verify \
  --receipt "$snapshot_receipt" >"$root/native-checkpoint-verify.json"
python3 - "$root/native-checkpoint-create.json" "$root/native-checkpoint-verify.json" <<'PY'
import json,sys
a=json.load(open(sys.argv[1])); b=json.load(open(sys.argv[2]))
assert a==b
assert len(a['tree_sha256'])==64 and len(a['status_sha256'])==64
PY
receipt_before="$(sudo sha256sum "$snapshot_receipt" | cut -d' ' -f1)"
journal_before="$(sudo sha256sum "$root/native-mark-journal/native-mark-only.json" | cut -d' ' -f1)"
# Simulate changed/broken original only after checkpoint is verified.
sudo chroot "$mark_guest" /usr/bin/apt-mark auto rpt-mark-root
sudo chroot "$mark_guest" /usr/bin/apt-mark showauto rpt-mark-root \
  | grep -Fxq rpt-mark-root
sudo ./target/debug/rpt native-rootfs-snapshot-restore \
  --receipt "$snapshot_receipt" --new-rootfs "$root/native-restored" \
  >"$root/native-restore.json"
sudo chroot "$root/native-restored" /usr/bin/apt-mark showmanual rpt-mark-root \
  | grep -Fxq rpt-mark-root
sudo chroot "$mark_guest" /usr/bin/apt-mark showauto rpt-mark-root \
  | grep -Fxq rpt-mark-root
test -z "$(sudo dpkg --root="$root/native-restored" --audit)"
test "$(sudo sha256sum "$snapshot_receipt" | cut -d' ' -f1)" = "$receipt_before"
test "$(sudo sha256sum "$root/native-mark-journal/native-mark-only.json" | cut -d' ' -f1)" = "$journal_before"
if sudo ./target/debug/rpt native-rootfs-snapshot-restore \
  --receipt "$snapshot_receipt" --new-rootfs "$root/native-restored" >/dev/null 2>&1; then
  echo "Snapshot restore overwrote an existing guest" >&2; exit 1
fi
if sudo ./target/debug/rpt native-rootfs-snapshot-create \
  --rootfs / --snapshot-rootfs "$root/native-live-snapshot" \
  --journal-dir "$root/native-mark-journal" --id native-live >/dev/null 2>&1; then
  echo "Native snapshot unexpectedly accepted live root" >&2; exit 1
fi
# Any snapshot content modification invalidates future verification.
sudo sh -c "echo tampered >> '$root/native-checkpoint/usr/share/rpt-mark-test/rpt-mark-root.txt'"
if sudo ./target/debug/rpt native-rootfs-snapshot-verify \
  --receipt "$snapshot_receipt" >/dev/null 2>&1; then
  echo "Native snapshot missed post-capture payload tampering" >&2; exit 1
fi
echo "PASS native checkpoint/verify/restore-to-new-rootfs, tamper and overwrite refusal"




# Install/remove and install/purge exercise actual dpkg operations in the
# same isolated container; different journals and disposable roots each time.
for action in install-remove install-purge; do
  ./target/debug/rpt sandbox-install \
    --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
    --cache "$root/rpt-cache" --architecture "$arch" --image "$sandbox_image" \
    --journal-dir "$root/sandbox-journal" --id "sandbox-$action" \
    --timeout-seconds 180 --lifecycle "$action"
  grep -Fq '"phase": "succeeded"' "$root/sandbox-journal/sandbox-$action.json"
done
./target/debug/rpt prepare-download-plan \
  --cache "$root/rpt-cache" --status "$root/apt/status" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/mismatch-manifest.json" rpt-lie
if ./target/debug/rpt sandbox-install \
  --manifest "$root/mismatch-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --architecture "$arch" --image "$sandbox_image" \
  --journal-dir "$root/mismatch-journal" --id sandbox-mismatch \
  --timeout-seconds 180 >"$root/mismatch-stdout.log" 2>"$root/mismatch-stderr.log"; then
  echo "Sandbox unexpectedly accepted signed metadata disagreeing with .deb control" >&2
  exit 1
fi
grep -Fq 'Signed manifest disagrees with Debian control fields' "$root/mismatch-stderr.log"

# A persistent, isolated container keeps its own dpkg database across RPT
# install/remove/purge commands. None of this state is reflected on the host.
./target/debug/rpt sandbox-session-create \
  --session "$session" --image "$sandbox_image"
# Candidate selection must use installed dpkg status from THIS container.
./target/debug/rpt sandbox-session-prepare-download-plan \
  --session "$session" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/session-before.json" rpt-sandbox-demo
python3 - "$root/session-before.json" <<'PY'
import json,sys
manifest=json.load(open(sys.argv[1]))
assert not manifest['executable']
assert [(p['name'],p['version']) for p in manifest['packages']] == [('rpt-sandbox-demo','1.0')]
PY

./target/debug/rpt sandbox-session-install \
  --session "$session" --id session-install \
  --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --journal-dir "$root/session-journal" \
  --architecture "$arch" --timeout-seconds 180
./target/debug/rpt sandbox-session-inspect --session "$session" \
  | grep -Fq rpt-sandbox-demo
grep -Fq '"phase": "succeeded"' "$root/session-journal/session-install.json"
./target/debug/rpt sandbox-session-plan-install \
  --session "$session" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" rpt-sandbox-demo > "$root/session-replan.json"
./target/debug/rpt sandbox-session-prepare-download-plan \
  --session "$session" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/session-after.json" rpt-sandbox-demo
python3 - "$root/session-after.json" "$root/session-replan.json" <<'PY'
import json,sys
manifest=json.load(open(sys.argv[1]))
plan=json.load(open(sys.argv[2]))
assert manifest['packages'] == [] and not manifest['executable']
assert plan['changes'] == []  # installed candidate already satisfied; no transaction
PY

./target/debug/rpt sandbox-session-plan-remove \
  --session "$session" --architecture "$arch" rpt-sandbox-demo > "$root/plan-remove.json"
python3 - "$root/plan-remove.json" <<'PY'
import json,sys
plan=json.load(open(sys.argv[1]))
assert plan['removable'] and not plan['executable']
assert plan['remove'] == ['rpt-sandbox-demo:all']
PY
./target/debug/rpt sandbox-session-remove-planned \
  --session "$session" --id session-remove --journal-dir "$root/session-journal" \
  --architecture "$arch" --timeout-seconds 180 rpt-sandbox-demo
# A removed package with conffiles remains visible to dpkg-query --show;
# assert the actual dpkg installed state instead of hiding the residual config.
test "$(docker exec "rpt-session-$session" dpkg-query -W \
  '-f=${Status}' rpt-sandbox-demo)" = 'deinstall ok config-files'
./target/debug/rpt sandbox-session-install \
  --session "$session" --id session-reinstall \
  --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --journal-dir "$root/session-journal" \
  --architecture "$arch" --timeout-seconds 180
# APT mark operations use durable journals and snapshots instead of direct
# Docker copying of extended_states into the test guest.
# This disposable guest is seeded with an empty APT mark set for a precise
# autoremove oracle; real base images can contain unrelated automatic packages.
# This also exercises the initially absent extended_states path.
docker exec "rpt-session-$session" /bin/sh -ec 'rm -f /var/lib/apt/extended_states'
if ./target/debug/rpt sandbox-session-mark \
  --session "$session" --id invalid-mark --journal-dir "$root/session-journal" \
  auto nonexistent-rpt-package >/dev/null 2>&1; then
  echo "Unknown installed package was accepted by sandbox marking" >&2; exit 1
fi
if ./target/debug/rpt sandbox-session-mark \
  --session "$session" --id invalid-duplicate --journal-dir "$root/session-journal" \
  auto rpt-sandbox-demo rpt-sandbox-demo:all >/dev/null 2>&1; then
  echo "Duplicate marking request was accepted" >&2; exit 1
fi
./target/debug/rpt sandbox-session-mark \
  --session "$session" --id mark-auto --journal-dir "$root/session-journal" \
  --timeout-seconds 180 auto rpt-sandbox-demo
grep -Fq '"phase": "succeeded"' "$root/session-journal/mark-auto.json"
docker exec "rpt-session-$session" apt-mark showauto rpt-sandbox-demo \
  | grep -Fxq rpt-sandbox-demo
./target/debug/rpt sandbox-session-mark \
  --session "$session" --id mark-manual --journal-dir "$root/session-journal" \
  --timeout-seconds 180 manual rpt-sandbox-demo
docker exec "rpt-session-$session" apt-mark showmanual rpt-sandbox-demo \
  | grep -Fxq rpt-sandbox-demo
./target/debug/rpt sandbox-session-plan-autoremove \
  --session "$session" --architecture "$arch" > "$root/plan-after-manual.json"
python3 - "$root/plan-after-manual.json" <<'PY'
import json,sys
plan=json.load(open(sys.argv[1]))
assert 'rpt-sandbox-demo:all' not in plan['remove'], plan
PY
./target/debug/rpt sandbox-session-mark \
  --session "$session" --id mark-auto-again --journal-dir "$root/session-journal" \
  --timeout-seconds 180 auto rpt-sandbox-demo
./target/debug/rpt sandbox-session-plan-autoremove \
  --session "$session" --architecture "$arch" > "$root/plan-autoremove.json"
python3 - "$root/plan-autoremove.json" <<'PY'
import json,sys
plan=json.load(open(sys.argv[1]))
assert plan['removable'] and not plan['executable']
assert plan['remove'] == ['rpt-sandbox-demo:all']
PY
./target/debug/rpt sandbox-session-autoremove-planned \
  --session "$session" --id session-auto --journal-dir "$root/session-journal" \
  --architecture "$arch" --timeout-seconds 180
./target/debug/rpt sandbox-session-install \
  --session "$session" --id session-postauto-install \
  --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --journal-dir "$root/session-journal" \
  --architecture "$arch" --timeout-seconds 180
./target/debug/rpt sandbox-session-remove \
  --session "$session" --id session-purge --journal-dir "$root/session-journal" \
  --timeout-seconds 180 --purge rpt-sandbox-demo
./target/debug/rpt sandbox-session-destroy --session "$session"
if docker container inspect "rpt-session-$session" >/dev/null 2>&1; then
  echo "Docker sandbox survived explicit destroy" >&2
  exit 1
fi
echo "PASS persistent isolated dpkg install/remove/reinstall/purge"


test ! -s "$root/apt/status"
if dpkg-query -W rpt-sandbox-demo >/dev/null 2>&1; then
  echo "Unexpected host dpkg mutation from sandbox installation" >&2
  exit 1
fi
echo "PASS signed plan to real isolated dpkg install (host unchanged)"

# Compare exact package/version selections, not just package names.
# APT's Inst lines carry versions in parentheses; RPT emits the version as
# its second field. Keep the parsers independent and assert nonempty plans.
compare_plan() {
  local pkg="$1"
  apt-get "${opts[@]}" -s -y install "$pkg" >"$root/apt-plan"
  ./target/debug/rpt plan-install --cache "$root/rpt-cache" --status "$root/apt/status" \
    --apt-root "$root/rpt-apt" --architecture "$arch" "$pkg" >"$root/rpt-plan"
  sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-plan" | LC_ALL=C sort -u >"$root/apt-versions"
  sed -nE 's/^  ([^ ]+) ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-plan" | LC_ALL=C sort -u >"$root/rpt-versions"
  if [[ ! -s "$root/apt-versions" || ! -s "$root/rpt-versions" ]] || \
      ! diff -u "$root/apt-versions" "$root/rpt-versions"; then
    echo "APT/RPT package/version plan mismatch for $pkg" >&2
    cat "$root/apt-plan" "$root/rpt-plan" >&2
    exit 1
  fi
}
reject_plan() {
  local pkg="$1"
  if apt-get "${opts[@]}" -s -y install "$pkg" >"$root/apt-reject" 2>&1; then
    echo "APT unexpectedly accepted $pkg" >&2; exit 1
  fi
  if ./target/debug/rpt plan-install --cache "$root/rpt-cache" --status "$root/apt/status" \
    --apt-root "$root/rpt-apt" --architecture "$arch" "$pkg" >"$root/rpt-reject" 2>&1; then
    echo "RPT unexpectedly accepted $pkg" >&2; exit 1
  fi
}
for pkg in rpt-app rpt-pre-app rpt-alternative rpt-virtual-app rpt-alternative-conflict rpt-alternative-breaks rpt-cycle-a rpt-order-app; do
  compare_plan "$pkg"
done
for pkg in rpt-broken rpt-version-mismatch rpt-self-conflict rpt-self-breaks; do
  reject_plan "$pkg"
done
# Conflicts must be rejected when both packages are explicitly requested.
if apt-get "${opts[@]}" -s -y install rpt-app rpt-conflict >"$root/apt-conflict" 2>&1; then
  echo "APT accepted incompatible package pair" >&2; exit 1
fi
# The exact selected alternative must match APT; alphabetical ordering
# would incorrectly choose rpt-alpha over the first-listed rpt-zed.
grep -Eq '^Inst rpt-zed[[:space:]]' "$root/apt-plan"
if grep -Eq '^Inst rpt-alpha[[:space:]]' "$root/apt-plan"; then
  echo "APT selected the second alternative unexpectedly" >&2; exit 1
fi
# Multi-root joint planning uses one dependency graph, with no duplicate
# shared packages. Match APT's simulated installed package/version set.
apt-get "${opts[@]}" -s -y install rpt-order-app rpt-app >"$root/apt-multi"
./target/debug/rpt plan-install-batch-policy --cache "$root/rpt-cache" \
  --status "$root/apt/status" --apt-root "$root/rpt-apt" \
  --architecture "$arch" rpt-order-app rpt-app >"$root/rpt-multi"
sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-multi" | LC_ALL=C sort -u >"$root/apt-multi-versions"
sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-multi" | LC_ALL=C sort -u >"$root/rpt-multi-versions"
test -s "$root/apt-multi-versions"
test -s "$root/rpt-multi-versions"
diff -u "$root/apt-multi-versions" "$root/rpt-multi-versions"
if ./target/debug/rpt plan-install-batch-policy --cache "$root/rpt-cache" \
  --status "$root/apt/status" --apt-root "$root/rpt-apt" \
  --architecture "$arch" rpt-app rpt-conflict >"$root/rpt-multi-conflict" 2>&1; then
  echo "RPT unexpectedly accepted jointly conflicting roots" >&2; exit 1
fi
echo "PASS signed APT/RPT differential: Debian ordered alternatives and joint multi-root candidate parity"

# A later mandatory root must constrain earlier alternatives. APT's simulated
# package/version set is authoritative for both argument orders; neither
# requested root may be dropped to preserve the preferred dependency.
for ordering in app-first blocker-first; do
  if [[ "$ordering" == app-first ]]; then
    roots=(rpt-joint-app rpt-joint-blocker)
  else
    roots=(rpt-joint-blocker rpt-joint-app)
  fi
  apt-get "${opts[@]}" -s -y install "${roots[@]}" \
    >"$root/apt-joint-lookahead"
  ./target/debug/rpt plan-install-batch-policy \
    --cache "$root/rpt-cache" --status "$root/apt/status" \
    --apt-root "$root/rpt-apt" --architecture "$arch" \
    "${roots[@]}" >"$root/rpt-joint-lookahead"
  sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-joint-lookahead" \
    | LC_ALL=C sort -u >"$root/apt-joint-lookahead-versions"
  sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-joint-lookahead" \
    | LC_ALL=C sort -u >"$root/rpt-joint-lookahead-versions"
  test -s "$root/apt-joint-lookahead-versions"
  diff -u "$root/apt-joint-lookahead-versions" "$root/rpt-joint-lookahead-versions"
  for pkg in rpt-joint-app rpt-joint-blocker rpt-joint-fallback; do
    grep -Fxq "$pkg 1.0" "$root/apt-joint-lookahead-versions"
  done
  if grep -Fq "rpt-joint-first " "$root/apt-joint-lookahead-versions"; then
    echo "APT chose an incompatible package despite the explicit blocker root" >&2
    exit 1
  fi
done
echo "PASS signed APT/RPT mandatory-root dependency backtracking, both orders"

# The conflict is NOT with the later explicitly requested root itself:
# it arises only from that root's transitive mandatory Depends. RPT must
# revisit a choice already committed for an earlier root rather than
# stopping after the locally viable first alternative.
for ordering in first-second second-first; do
  if [[ "$ordering" == first-second ]]; then
    roots=(rpt-global-first rpt-global-second)
  else
    roots=(rpt-global-second rpt-global-first)
  fi
  # Different APT versions can select different heuristics for a
  # previous root's already-committed dependency alternative. This is
  # a known parity gap: compare exactly if APT resolves; otherwise
  # require a recognizable resolver refusal and validate RPT's closure.
  apt_resolved=true
  if ! apt-get "${opts[@]}" -s -y install "${roots[@]}" \
    >"$root/apt-global-backtrack" 2>"$root/apt-global-backtrack.err"; then
    apt_resolved=false
    if ! grep -Eq 'pkgProblemResolver|Unable to correct problems|unmet dependencies' \
      "$root/apt-global-backtrack.err" "$root/apt-global-backtrack"; then
      cat "$root/apt-global-backtrack.err" >&2
      echo "APT failed unexpectedly (not a dependency resolver refusal)" >&2
      exit 1
    fi
  fi
  ./target/debug/rpt plan-install-batch-policy \
    --cache "$root/rpt-cache" --status "$root/apt/status" \
    --apt-root "$root/rpt-apt" --architecture "$arch" \
    "${roots[@]}" >"$root/rpt-global-backtrack"
  sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-global-backtrack" \
    | LC_ALL=C sort -u >"$root/apt-global-versions"
  sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-global-backtrack" \
    | LC_ALL=C sort -u >"$root/rpt-global-versions"
  if [[ "$apt_resolved" == true ]]; then
    test -s "$root/apt-global-versions"
    diff -u "$root/apt-global-versions" "$root/rpt-global-versions"
  else
    echo "KNOWN APT/RPT DIVERGENCE: $(apt-get --version | head -n1) declined global backtracking for $ordering" >&2
  fi
  for pkg in rpt-global-first rpt-global-second rpt-global-fallback rpt-global-blocker; do
    grep -Fxq "$pkg 1.0" "$root/rpt-global-versions"
  done
  if grep -Fq 'rpt-global-preferred ' "$root/rpt-global-versions"; then
    echo "RPT retained an earlier conflicting dependency branch" >&2; exit 1
  fi
done
echo "PASS RPT transitive global backtracking (APT matched where accepted; known solver divergence reported)"


# When several versioned virtual providers exist, a separate explicitly
# requested package may make all but one infeasible. Compare the exact signed
# candidate/version set with APT for both input orders. Ambiguous viable
# providers outside this forced-unique case remain a separate parity gap.
for ordering in app-first blocker-first; do
  if [[ "$ordering" == app-first ]]; then
    roots=(rpt-multi-virtual-app rpt-multi-virtual-blocker)
  else
    roots=(rpt-multi-virtual-blocker rpt-multi-virtual-app)
  fi
  apt-get "${opts[@]}" -s -y install "${roots[@]}" \
    >"$root/apt-multi-provider"
  ./target/debug/rpt plan-install-batch-policy \
    --cache "$root/rpt-cache" --status "$root/apt/status" \
    --apt-root "$root/rpt-apt" --architecture "$arch" \
    "${roots[@]}" >"$root/rpt-multi-provider"
  sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-multi-provider" \
    | LC_ALL=C sort -u >"$root/apt-multi-provider-versions"
  sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-multi-provider" \
    | LC_ALL=C sort -u >"$root/rpt-multi-provider-versions"
  test -s "$root/apt-multi-provider-versions"
  diff -u "$root/apt-multi-provider-versions" "$root/rpt-multi-provider-versions"
  for pkg in rpt-multi-virtual-app rpt-multi-virtual-blocker rpt-multi-virtual-b; do
    grep -Fxq "$pkg 1.0" "$root/apt-multi-provider-versions"
  done
  if grep -Fq "rpt-multi-virtual-a " "$root/apt-multi-provider-versions"; then
    echo "APT chose a provider incompatible with the requested blocker" >&2
    exit 1
  fi
done
echo "PASS signed APT/RPT forced-unique virtual provider with joint roots (both orders)"

# Candidate policy must be applied BEFORE virtual-provider ambiguity checks.
# With no explicit blocker, multiple repository providers exist, but pinning
# provider A to -1 leaves only B installable. Compare actual APT and RPT
# binary/version selections on the same signed metadata.
mkdir -p "$root/rpt-apt/preferences.d" "$root/apt/etc/preferences.d"
cat >"$root/rpt-apt/preferences" <<'PREF'
Package: rpt-multi-virtual-a
Pin: version 1.0
Pin-Priority: -1

PREF
pin_opts=("${opts[@]}"
  -o "Dir::Etc::preferences=$root/rpt-apt/preferences"
  -o "Dir::Etc::preferencesparts=$root/apt/etc/preferences.d")
apt-get "${pin_opts[@]}" -s -y install rpt-multi-virtual-app \
  >"$root/apt-pinned-virtual"
./target/debug/rpt plan-install-batch-policy \
  --cache "$root/rpt-cache" --status "$root/apt/status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" \
  rpt-multi-virtual-app >"$root/rpt-pinned-virtual"
sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-pinned-virtual" \
  | LC_ALL=C sort -u >"$root/apt-pinned-virtual-versions"
sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-pinned-virtual" \
  | LC_ALL=C sort -u >"$root/rpt-pinned-virtual-versions"
test -s "$root/apt-pinned-virtual-versions"
diff -u "$root/apt-pinned-virtual-versions" "$root/rpt-pinned-virtual-versions"
grep -Fxq 'rpt-multi-virtual-b 1.0' "$root/rpt-pinned-virtual-versions"
if grep -Fq 'rpt-multi-virtual-a ' "$root/rpt-pinned-virtual-versions"; then
  echo "RPT selected a virtual provider forbidden by APT pin -1" >&2
  exit 1
fi
cat >>"$root/rpt-apt/preferences" <<'PREF'
Package: rpt-multi-virtual-b
Pin: version 1.0
Pin-Priority: -1

PREF
if apt-get "${pin_opts[@]}" -s -y install rpt-multi-virtual-app \
  >"$root/apt-double-pinned-virtual" 2>&1; then
  echo "APT unexpectedly selected a provider when both are pinned out" >&2
  exit 1
fi
if ./target/debug/rpt plan-install-batch-policy \
  --cache "$root/rpt-cache" --status "$root/apt/status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" \
  rpt-multi-virtual-app >"$root/rpt-double-pinned-virtual" 2>&1; then
  echo "RPT selected a provider when both are pinned out" >&2
  exit 1
fi
rm "$root/rpt-apt/preferences"
echo "PASS signed APT/RPT pinned virtual providers (-1, both forbidden)"



# The installed dpkg provider is intentionally ABSENT from the signed index.
# Its actual dpkg Status/Provides satisfies a versioned virtual dependency:
# RPT must neither require a repo candidate for that already installed
# provider nor schedule an unrelated package download.
cat >"$root/installed-virtual-status" <<'STATUS'
Package: rpt-local-provider
Status: install ok installed
Priority: optional
Section: misc
Architecture: all
Version: 9.0
Provides: rpt-local-virtual-service (= 2)
Description: locally installed virtual provider, not a repository candidate

STATUS
provider_opts=("${opts[@]}" -o "Dir::State::status=$root/installed-virtual-status")
apt-get "${provider_opts[@]}" -s -y install rpt-installed-virtual-app \
  >"$root/apt-installed-virtual"
./target/debug/rpt plan-install-batch-policy \
  --cache "$root/rpt-cache" --status "$root/installed-virtual-status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" \
  rpt-installed-virtual-app >"$root/rpt-installed-virtual"
sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-installed-virtual" \
  | LC_ALL=C sort -u >"$root/apt-installed-virtual-versions"
sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-installed-virtual" \
  | LC_ALL=C sort -u >"$root/rpt-installed-virtual-versions"
grep -Fxq 'rpt-installed-virtual-app 1.0' "$root/apt-installed-virtual-versions"
diff -u "$root/apt-installed-virtual-versions" "$root/rpt-installed-virtual-versions"
# The same installed provider must NOT satisfy an out-of-range dependency.
if apt-get "${provider_opts[@]}" -s -y install rpt-installed-virtual-bad \
  >"$root/apt-installed-virtual-rejected" 2>&1; then
  echo "APT unexpectedly accepted out-of-range installed virtual Provides" >&2; exit 1
fi
if ./target/debug/rpt plan-install-batch-policy \
  --cache "$root/rpt-cache" --status "$root/installed-virtual-status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" \
  rpt-installed-virtual-bad >"$root/rpt-installed-virtual-rejected" 2>&1; then
  echo "RPT unexpectedly accepted out-of-range installed virtual Provides" >&2; exit 1
fi
echo "PASS signed APT/RPT installed versioned virtual provider and negative bound"



# Compare APT Install-Recommends=true/false with an explicit RPT preview
# mode. Both must select the same names and versions from signed metadata.
apt-get "${opts[@]}" -s -y -o APT::Install-Recommends=true \
  install rpt-soft-app >"$root/apt-soft-on"
./target/debug/rpt plan-install-batch-policy \
  --cache "$root/rpt-cache" --status "$root/apt/status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" \
  --include-recommends rpt-soft-app >"$root/rpt-soft-on"
sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-soft-on" \
  | LC_ALL=C sort -u >"$root/apt-soft-on-versions"
sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-soft-on" \
  | LC_ALL=C sort -u >"$root/rpt-soft-on-versions"
diff -u "$root/apt-soft-on-versions" "$root/rpt-soft-on-versions"
grep -Fxq 'rpt-soft-extra 1.0' "$root/rpt-soft-on-versions"

apt-get "${opts[@]}" -s -y -o APT::Install-Recommends=false \
  install rpt-soft-app >"$root/apt-soft-off"
./target/debug/rpt plan-install-batch-policy \
  --cache "$root/rpt-cache" --status "$root/apt/status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" \
  rpt-soft-app >"$root/rpt-soft-off"
sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-soft-off" \
  | LC_ALL=C sort -u >"$root/apt-soft-off-versions"
sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-soft-off" \
  | LC_ALL=C sort -u >"$root/rpt-soft-off-versions"
diff -u "$root/apt-soft-off-versions" "$root/rpt-soft-off-versions"
if grep -Fq 'rpt-soft-extra' "$root/rpt-soft-off-versions"; then
  echo 'RPT included a disabled Recommends edge' >&2; exit 1
fi
echo "PASS signed APT/RPT Install-Recommends true/false selected binary/version parity"

# Exact negative APT pin: optional Recommends must disappear without aborting
# the mandatory install. Pin files are scoped to this disposable fixture.
mkdir -p "$root/rpt-apt/preferences.d" "$root/apt/etc/preferences.d"
cat > "$root/rpt-apt/preferences" <<'PREF'
Package: rpt-soft-extra
Pin: version 1.0
Pin-Priority: -1

PREF
apt-get "${opts[@]}" -o "Dir::Etc::preferences=$root/rpt-apt/preferences" \
  -o "Dir::Etc::preferencesparts=$root/apt/etc/preferences.d" \
  -o APT::Install-Recommends=true -s -y install rpt-soft-app >"$root/apt-soft-pinned"
./target/debug/rpt plan-install-batch-policy \
  --cache "$root/rpt-cache" --status "$root/apt/status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" \
  --include-recommends rpt-soft-app >"$root/rpt-soft-pinned"
sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-soft-pinned" \
  | LC_ALL=C sort -u >"$root/apt-soft-pinned-versions"
sed -nE 's/^  ([^ :]+):[^ ]+ ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-soft-pinned" \
  | LC_ALL=C sort -u >"$root/rpt-soft-pinned-versions"
diff -u "$root/apt-soft-pinned-versions" "$root/rpt-soft-pinned-versions"
grep -Fxq 'rpt-soft-app 1.0' "$root/rpt-soft-pinned-versions"
if grep -Fq 'rpt-soft-extra' "$root/rpt-soft-pinned-versions"; then
  echo "An optional candidate prohibited by APT pins was still planned" >&2
  exit 1
fi
rm "$root/rpt-apt/preferences"

# A held installed binary must not silently upgrade via explicit installation
# without override. APT's -y mode refuses such operations; RPT has no override.
cat > "$root/held-status" <<EOF
Package: rpt-soft-app
Status: hold ok installed
Architecture: all
Version: 0.9
Description: held installed package

EOF
if apt-get "${opts[@]}" -o "Dir::State::status=$root/held-status" \
  -o "Dir::Etc::preferences=$root/rpt-apt/preferences" \
  -o "Dir::Etc::preferencesparts=$root/apt/etc/preferences.d" \
  -o APT::Install-Recommends=false -s -y install rpt-soft-app \
  >"$root/apt-held.log" 2>&1; then
  echo "APT unexpectedly upgraded an explicitly held package without override" >&2
  cat "$root/apt-held.log" >&2
  exit 1
fi
if ./target/debug/rpt plan-install-batch-policy \
  --cache "$root/rpt-cache" --status "$root/held-status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" rpt-soft-app \
  >"$root/rpt-held.log" 2>&1; then
  echo "RPT unexpectedly upgraded an explicitly held package without override" >&2
  exit 1
fi
grep -Fq 'held package' "$root/rpt-held.log"
echo "PASS signed APT/RPT negative optional pin and explicitly held package refusal"



# Upgrade milestone: add two real packages to signed repository after
# all original APT/RPT differential fixtures have completed.
mkdir -p "$root/helper-v1/DEBIAN" "$root/demo-v2/DEBIAN"
cat > "$root/helper-v1/DEBIAN/control" <<'CONTROL'
Package: rpt-sandbox-helper
Version: 1.0
Architecture: all
Maintainer: RPT CI <rpt-test@example.invalid>
Description: helper for predependency upgrade
CONTROL
cat > "$root/demo-v2/DEBIAN/control" <<'CONTROL'
Package: rpt-sandbox-demo
Version: 2.0
Architecture: all
Pre-Depends: rpt-sandbox-helper (>= 1.0)
Maintainer: RPT CI <rpt-test@example.invalid>
Description: real dpkg upgrade with predependency
CONTROL
mkdir -p "$root/helper-v1/usr/share/rpt-test" "$root/demo-v2/usr/share/rpt-test"
printf 'helper 1\n' > "$root/helper-v1/usr/share/rpt-test/helper.txt"
printf 'demo 2\n' > "$root/demo-v2/usr/share/rpt-test/installed.txt"
# Deliberately keep the upgrade transaction open for cross-process locking tests.
cat > "$root/demo-v2/DEBIAN/postinst" <<'POSTINST'
#!/bin/sh
set -e
touch /tmp/rpt-upgrade-postinst-running
sleep 4
POSTINST
chmod 0755 "$root/demo-v2/DEBIAN/postinst"
dpkg-deb --build --root-owner-group "$root/helper-v1" \
  "$root/www/pool/rpt-sandbox-helper_1.0_all.deb" >/dev/null
dpkg-deb --build --root-owner-group "$root/demo-v2" \
  "$root/www/pool/rpt-sandbox-demo_2.0_all.deb" >/dev/null
for spec in rpt-sandbox-helper:1.0 rpt-sandbox-demo:2.0; do
  name="$(printf '%s' "$spec" | cut -d: -f1)"
  ver="$(printf '%s' "$spec" | cut -d: -f2)"
  file="pool/$name"_"$ver"_all.deb
  checksum="$(sha256sum "$root/www/$file" | cut -d' ' -f1)"
  length="$(wc -c < "$root/www/$file" | tr -d ' ')"
  printf 'Package: %s\nVersion: %s\nArchitecture: all\nFilename: %s\nSHA256: %s\nSize: %s\n' \
    "$name" "$ver" "$file" "$checksum" "$length" >>"$index"
  if [[ "$name" == "rpt-sandbox-demo" ]]; then
    printf 'Pre-Depends: rpt-sandbox-helper (>= 1.0)\n' >>"$index"
  fi
  printf 'Description: signed upgrade artifact\n\n' >>"$index"
done
hash="$(sha256sum "$index" | cut -d' ' -f1)"
size="$(wc -c < "$index" | tr -d ' ')"
cat > "$release" <<EOF
Origin: RPT Differential
Label: RPT Differential
Suite: fixture
Codename: fixture
Date: $(LC_ALL=C date -Ru)
Architectures: $arch
Components: main
SHA256:
 $hash $size main/binary-$arch/Packages
EOF
gpg --homedir "$root/gnupg" --batch --yes --pinentry-mode loopback --passphrase '' \
  --clearsign --digest-algo SHA256 --output "$root/www/dists/fixture/InRelease" "$release"
./target/debug/rpt update --root "$root/rpt-apt" --output "$root/rpt-cache-v2" \
  --no-progress >"$root/rpt-v2-update.log"

# Major integrated M3 acceptance: from the *installed* v1 guest,
# auto-discover a signed v2 candidate, resolve its new Pre-Depends, construct
# a status-bound intent, download artifacts, and invoke the real native guest
# dpkg executor. Never mount or execute on the CI runner's host rootfs.
auto_upgrade_guest="$root/native-upgrade-guest"
sudo mkdir -p "$auto_upgrade_guest"
auto_upgrade_container="rpt-native-auto-export-$(basename "$root")"
docker create --name "$auto_upgrade_container" --network=none \
  --entrypoint /bin/sh "$sandbox_image" -c true >/dev/null
docker export "$auto_upgrade_container" | sudo tar -xpf - -C "$auto_upgrade_guest"
docker rm "$auto_upgrade_container" >/dev/null
printf 'rpt-disposable-rootfs-v1\n' \
  | sudo tee "$auto_upgrade_guest/.rpt-disposable-rootfs" >/dev/null
sudo dpkg --root="$auto_upgrade_guest" --install \
  "$root/www/pool/rpt-sandbox-demo_1.0_all.deb"
if ./target/debug/rpt native-rootfs-prepare-system-upgrade --rootfs / \
    --cache "$root/rpt-cache-v2" --apt-root "$root/rpt-apt" \
    --output "$root/native-unsafe-system-upgrade.json" >/dev/null 2>&1; then
  echo 'Native system-upgrade intent unexpectedly accepted live /' >&2; exit 1
fi
./target/debug/rpt native-rootfs-prepare-system-upgrade \
  --rootfs "$auto_upgrade_guest" --cache "$root/rpt-cache-v2" \
  --apt-root "$root/rpt-apt" --architecture "$arch" \
  --max-roots 128 --max-packages 200 \
  --output "$root/native-auto-upgrade-intent.json"
python3 - "$root/native-auto-upgrade-intent.json" "$auto_upgrade_guest" <<'PY'
import json,sys
intent=json.load(open(sys.argv[1]))
assert intent['session']=='native-rootfs:'+sys.argv[2]
assert intent['requested']==['rpt-sandbox-demo']
assert not intent['executable']
assert [(s['package'],s['version'],s['installed']) for s in intent['steps']]==[
    ('rpt-sandbox-helper','1.0',None),
    ('rpt-sandbox-demo','2.0','1.0')]
assert [(p['name'],p['version']) for p in intent['packages']]==[
    ('rpt-sandbox-helper','1.0'),('rpt-sandbox-demo','2.0')]
PY
./target/debug/rpt fetch --manifest "$root/native-auto-upgrade-intent.json" \
  --output "$root/native-auto-artifacts" --workers 2 --segments 1 \
  >"$root/native-auto-fetch.log"
sudo ./target/debug/rpt native-rootfs-apply-transaction \
  --rootfs "$auto_upgrade_guest" --id native-auto-upgrade \
  --transaction "$root/native-auto-upgrade-intent.json" \
  --artifacts "$root/native-auto-artifacts" \
  --cache "$root/rpt-cache-v2" --apt-root "$root/rpt-apt" \
  --journal-dir "$root/native-auto-journal" --timeout-seconds 180 \
  >"$root/native-auto-upgrade.log"
grep -Fq '"phase": "succeeded"' \
  "$root/native-auto-journal/native-auto-upgrade.json"
test "$(sudo dpkg-query --admindir="$auto_upgrade_guest/var/lib/dpkg" \
  -W '-f=${Status}|${Version}' rpt-sandbox-demo)" = 'install ok installed|2.0'
test "$(sudo dpkg-query --admindir="$auto_upgrade_guest/var/lib/dpkg" \
  -W '-f=${Status}|${Version}' rpt-sandbox-helper)" = 'install ok installed|1.0'
test -z "$(sudo dpkg --root="$auto_upgrade_guest" --audit)"
# An already upgraded guest has no eligible newer candidates: refuse to
# create a misleading empty auto-upgrade transaction for dpkg to execute.
if ./target/debug/rpt native-rootfs-prepare-system-upgrade \
    --rootfs "$auto_upgrade_guest" --cache "$root/rpt-cache-v2" \
    --apt-root "$root/rpt-apt" --architecture "$arch" \
    --output "$root/native-auto-noop.json" >"$root/native-auto-noop.log" 2>&1; then
  echo 'Native auto-upgrade created a no-op execution intent' >&2; exit 1
fi
echo 'PASS signed native-rootfs auto-upgrade discovery, dependency closure, real dpkg, replay and live-root refusal'

# Seed v1 in a new persistent sandbox; reject an intent created before seeding.
./target/debug/rpt sandbox-session-create --session "$session" --image "$sandbox_image"
./target/debug/rpt sandbox-session-prepare-transaction \
  --session "$session" --cache "$root/rpt-cache" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/bound-v1.json" rpt-sandbox-demo
./target/debug/rpt sandbox-session-install \
  --session "$session" --id upgrade-seed-v1 \
  --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --journal-dir "$root/upgrade-journal" \
  --architecture "$arch" --timeout-seconds 180
if ./target/debug/rpt sandbox-session-apply-transaction \
  --session "$session" --id stale-v1 --transaction "$root/bound-v1.json" \
  --artifacts "$root/sandbox-artifacts" --cache "$root/rpt-cache" \
  --apt-root "$root/rpt-apt" --journal-dir "$root/upgrade-journal" \
  --timeout-seconds 180 >"$root/stale-out.log" 2>"$root/stale-err.log"; then
  echo "Stale dpkg status intent was accepted" >&2; exit 1
fi
grep -Fq 'stale sandbox transaction' "$root/stale-err.log"

./target/debug/rpt sandbox-session-prepare-transaction \
  --session "$session" --cache "$root/rpt-cache-v2" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/bound-upgrade.json" rpt-sandbox-demo
python3 - "$root/bound-upgrade.json" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['schema_version']==1 and x['session']=='ci-test' and not x['executable']
assert [(p['name'],p['version']) for p in x['packages']] == [
  ('rpt-sandbox-helper','1.0'),('rpt-sandbox-demo','2.0')]
assert [(p['package'],p['installed'],p['version']) for p in x['steps']] == [
  ('rpt-sandbox-helper',None,'1.0'),('rpt-sandbox-demo','1.0','2.0')]
PY
./target/debug/rpt fetch --manifest "$root/bound-upgrade.json" \
  --output "$root/upgrade-artifacts" --workers 2 --segments 1 >"$root/upgrade-fetch.log"
./target/debug/rpt sandbox-session-apply-transaction \
  --session "$session" --id upgrade-v2 --transaction "$root/bound-upgrade.json" \
  --artifacts "$root/upgrade-artifacts" --cache "$root/rpt-cache-v2" \
  --apt-root "$root/rpt-apt" --journal-dir "$root/upgrade-journal" --timeout-seconds 180 \
  >"$root/upgrade-run.log" 2>&1 &
upgrade_pid="$!"
for _ in {1..100}; do
  if grep -Fq '"phase": "running"' "$root/upgrade-journal/upgrade-v2.json" 2>/dev/null; then break; fi
  sleep .05
done
if ! grep -Fq '"phase": "running"' "$root/upgrade-journal/upgrade-v2.json"; then
  cat "$root/upgrade-run.log" >&2
  echo "Upgrade never acquired the transaction journal" >&2; exit 1
fi
# Even with a DIFFERENT journal directory, a second RPT process must not
# run dpkg in the same container while upgrade-v2 owns its session lease.
if ./target/debug/rpt sandbox-session-remove \
  --session "$session" --id concurrent-delete --journal-dir "$root/other-journal" \
  --timeout-seconds 120 rpt-sandbox-demo >"$root/parallel-out.log" 2>"$root/parallel-err.log"; then
  echo "Concurrent sandbox mutation wrongly acquired the session lock" >&2; exit 1
fi
grep -Fq 'another RPT operation owns this Docker sandbox session' "$root/parallel-err.log"
wait "$upgrade_pid"
upgrade_pid=""
grep -Fq '"phase": "succeeded"' "$root/upgrade-journal/upgrade-v2.json"
test "$(docker exec "rpt-session-$session" dpkg-query -W -f='${Version}' rpt-sandbox-demo)" = "2.0"
test "$(docker exec "rpt-session-$session" dpkg-query -W -f='${Status}' rpt-sandbox-helper)" = "install ok installed"
if ./target/debug/rpt sandbox-session-apply-transaction \
  --session "$session" --id upgrade-replay --transaction "$root/bound-upgrade.json" \
  --artifacts "$root/upgrade-artifacts" --cache "$root/rpt-cache-v2" \
  --apt-root "$root/rpt-apt" --journal-dir "$root/upgrade-journal" \
  --timeout-seconds 180 >"$root/replay-out.log" 2>"$root/replay-err.log"; then
  echo "Replayed upgrade intent was accepted" >&2; exit 1
fi
grep -Fq 'stale sandbox transaction' "$root/replay-err.log"
./target/debug/rpt sandbox-session-destroy --session "$session"
echo "PASS authenticated bound install+upgrade 1.0 to 2.0 and stale/replay refusal"

# PR #130: crash an actual dpkg upgrade (SIGKILL) while a maintainer script
# is running. RPT's automatic pre-transaction snapshot must remain usable,
# the unfinished journal must remain Running and the original container
# must be preserved for forensic inspection.
./target/debug/rpt sandbox-session-create \
  --session crash-ci --image "$sandbox_image"
./target/debug/rpt sandbox-session-install \
  --session crash-ci --id crash-seed-v1 \
  --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --journal-dir "$root/crash-seed-journal" \
  --architecture "$arch" --timeout-seconds 180
./target/debug/rpt sandbox-session-prepare-transaction \
  --session crash-ci --cache "$root/rpt-cache-v2" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/crash-bound-v2.json" rpt-sandbox-demo
./target/debug/rpt sandbox-session-apply-transaction \
  --session crash-ci --id crash-upgrade --transaction "$root/crash-bound-v2.json" \
  --artifacts "$root/upgrade-artifacts" --cache "$root/rpt-cache-v2" \
  --apt-root "$root/rpt-apt" --journal-dir "$root/crash-journal" \
  --timeout-seconds 180 >"$root/crash-run.log" 2>&1 &
crash_pid="$!"
# Wait for the real dpkg postinst marker, not just journal phase Running.
postinst_started=0
for _ in {1..200}; do
  if docker exec rpt-session-crash-ci test -e /tmp/rpt-upgrade-postinst-running >/dev/null 2>&1; then
    postinst_started=1
    break
  fi
  if ! kill -0 "$crash_pid" 2>/dev/null; then break; fi
  sleep .05
done
if [[ "$postinst_started" != 1 ]]; then
  cat "$root/crash-run.log" >&2
  echo "dpkg postinst did not start before SIGKILL fixture" >&2
  exit 1
fi
kill -9 "$crash_pid" 2>/dev/null || :
wait "$crash_pid" 2>/dev/null || :
crash_pid=""
docker stop --time 0 rpt-session-crash-ci >/dev/null
receipt="$root/crash-journal/sandbox-snapshots/crash-ci--crash-upgrade.json"
test -s "$receipt"
grep -Fq '"phase": "running"' "$root/crash-journal/crash-upgrade.json"
./target/debug/rpt sandbox-snapshot-verify \
  --receipt "$receipt" >"$root/crash-snapshot.json"
python3 - "$root/crash-snapshot.json" <<'PY'
import json,sys
x=json.load(open(sys.argv[1]))
assert x['source_session']=='crash-ci' and x['transaction_id']=='crash-upgrade'
assert len(x['status_sha256']) == 64 and x['image_id'].startswith('sha256:')
PY
if ./target/debug/rpt sandbox-snapshot-restore \
  --receipt "$receipt" --new-session crash-ci >/dev/null 2>&1; then
  echo "Unsafe restoration over original sandbox unexpectedly allowed" >&2
  exit 1
fi
./target/debug/rpt sandbox-snapshot-restore \
  --receipt "$receipt" --new-session restored-ci
test "$(docker exec rpt-session-restored-ci dpkg-query -W -f='${Version}' rpt-sandbox-demo)" = "1.0"
if docker exec rpt-session-restored-ci dpkg-query -W rpt-sandbox-helper >/dev/null 2>&1; then
  echo "Restored snapshot wrongly includes post-crash dependency" >&2
  exit 1
fi
test -z "$(docker exec rpt-session-restored-ci dpkg --audit)"
docker container inspect rpt-session-crash-ci >/dev/null
grep -Fq '"phase": "running"' "$root/crash-journal/crash-upgrade.json"
# The crashed journal is not automatically cleared by restore.
if ./target/debug/rpt sandbox-session-remove \
  --session restored-ci --id blocked-after-crash \
  --journal-dir "$root/crash-journal" rpt-sandbox-demo \
  >"$root/blocked-out.log" 2>"$root/blocked-err.log"; then
  echo "Unfinished journal gate was bypassed by restoration" >&2
  exit 1
fi
grep -Fq 'unfinished transaction' "$root/blocked-err.log"
./target/debug/rpt sandbox-session-destroy --session restored-ci
docker rm -f rpt-session-crash-ci >/dev/null
echo "PASS SIGKILL dpkg recovery: image receipt, clean separate restore, forensic journal preserved"

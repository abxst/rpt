#!/usr/bin/env bash
# Unprivileged signed APT/RPT amd64+i386 differential, no host dpkg writes.
set -euo pipefail
rpt="${1:?RPT executable required}"
test "$(dpkg --print-architecture)" = amd64
root="$(mktemp -d)"
server_pid=""
cleanup() {
  if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || :; wait "$server_pid" 2>/dev/null || :; fi
  rm -rf "$root"
}
trap cleanup EXIT
mkdir -p "$root/www/dists/fixture/main/binary-amd64" "$root/www/dists/fixture/main/binary-i386" \
  "$root/gnupg" "$root/apt/lists/partial" "$root/apt/cache/archives/partial" \
  "$root/apt/etc/empty" "$root/rpt-apt/sources.list.d" "$root/cache"
chmod 700 "$root/gnupg"
cat > "$root/www/dists/fixture/main/binary-amd64/Packages" <<'EOF'
Package: rpt-pair
Version: 1.0
Architecture: amd64
Multi-Arch: same
Filename: pool/rpt-pair_1.0_amd64.deb
Size: 1
Description: native library pair

Package: rpt-common-data
Version: 1.0
Architecture: all
Multi-Arch: foreign
Filename: pool/rpt-common-data_1.0_all.deb
Size: 1
Description: architecture independent dependency of i386 binary

EOF
cat > "$root/www/dists/fixture/main/binary-i386/Packages" <<'EOF'
Package: rpt-foreign-app
Version: 1.0
Architecture: i386
Depends: rpt-foreign-lib (>= 1.0), rpt-common-data (>= 1.0)
Filename: pool/rpt-foreign-app_1.0_i386.deb
Size: 1
Description: foreign application

Package: rpt-foreign-lib
Version: 1.0
Architecture: i386
Filename: pool/rpt-foreign-lib_1.0_i386.deb
Size: 1
Description: foreign dependency

Package: rpt-host-tool-user
Version: 1.0
Architecture: i386
Depends: rpt-host-tool (>= 2.0)
Filename: pool/rpt-host-tool-user_1.0_i386.deb
Size: 1
Description: foreign app satisfied by installed native foreign tool

Package: rpt-host-interpreter-user
Version: 1.0
Architecture: i386
Depends: rpt-host-interpreter:any (>= 2.0)
Filename: pool/rpt-host-interpreter-user_1.0_i386.deb
Size: 1
Description: foreign app satisfied by installed native allowed interpreter

Package: rpt-pair
Version: 1.0
Architecture: i386
Multi-Arch: same
Filename: pool/rpt-pair_1.0_i386.deb
Size: 1
Description: foreign library pair

Package: rpt-common-data
Version: 1.0
Architecture: all
Multi-Arch: foreign
Filename: pool/rpt-common-data_1.0_all.deb
Size: 1
Description: architecture independent dependency of i386 binary

EOF
payload_hash="$(printf x | sha256sum | cut -d' ' -f1)"
for arch in amd64 i386; do
  sed -i "/^Size: 1$/a SHA256: $payload_hash" \
    "$root/www/dists/fixture/main/binary-$arch/Packages"
done
gpg --homedir "$root/gnupg" --batch --pinentry-mode loopback --passphrase '' \
  --quick-generate-key 'RPT MultiArch CI <rpt-multiarch@example.invalid>' ed25519 sign 0 >/dev/null 2>&1
gpg --homedir "$root/gnupg" --batch --export \
  'RPT MultiArch CI <rpt-multiarch@example.invalid>' >"$root/keyring.gpg"
release="$root/www/dists/fixture/Release"
cat > "$release" <<EOF
Origin: RPT Multiarch
Label: RPT Multiarch
Suite: fixture
Codename: fixture
Date: $(LC_ALL=C date -Ru)
Architectures: amd64 i386
Components: main
SHA256:
EOF
for arch in amd64 i386; do
  idx="$root/www/dists/fixture/main/binary-$arch/Packages"
  printf ' %s %s main/binary-%s/Packages\n' \
    "$(sha256sum "$idx" | cut -d' ' -f1)" "$(wc -c <"$idx" | tr -d ' ')" "$arch" >>"$release"
done
gpg --homedir "$root/gnupg" --batch --yes --pinentry-mode loopback --passphrase '' \
  --clearsign --digest-algo SHA256 --output "$root/www/dists/fixture/InRelease" "$release"
gpgv --keyring "$root/keyring.gpg" "$root/www/dists/fixture/InRelease" >/dev/null 2>&1
python3 - "$root" <<'PY'
import socket,sys
with socket.socket() as s:
    s.bind(('127.0.0.1',0))
    open(sys.argv[1]+'/port','w').write(str(s.getsockname()[1]))
PY
port="$(cat "$root/port")"
python3 -m http.server "$port" --bind 127.0.0.1 --directory "$root/www" >"$root/http.log" 2>&1 &
server_pid="$!"
for _ in {1..60}; do
  if python3 - "$port" <<'PY'
import sys,urllib.request
try: urllib.request.urlopen('http://127.0.0.1:'+sys.argv[1]+'/dists/fixture/InRelease',timeout=.3)
except Exception: sys.exit(1)
PY
  then break; fi
  sleep .1
done
uri="http://127.0.0.1:$port"
printf 'deb [signed-by=%s] %s fixture main\n' "$root/keyring.gpg" "$uri" >"$root/apt/etc/sources.list"
cp "$root/apt/etc/sources.list" "$root/rpt-apt/sources.list"
: >"$root/empty-status"
cat >"$root/native-installed" <<'EOF'
Package: rpt-pair
Status: install ok installed
Architecture: amd64
Version: 1.0
Multi-Arch: same
Description: native installed library

EOF
opts=(
  -o "Dir::State::lists=$root/apt/lists"
  -o "Dir::Cache=$root/apt/cache"
  -o "Dir::Etc::sourcelist=$root/apt/etc/sources.list"
  -o "Dir::Etc::sourceparts=$root/apt/etc/empty"
  -o "APT::Architectures::=i386"
  -o "APT::Get::List-Cleanup=0"
  -o "Debug::NoLocking=1"
)
apt-get "${opts[@]}" -o "Dir::State::status=$root/empty-status" update >"$root/apt-update.log"
"$rpt" update --root "$root/rpt-apt" --output "$root/cache" \
  --additional-architecture i386 --no-progress >"$root/rpt-update.log"
"$rpt" cache-versions --cache "$root/cache" --architecture i386 \
  --package rpt-foreign-app | grep -Fxq '1.0'

compare_plan() {
  local label="$1" status="$2" package="$3"
  apt-get "${opts[@]}" -o "Dir::State::status=$status" \
    -o APT::Install-Recommends=false -s -y install "$package" >"$root/apt-$label"
  "$rpt" plan-install-foreign-policy --cache "$root/cache" \
    --status "$status" --apt-root "$root/rpt-apt" \
    --native-architecture amd64 --foreign-architecture i386 \
    "$package" >"$root/rpt-$label"
  sed -nE 's/^Inst ([^ ]+) \(([^ )]+).*/\1 \2/p' "$root/apt-$label" | LC_ALL=C sort -u \
    >"$root/apt-$label-selected"
  sed -nE 's/^  ([^ ]+) ([^ ]+) \(installed: .*\)$/\1 \2/p' "$root/rpt-$label" |
    sed -E 's/:all ([^ ]+)$/ \1/' | LC_ALL=C sort -u \
    >"$root/rpt-$label-selected"
  test -s "$root/rpt-$label-selected"
  diff -u "$root/apt-$label-selected" "$root/rpt-$label-selected"
}
compare_plan foreign "$root/empty-status" rpt-foreign-app:i386
grep -Fxq 'rpt-foreign-app:i386 1.0' "$root/rpt-foreign-selected"
grep -Fxq 'rpt-foreign-lib:i386 1.0' "$root/rpt-foreign-selected"
grep -Fxq 'rpt-common-data 1.0' "$root/rpt-foreign-selected"
grep -Fq 'rpt-common-data:all 1.0' "$root/rpt-foreign"
compare_plan pair "$root/native-installed" rpt-pair:i386
grep -Fxq 'rpt-pair:i386 1.0' "$root/rpt-pair-selected"
cat >"$root/native-cross-installed" <<'EOF'
Package: rpt-host-tool
Status: install ok installed
Architecture: amd64
Version: 2.0
Multi-Arch: foreign
Description: native tool already installed

Package: rpt-host-interpreter
Status: install ok installed
Architecture: amd64
Version: 2.0
Multi-Arch: allowed
Description: native interpreter already installed

EOF
compare_plan host-tool "$root/native-cross-installed" rpt-host-tool-user:i386
compare_plan host-any "$root/native-cross-installed" rpt-host-interpreter-user:i386
grep -Fxq 'rpt-host-tool-user:i386 1.0' "$root/rpt-host-tool-selected"
grep -Fxq 'rpt-host-interpreter-user:i386 1.0' "$root/rpt-host-any-selected"
test "$(wc -l <"$root/rpt-host-tool-selected")" -eq 1
test "$(wc -l <"$root/rpt-host-any-selected")" -eq 1
# Wrong installed Multi-Arch flags must never be treated as proof.
sed 's/Multi-Arch: foreign/Multi-Arch: no/' "$root/native-cross-installed" >"$root/invalid-foreign-status"
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/invalid-foreign-status" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-host-tool-user:i386 >"$root/invalid-foreign.log" 2>&1; then
  echo 'RPT accepted native Multi-Arch: no as a foreign dependency' >&2; exit 1
fi
sed 's/Multi-Arch: allowed/Multi-Arch: no/' "$root/native-cross-installed" >"$root/invalid-any-status"
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/invalid-any-status" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-host-interpreter-user:i386 >"$root/invalid-any.log" 2>&1; then
  echo 'RPT accepted native Multi-Arch: no for :any' >&2; exit 1
fi
sed 's/Version: 1.0/Version: 2.0/' "$root/native-installed" >"$root/mismatch"
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/mismatch" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-pair:i386 >"$root/mismatch.log" 2>&1; then
  echo 'RPT accepted mismatched Multi-Arch: same coinstallation' >&2; exit 1
fi
index="$(find "$root/cache" -type f -name 'index-*.raw' | head -n1)"
test -n "$index"
printf '\n# cache tamper\n' >>"$index"
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/empty-status" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-foreign-app:i386 >"$root/tampered.log" 2>&1; then
  echo 'RPT accepted a tampered signed Multi-Arch index' >&2; exit 1
fi
echo 'PASS authenticated APT/RPT Multi-Arch dependencies, installed native foreign/allowed, coinstall and tamper refusal'

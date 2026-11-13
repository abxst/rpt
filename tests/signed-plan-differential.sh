#!/usr/bin/env bash
# Read-only, same-repository APT/RPT dependency-plan differential.
set -euo pipefail
root="$(mktemp -d)"
server_pid=""
cleanup() {
  if [[ -n "$server_pid" ]]; then kill "$server_pid" 2>/dev/null || :; wait "$server_pid" 2>/dev/null || :; fi
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

PACKAGES
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
for pkg in rpt-app rpt-pre-app; do
  apt-get "${opts[@]}" -s -y install "$pkg" >"$root/apt-plan"
  ./target/debug/rpt plan-install --cache "$root/rpt-cache" --status "$root/apt/status" \
    --apt-root "$root/rpt-apt" --architecture "$arch" "$pkg" >"$root/rpt-plan"
  sed -n 's/^Inst \([^ ]*\) .*/\1/p' "$root/apt-plan" | LC_ALL=C sort -u >"$root/apt-names"
  sed -n 's/^  \([^ ]*\) [^ ]* (installed: .*)$/\1/p' "$root/rpt-plan" | LC_ALL=C sort -u >"$root/rpt-names"
  if ! diff -u "$root/apt-names" "$root/rpt-names"; then
    echo "APT/RPT dependency plan differs for $pkg" >&2
    cat "$root/apt-plan" "$root/rpt-plan" >&2
    exit 1
  fi
done
if apt-get "${opts[@]}" -s -y install rpt-broken >"$root/apt-broken" 2>&1; then
  echo "APT unexpectedly accepted unsatisfiable fixture" >&2; exit 1
fi
if ./target/debug/rpt plan-install --cache "$root/rpt-cache" --status "$root/apt/status" \
  --apt-root "$root/rpt-apt" --architecture "$arch" rpt-broken >"$root/rpt-broken" 2>&1; then
  echo "RPT unexpectedly accepted unsatisfiable fixture" >&2; exit 1
fi
echo "PASS signed APT/RPT plan differential: Depends, Pre-Depends, missing dependency"

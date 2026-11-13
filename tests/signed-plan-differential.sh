#!/usr/bin/env bash
# Read-only, same-repository APT/RPT dependency-plan differential.
set -euo pipefail
root="$(mktemp -d)"
server_pid=""
session="ci-test"
cleanup() {
  docker rm -f "rpt-session-$session" >/dev/null 2>&1 || :
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
docker pull debian:bookworm-slim >/dev/null
./target/debug/rpt prepare-download-plan \
  --cache "$root/rpt-cache" --status "$root/apt/status" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/sandbox-manifest.json" rpt-sandbox-demo
./target/debug/rpt fetch --manifest "$root/sandbox-manifest.json" \
  --output "$root/sandbox-artifacts" --workers 1 --segments 1 >"$root/sandbox-fetch.log"
./target/debug/rpt sandbox-install \
  --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --architecture "$arch" --image debian:bookworm-slim \
  --journal-dir "$root/sandbox-journal" --id sandbox-ci --timeout-seconds 180
grep -Fq '"phase": "succeeded"' "$root/sandbox-journal/sandbox-ci.json"
# Install/remove and install/purge exercise actual dpkg operations in the
# same isolated container; different journals and disposable roots each time.
for action in install-remove install-purge; do
  ./target/debug/rpt sandbox-install \
    --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
    --cache "$root/rpt-cache" --architecture "$arch" --image debian:bookworm-slim \
    --journal-dir "$root/sandbox-journal" --id "sandbox-$action" \
    --timeout-seconds 180 --lifecycle "$action"
  grep -Fq '"phase": "succeeded"' "$root/sandbox-journal/sandbox-$action.json"
done
./target/debug/rpt prepare-download-plan \
  --cache "$root/rpt-cache" --status "$root/apt/status" --apt-root "$root/rpt-apt" \
  --architecture "$arch" --output "$root/mismatch-manifest.json" rpt-lie
if ./target/debug/rpt sandbox-install \
  --manifest "$root/mismatch-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --architecture "$arch" --image debian:bookworm-slim \
  --journal-dir "$root/mismatch-journal" --id sandbox-mismatch \
  --timeout-seconds 180 >"$root/mismatch-stdout.log" 2>"$root/mismatch-stderr.log"; then
  echo "Sandbox unexpectedly accepted signed metadata disagreeing with .deb control" >&2
  exit 1
fi
grep -Fq 'Signed manifest disagrees with Debian control fields' "$root/mismatch-stderr.log"

# A persistent, isolated container keeps its own dpkg database across RPT
# install/remove/purge commands. None of this state is reflected on the host.
./target/debug/rpt sandbox-session-create \
  --session "$session" --image debian:bookworm-slim
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
if ./target/debug/rpt sandbox-session-inspect --session "$session" | grep -Fq rpt-sandbox-demo; then
  echo "Package still present after sandbox removal" >&2
  exit 1
fi
./target/debug/rpt sandbox-session-install \
  --session "$session" --id session-reinstall \
  --manifest "$root/sandbox-manifest.json" --artifacts "$root/sandbox-artifacts" \
  --cache "$root/rpt-cache" --journal-dir "$root/session-journal" \
  --architecture "$arch" --timeout-seconds 180
# Mark our fixture as auto-installed inside the sandbox; the installed
# Debian base packages remain manual roots.
cat > "$root/sandbox-auto-states" <<'EOF'
Package: rpt-sandbox-demo
Architecture: all
Auto-Installed: 1

EOF
docker exec "rpt-session-$session" mkdir -p /var/lib/apt
docker cp "$root/sandbox-auto-states" "rpt-session-$session:/var/lib/apt/extended_states"
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
for pkg in rpt-app rpt-pre-app rpt-alternative rpt-virtual-app rpt-alternative-conflict rpt-alternative-breaks rpt-cycle-a; do
  compare_plan "$pkg"
done
for pkg in rpt-broken rpt-version-mismatch rpt-self-conflict rpt-self-breaks; do
  reject_plan "$pkg"
done
# Conflicts must be rejected when both packages are explicitly requested.
if apt-get "${opts[@]}" -s -y install rpt-app rpt-conflict >"$root/apt-conflict" 2>&1; then
  echo "APT accepted incompatible package pair" >&2; exit 1
fi
# RPT's public CLI currently accepts one root package, so do not pretend
# that its single-root preview validates a multi-root conflict.
echo "PASS signed APT/RPT versioned plan differential: Depends, Pre-Depends, alternatives, Provides, Conflicts, Breaks, missing/version constraints; APT multi-root Conflicts"

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

Package: rpt-new-native-leaf
Version: 2.0
Architecture: amd64
Multi-Arch: foreign
Provides: rpt-upgrade-virtual (= 2.0)
Conflicts: rpt-conflicting-old
Filename: pool/rpt-new-native-leaf_2.0_amd64.deb
Size: 1
Description: new native leaf for foreign unqualified dependency

Package: rpt-new-allowed-leaf
Version: 2.0
Architecture: amd64
Multi-Arch: allowed
Filename: pool/rpt-new-allowed-leaf_2.0_amd64.deb
Size: 1
Description: new native leaf for foreign any dependency

Package: rpt-native-recursive-tool
Version: 2.0
Architecture: amd64
Multi-Arch: foreign
Depends: rpt-native-recursive-helper (>= 3.0)
Filename: pool/rpt-native-recursive-tool_2.0_amd64.deb
Size: 1
Description: native tool depended on by i386 app

Package: rpt-native-recursive-helper
Version: 3.0
Architecture: amd64
Depends: rpt-native-recursive-bottom (= 4.0)
Filename: pool/rpt-native-recursive-helper_3.0_amd64.deb
Size: 1
Description: native-only intermediate helper

Package: rpt-native-recursive-bottom
Version: 4.0
Architecture: amd64
Filename: pool/rpt-native-recursive-bottom_4.0_amd64.deb
Size: 1
Description: native-only bottom dependency

Package: rpt-pre-native-tool
Version: 2.0
Architecture: amd64
Multi-Arch: foreign
Pre-Depends: rpt-pre-native-setup (>= 1.0)
Filename: pool/rpt-pre-native-tool_2.0_amd64.deb
Size: 1
Description: native predepends test tool

Package: rpt-pre-native-setup
Version: 1.0
Architecture: amd64
Filename: pool/rpt-pre-native-setup_1.0_amd64.deb
Size: 1
Description: native setup dependency

Package: rpt-alt-native-tool
Version: 2.0
Architecture: amd64
Multi-Arch: foreign
Depends: rpt-alt-native-bad | rpt-alt-native-good
Filename: pool/rpt-alt-native-tool_2.0_amd64.deb
Size: 1
Description: native tool with ordered alternatives

Package: rpt-alt-native-bad
Version: 1.0
Architecture: amd64
Conflicts: rpt-alt-installed
Filename: pool/rpt-alt-native-bad_1.0_amd64.deb
Size: 1
Description: first native alternative conflicts with installed binary

Package: rpt-alt-native-good
Version: 1.0
Architecture: amd64
Filename: pool/rpt-alt-native-good_1.0_amd64.deb
Size: 1
Description: second native alternative is safe

Package: rpt-all-native-tool
Version: 2.0
Architecture: amd64
Multi-Arch: foreign
Depends: rpt-all-native-data
Filename: pool/rpt-all-native-tool_2.0_amd64.deb
Size: 1
Description: native tool requiring shared all leaf

Package: rpt-all-native-data
Version: 1.0
Architecture: all
Multi-Arch: foreign
Filename: pool/rpt-all-native-data_1.0_all.deb
Size: 1
Description: signed universal binary dependency

Package: rpt-native-virtual-tool
Version: 2.0
Architecture: amd64
Multi-Arch: foreign
Depends: rpt-native-virtual-cap (>= 7.0)
Filename: pool/rpt-native-virtual-tool_2.0_amd64.deb
Size: 1
Description: native tool requiring a versioned virtual capability

Package: rpt-native-virtual-impl
Version: 1.0
Architecture: amd64
Provides: rpt-native-virtual-cap (= 7.0)
Filename: pool/rpt-native-virtual-impl_1.0_amd64.deb
Size: 1
Description: versioned native virtual implementation

Package: rpt-cross-virtual-provider
Version: 1.0
Architecture: amd64
Multi-Arch: foreign
Provides: rpt-cross-virtual-cap (= 7.0)
Depends: rpt-native-virtual-impl (>= 1.0)
Filename: pool/rpt-cross-virtual-provider_1.0_amd64.deb
Size: 1
Description: signed foreign native virtual provider with native dependency

Package: rpt-cross-any-provider
Version: 1.0
Architecture: amd64
Multi-Arch: allowed
Provides: rpt-cross-any-cap (= 7.0)
Filename: pool/rpt-cross-any-provider_1.0_amd64.deb
Size: 1
Description: signed allowed native provider for foreign any virtual dependency

Package: rpt-fallback-cap
Version: 1.0
Architecture: amd64
Filename: pool/rpt-fallback-cap_1.0_amd64.deb
Size: 1
Description: mismatched real package is not an eligible dependency

Package: rpt-fallback-provider
Version: 3.0
Architecture: amd64
Provides: rpt-fallback-cap (= 7.0)
Filename: pool/rpt-fallback-provider_3.0_amd64.deb
Size: 1
Description: signed native versioned virtual capability

Package: rpt-fallback-tool
Version: 1.0
Architecture: amd64
Multi-Arch: foreign
Depends: rpt-fallback-cap (>= 7.0)
Filename: pool/rpt-fallback-tool_1.0_amd64.deb
Size: 1
Description: foreign-compatible tool with versioned virtual Depends

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

Package: rpt-new-native-user
Version: 1.0
Architecture: i386
Depends: rpt-new-native-leaf (>= 2.0)
Filename: pool/rpt-new-native-user_1.0_i386.deb
Size: 1
Description: foreign user of a new native leaf

Package: rpt-new-allowed-user
Version: 1.0
Architecture: i386
Depends: rpt-new-allowed-leaf:any (>= 2.0)
Filename: pool/rpt-new-allowed-user_1.0_i386.deb
Size: 1
Description: foreign user of a new native allowed leaf

Package: rpt-native-recursive-user
Version: 1.0
Architecture: i386
Depends: rpt-native-recursive-tool (>= 2.0)
Filename: pool/rpt-native-recursive-user_1.0_i386.deb
Size: 1
Description: foreign i386 app with nested native dependency chain

Package: rpt-native-recursive-helper
Version: 99.0
Architecture: i386
Filename: pool/rpt-native-recursive-helper_99.0_i386.deb
Size: 1
Description: foreign decoy must not resolve amd64 owner's Depends

Package: rpt-pre-native-user
Version: 1.0
Architecture: i386
Depends: rpt-pre-native-tool (>= 2.0)
Filename: pool/rpt-pre-native-user_1.0_i386.deb
Size: 1
Description: foreign user of a native predepends tool

Package: rpt-alt-native-user
Version: 1.0
Architecture: i386
Depends: rpt-alt-native-tool (>= 2.0)
Filename: pool/rpt-alt-native-user_1.0_i386.deb
Size: 1
Description: foreign user of a native alternative tool

Package: rpt-all-native-user
Version: 1.0
Architecture: i386
Depends: rpt-all-native-tool (>= 2.0)
Filename: pool/rpt-all-native-user_1.0_i386.deb
Size: 1
Description: foreign user of a native universal dependency tool

Package: rpt-all-native-data
Version: 1.0
Architecture: all
Multi-Arch: foreign
Filename: pool/rpt-all-native-data_1.0_all.deb
Size: 1
Description: signed universal binary dependency

Package: rpt-native-virtual-user
Version: 1.0
Architecture: i386
Depends: rpt-native-virtual-tool (>= 2.0)
Filename: pool/rpt-native-virtual-user_1.0_i386.deb
Size: 1
Description: foreign app needing a native virtual provider

Package: rpt-native-virtual-decoy
Version: 99.0
Architecture: i386
Provides: rpt-native-virtual-cap (= 99.0)
Filename: pool/rpt-native-virtual-decoy_99.0_i386.deb
Size: 1
Description: foreign versioned virtual decoy

Package: rpt-cross-virtual-user
Version: 1.0
Architecture: i386
Depends: rpt-cross-virtual-cap (>= 7.0)
Filename: pool/rpt-cross-virtual-user_1.0_i386.deb
Size: 1
Description: i386 app requiring native foreign virtual provider

Package: rpt-cross-any-user
Version: 1.0
Architecture: i386
Depends: rpt-cross-any-cap:any (>= 7.0)
Filename: pool/rpt-cross-any-user_1.0_i386.deb
Size: 1
Description: i386 app requiring native allowed virtual provider

Package: rpt-cross-installed-only-user
Version: 1.0
Architecture: i386
Depends: rpt-cross-installed-only-cap (>= 7.0)
Filename: pool/rpt-cross-installed-only-user_1.0_i386.deb
Size: 1
Description: i386 app requiring virtual capability from native dpkg status

Package: rpt-cross-virtual-decoy
Version: 99.0
Architecture: i386
Provides: rpt-cross-virtual-cap (= 6.0)
Filename: pool/rpt-cross-virtual-decoy_99.0_i386.deb
Size: 1
Description: version insufficient i386 virtual provider must not match

Package: rpt-fallback-user
Version: 1.0
Architecture: i386
Depends: rpt-fallback-tool
Filename: pool/rpt-fallback-user_1.0_i386.deb
Size: 1
Description: foreign app selecting native versioned virtual provider despite real-name mismatch

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
    sed -E 's/:(all|amd64) ([^ ]+)$/ \2/' | LC_ALL=C sort -u \
    >"$root/rpt-$label-selected"
  test -s "$root/rpt-$label-selected"
  diff -u "$root/apt-$label-selected" "$root/rpt-$label-selected"
}
compare_plan real-virtual-fallback "$root/empty-status" rpt-fallback-user:i386
grep -Fxq 'rpt-fallback-user:i386 1.0' "$root/rpt-real-virtual-fallback-selected"
grep -Fxq 'rpt-fallback-tool 1.0' "$root/rpt-real-virtual-fallback-selected"
grep -Fxq 'rpt-fallback-provider 3.0' "$root/rpt-real-virtual-fallback-selected"
! grep -Fxq 'rpt-fallback-cap 1.0' "$root/rpt-real-virtual-fallback-selected"
test "$(wc -l < "$root/rpt-real-virtual-fallback-selected")" -eq 3
# M3 integrated system-upgrade audit: scan all installed native/Architecture:
# all identities, resolve all eligible upgrades jointly, and refuse holds,
# installed-only and partial plans. No host dpkg writes.
cat >"$root/system-upgrade-status" <<'EOF'
Package: rpt-new-native-leaf
Status: install ok installed
Architecture: amd64
Version: 1.0
Multi-Arch: foreign
Description: old native leaf

Package: rpt-new-allowed-leaf
Status: install ok installed
Architecture: amd64
Version: 1.0
Multi-Arch: allowed
Description: old allowed leaf

Package: rpt-pre-native-setup
Status: hold ok installed
Architecture: amd64
Version: 0.5
Description: held older setup

Package: rpt-installed-only
Status: install ok installed
Architecture: amd64
Version: 1.0
Description: configured binary without any repository origin

EOF
"$rpt" plan-system-upgrade-policy --cache "$root/cache" \
  --status "$root/system-upgrade-status" --apt-root "$root/rpt-apt" \
  --architecture amd64 --max-roots 128 --max-packages 200 \
  >"$root/rpt-system-upgrade.json"
python3 - "$root/rpt-system-upgrade.json" "$root/rpt-system-upgrade-selected" <<'PY'
import json,sys
report=json.load(open(sys.argv[1]))
assert report['scanned_installed']==4
assert report['requested_upgrades']==['rpt-new-allowed-leaf','rpt-new-native-leaf']
assert report['held']==['rpt-pre-native-setup:amd64']
assert report['installed_only']==['rpt-installed-only:amd64']
assert report['transaction']['upgrades']==2 and report['transaction']['installs']==0
assert report['transaction']['downgrades']==0
assert not report['transaction']['executable'] and not report['executable']
with open(sys.argv[2],'w') as f:
  for c in sorted(report['transaction']['changes'], key=lambda c:c['package']):
    assert c['action']=='upgrade' and c['architecture']=='amd64'
    f.write(c['package']+' '+c['to']+'\n')
PY
apt-get "${opts[@]}" -o "Dir::State::status=$root/system-upgrade-status" \
  -o APT::Install-Recommends=false -s -y upgrade >"$root/apt-system-upgrade"
# apt-get -s prints upgrades as "Inst name [old] (new ...)" and fresh
# installs as "Inst name (new ...)"; retain both as candidate-version evidence.
sed -nE 's/^Inst ([^ ]+) (\[[^]]+\] )?\(([^ )]+).*/\1 \3/p' "$root/apt-system-upgrade" | \
  LC_ALL=C sort -u >"$root/apt-system-upgrade-selected"
diff -u "$root/apt-system-upgrade-selected" "$root/rpt-system-upgrade-selected"
# The budget must fail closed; a partial upgrade of only the first binary
# would falsely imply that all installed candidates were considered.
if "$rpt" plan-system-upgrade-policy --cache "$root/cache" \
  --status "$root/system-upgrade-status" --apt-root "$root/rpt-apt" \
  --architecture amd64 --max-roots 1 >"$root/system-budget.log" 2>&1; then
  echo 'RPT silently previewed a partial system upgrade' >&2; exit 1
fi
"$rpt" plan-system-upgrade-policy --cache "$root/cache" \
  --status "$root/empty-status" --apt-root "$root/rpt-apt" \
  --architecture amd64 >"$root/system-empty.json"
python3 - "$root/system-empty.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r['scanned_installed']==0
assert r['requested_upgrades']==[]
assert r['transaction']['changes']==[]
assert not r['executable']
PY
# M4: reverse dependencies of packages NOT requested by the resolver
# must remain satisfiable after the signed upgrade. This consumer exists only
# in installed dpkg status, outside the new root candidate set.
python3 - "$root/system-upgrade-status" "$root/system-reverse-broken" <<'PY'
import sys
data=open(sys.argv[1]).read()
data=data.replace('Description: old native leaf',
                  'Provides: rpt-upgrade-virtual (= 1.0)\nDescription: old native leaf')
data=data.replace('Description: configured binary without any repository origin',
                  'Depends: rpt-upgrade-virtual (<= 1.0)\nDescription: configured binary without any repository origin')
open(sys.argv[2],'w').write(data)
PY
if "$rpt" plan-system-upgrade-policy --cache "$root/cache" \
  --status "$root/system-reverse-broken" --apt-root "$root/rpt-apt" \
  --architecture amd64 >"$root/system-reverse-broken.log" 2>&1; then
  echo 'RPT upgraded a virtual provider and broke an unselected installed dependent' >&2
  exit 1
fi
grep -Fq 'reverse dependency of rpt-installed-only:amd64' "$root/system-reverse-broken.log"
apt-get "${opts[@]}" -o "Dir::State::status=$root/system-reverse-broken" \
  -o APT::Install-Recommends=false -s -y upgrade >"$root/apt-reverse-broken"
if grep -Eq '^Inst rpt-new-native-leaf ' "$root/apt-reverse-broken"; then
  echo 'APT unexpectedly upgraded a provider needed by a retained old virtual dependent' >&2
  exit 1
fi

# A separate still-configured provider for the old virtual version makes the
# transition safe. This validates OR semantics across existing binary Provides.
cp "$root/system-reverse-broken" "$root/system-reverse-alternative"
cat >>"$root/system-reverse-alternative" <<'EOF'
Package: rpt-old-capability-provider
Status: install ok installed
Architecture: amd64
Version: 1.0
Provides: rpt-upgrade-virtual (= 1.0)
Description: second installed-only satisfying virtual provider

EOF
"$rpt" plan-system-upgrade-policy --cache "$root/cache" \
  --status "$root/system-reverse-alternative" --apt-root "$root/rpt-apt" \
  --architecture amd64 >"$root/system-reverse-alternative.json"
python3 - "$root/system-reverse-alternative.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r['scanned_installed']==5
assert r['post_state']['installed_before']==5
assert r['post_state']['installed_after']==5
assert r['post_state']['retained_dependents_checked']==3
assert r['post_state']['staged_binaries']==2
assert not r['post_state']['executable'] and not r['executable']
assert r['requested_upgrades']==['rpt-new-allowed-leaf','rpt-new-native-leaf']
PY

# A negative installed relationship with a staged v2 must be checked even if
# the installed owner was not in any new dependency closure.
python3 - "$root/system-upgrade-status" "$root/system-reverse-negative" <<'PY'
import sys
data=open(sys.argv[1]).read().replace(
  'Description: configured binary without any repository origin',
  'Conflicts: rpt-new-native-leaf (>= 2.0)\nDescription: configured binary without any repository origin')
open(sys.argv[2],'w').write(data)
PY
if "$rpt" plan-system-upgrade-policy --cache "$root/cache" \
  --status "$root/system-reverse-negative" --apt-root "$root/rpt-apt" \
  --architecture amd64 >"$root/system-reverse-negative.log" 2>&1; then
  echo 'RPT ignored reverse Conflicts against a staged upgrade' >&2
  exit 1
fi
grep -Fq 'Conflicts/Breaks' "$root/system-reverse-negative.log"
echo 'PASS M4 signed global installed reverse dependency, virtual alternatives and negative relation preflight'
# M5: an old, unneeded installed package blocks a new signed candidate.
# The normal no-removal planner must reject this. The experimental joint
# explicit-removal scenario must prove it safe across the ORIGINAL dpkg state.
cp "$root/system-upgrade-status" "$root/system-with-retired"
cat >>"$root/system-with-retired" <<'EOF'
Package: rpt-conflicting-old
Status: install ok installed
Architecture: amd64
Version: 1.0
Description: obsolete native binary blocks new signed leaf

EOF
if "$rpt" plan-system-upgrade-policy --cache "$root/cache" \
  --status "$root/system-with-retired" --apt-root "$root/rpt-apt" \
  --architecture amd64 >"$root/system-retired-no-removal.log" 2>&1; then
  echo 'RPT no-removal planner ignored an installed conflict' >&2
  exit 1
fi
"$rpt" plan-full-upgrade-scenario --cache "$root/cache" \
  --status "$root/system-with-retired" --apt-root "$root/rpt-apt" \
  --architecture amd64 --remove rpt-conflicting-old \
  --max-roots 128 --max-packages 200 >"$root/system-joint-scenario.json"
python3 - "$root/system-joint-scenario.json" "$root/system-joint-inst" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r['schema_version']==1
assert len(r['original_status_sha256'])==64
assert len(r['projected_status_sha256'])==64
assert r['original_status_sha256'] != r['projected_status_sha256']
assert r['removals']==[{
  'name':'rpt-conflicting-old','architecture':'amd64','installed_version':'1.0'}]
assert r['upgrades']['requested_upgrades']==[
  'rpt-new-allowed-leaf','rpt-new-native-leaf']
assert r['final_state']['installed_before']==5
assert r['final_state']['installed_after']==4
assert r['final_state']['removed_binaries']==1
assert r['final_state']['staged_binaries']==2
assert not r['executable'] and not r['final_state']['executable']
with open(sys.argv[2],'w') as f:
  for item in r['upgrades']['transaction']['changes']:
    f.write(item['package']+' '+item['to']+'\n')
PY
apt-get "${opts[@]}" -o "Dir::State::status=$root/system-with-retired" \
  -o APT::Install-Recommends=false -s -y install \
  rpt-new-native-leaf rpt-new-allowed-leaf rpt-conflicting-old- \
  >"$root/apt-joint-scenario"
sed -nE 's/^Inst ([^ ]+) (\[[^]]+\] )?\(([^ )]+).*/\1 \3/p' \
  "$root/apt-joint-scenario" | LC_ALL=C sort -u >"$root/apt-joint-inst"
LC_ALL=C sort -u "$root/system-joint-inst" -o "$root/system-joint-inst"
diff -u "$root/apt-joint-inst" "$root/system-joint-inst"
grep -Eq '^Remv rpt-conflicting-old( |$)' "$root/apt-joint-scenario"
# APT can independently decide whether a dist-upgrade removes this
# irrelevant blocker; this is recorded as evidence, not imposed as an
# ordering guarantee for arbitrary APT versions.
apt-get "${opts[@]}" -o "Dir::State::status=$root/system-with-retired" \
  -o APT::Install-Recommends=false -s -y dist-upgrade \
  >"$root/apt-joint-dist-upgrade"
grep -Eq '^Remv rpt-conflicting-old( |$)' "$root/apt-joint-dist-upgrade"
grep -Eq '^Inst rpt-new-native-leaf( |$)' "$root/apt-joint-dist-upgrade"

# No partial/misleading plan may be returned for an unsafe scenario:
# held/Essential, duplicate removal, broken reverse dependency and root
# discovery budget must each fail before any mutation.
sed 's/Status: install ok installed/Status: hold ok installed/' \
  "$root/system-with-retired" >"$root/system-all-held"
if "$rpt" plan-full-upgrade-scenario --cache "$root/cache" \
  --status "$root/system-all-held" --apt-root "$root/rpt-apt" \
  --remove rpt-conflicting-old >"$root/full-held.log" 2>&1; then
  echo 'RPT allowed held removed binary' >&2; exit 1
fi
if "$rpt" plan-full-upgrade-scenario --cache "$root/cache" \
  --status "$root/system-with-retired" --apt-root "$root/rpt-apt" \
  --remove rpt-conflicting-old --remove rpt-conflicting-old:amd64 \
  >"$root/full-dup.log" 2>&1; then
  echo 'RPT accepted duplicate explicit removal' >&2; exit 1
fi
sed 's/Description: configured binary without any repository origin/Depends: rpt-conflicting-old\nDescription: configured binary without any repository origin/' \
  "$root/system-with-retired" >"$root/full-dependency-blocked"
if "$rpt" plan-full-upgrade-scenario --cache "$root/cache" \
  --status "$root/full-dependency-blocked" --apt-root "$root/rpt-apt" \
  --remove rpt-conflicting-old >"$root/full-dependency-blocked.log" 2>&1; then
  echo 'RPT removal silently broke retained installed reverse dependency' >&2
  exit 1
fi
if "$rpt" plan-full-upgrade-scenario --cache "$root/cache" \
  --status "$root/system-with-retired" --apt-root "$root/rpt-apt" \
  --max-roots 1 --remove rpt-conflicting-old >"$root/full-roots.log" 2>&1; then
  echo 'RPT silently truncated joint upgrade root set' >&2; exit 1
fi
echo 'PASS M5 signed explicit removal plus global joint upgrades, APT oracle and safety blockers'
# M6: the read-only automatic proposer must choose the UNIQUE MINIMAL
# signed-conflict-derived removal set. Verify the prior explicit-scenario
# oracle and APT dist-upgrade agree for this narrow fixture.
"$rpt" plan-auto-full-upgrade --cache "$root/cache" \
  --status "$root/system-with-retired" --apt-root "$root/rpt-apt" \
  --architecture amd64 --max-removals 3 --max-roots 128 \
  >"$root/system-auto-full-upgrade.json"
python3 - "$root/system-auto-full-upgrade.json" \
  "$root/system-joint-scenario.json" "$root/apt-joint-dist-upgrade" <<'PY'
import json,sys,re
auto=json.load(open(sys.argv[1]))
explicit=json.load(open(sys.argv[2]))
apt=open(sys.argv[3]).read()
assert auto['schema_version']==1
assert not auto['executable']
assert auto['removal_candidates']==['rpt-conflicting-old:amd64']
assert auto['selected_removals']==['rpt-conflicting-old:amd64']
assert auto['combinations_examined']==1
assert auto['selection']['kind']=='removal_scenario'
assert auto['selection']['scenario']['original_status_sha256']==explicit['original_status_sha256']
assert auto['selection']['scenario']['removals']==explicit['removals']
assert auto['selection']['scenario']['upgrades']['requested_upgrades']==explicit['upgrades']['requested_upgrades']
assert not auto['selection']['scenario']['executable']
assert re.search(r'^Remv rpt-conflicting-old(?: |$)',apt,re.M)
PY
"$rpt" plan-auto-full-upgrade --cache "$root/cache" \
  --status "$root/system-upgrade-status" --apt-root "$root/rpt-apt" \
  --architecture amd64 --max-removals 3 >"$root/system-auto-none.json"
python3 - "$root/system-auto-none.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r['selection']['kind']=='no_removal'
assert r['selected_removals']==[] and r['removal_candidates']==[]
assert r['combinations_examined']==0 and not r['executable']
assert r['selection']['audit']['transaction']['upgrades']==2
PY
# Test the specific blocker held while legitimate upgraded roots remain
# unheld; broad hold state would suppress the upgrade entirely.
python3 - "$root/system-with-retired" "$root/system-auto-held" <<'PY'
import sys
s=open(sys.argv[1]).read()
s=s.replace('Package: rpt-conflicting-old\nStatus: install ok installed',
            'Package: rpt-conflicting-old\nStatus: hold ok installed')
open(sys.argv[2],'w').write(s)
PY
if "$rpt" plan-auto-full-upgrade --cache "$root/cache" \
  --status "$root/system-auto-held" --apt-root "$root/rpt-apt" \
  --max-removals 3 >"$root/system-auto-held.log" 2>&1; then
  echo 'RPT automatically removed a held conflict blocker' >&2; exit 1
fi
if "$rpt" plan-auto-full-upgrade --cache "$root/cache" \
  --status "$root/full-dependency-blocked" --apt-root "$root/rpt-apt" \
  --max-removals 3 >"$root/system-auto-reverse.log" 2>&1; then
  echo 'RPT automatically removed an installed reverse dependency target' >&2; exit 1
fi
if "$rpt" plan-auto-full-upgrade --cache "$root/cache" \
  --status "$root/system-with-retired" --apt-root "$root/rpt-apt" \
  --max-roots 1 >"$root/system-auto-budget.log" 2>&1; then
  echo 'RPT silently truncated auto full-upgrade roots' >&2; exit 1
fi
if "$rpt" plan-auto-full-upgrade --cache "$root/cache" \
  --status "$root/system-with-retired" --apt-root "$root/rpt-apt" \
  --max-removals 0 >"$root/system-auto-zero-budget.log" 2>&1; then
  echo 'RPT accepted an invalid automatic removal budget' >&2; exit 1
fi
echo 'PASS M6 bounded signed automatic no-removal and unique-minimal conflict removal oracle'


echo 'PASS bounded signed no-removal system upgrade audit vs APT held/installed-only differential'
compare_plan cross-virtual "$root/empty-status" rpt-cross-virtual-user:i386
grep -Fxq 'rpt-cross-virtual-user:i386 1.0' "$root/rpt-cross-virtual-selected"
grep -Fxq 'rpt-cross-virtual-provider 1.0' "$root/rpt-cross-virtual-selected"
grep -Fxq 'rpt-native-virtual-impl 1.0' "$root/rpt-cross-virtual-selected"
! grep -Fq 'rpt-cross-virtual-decoy' "$root/rpt-cross-virtual"
test "$(wc -l < "$root/rpt-cross-virtual-selected")" -eq 3
compare_plan cross-any "$root/empty-status" rpt-cross-any-user:i386
grep -Fxq 'rpt-cross-any-provider 1.0' "$root/rpt-cross-any-selected"
test "$(wc -l < "$root/rpt-cross-any-selected")" -eq 2
# An installed native provider's Multi-Arch and virtual Provides must
# match the requesting i386 dependency. Do not infer its capabilities
# from the signed repository's current candidate.
cat >"$root/cross-installed-only" <<'EOF'
Package: rpt-cross-status-provider
Status: install ok installed
Architecture: amd64
Version: 1.0
Multi-Arch: foreign
Provides: rpt-cross-installed-only-cap (= 7.0)
Description: installed-only native foreign virtual provider

EOF
compare_plan cross-installed-only "$root/cross-installed-only" rpt-cross-installed-only-user:i386
test "$(wc -l < "$root/rpt-cross-installed-only-selected")" -eq 1
grep -Fxq 'rpt-cross-installed-only-user:i386 1.0' "$root/rpt-cross-installed-only-selected"
sed 's/Multi-Arch: foreign/Multi-Arch: no/' "$root/cross-installed-only" >"$root/cross-installed-no"
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/cross-installed-no" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-cross-installed-only-user:i386 >"$root/cross-installed-no.log" 2>&1; then
  echo 'RPT incorrectly allowed non-Multi-Arch installed virtual provider' >&2; exit 1
fi
sed 's/Provides: rpt-cross-installed-only-cap (= 7.0)/Provides: rpt-cross-installed-only-cap (= 6.0)/' \
  "$root/cross-installed-only" >"$root/cross-installed-old"
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/cross-installed-old" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-cross-installed-only-user:i386 >"$root/cross-installed-old.log" 2>&1; then
  echo 'RPT incorrectly accepted outdated installed virtual capability' >&2; exit 1
fi
compare_plan native-virtual "$root/empty-status" rpt-native-virtual-user:i386
grep -Fxq 'rpt-native-virtual-tool 2.0' "$root/rpt-native-virtual-selected"
grep -Fxq 'rpt-native-virtual-impl 1.0' "$root/rpt-native-virtual-selected"
grep -Fxq 'rpt-native-virtual-user:i386 1.0' "$root/rpt-native-virtual-selected"
! grep -Fq 'rpt-native-virtual-decoy' "$root/rpt-native-virtual"
test "$(wc -l <"$root/rpt-native-virtual-selected")" -eq 3
# Installed status may provide a capability with no repository candidate;
# its installed Provides version, not a newer repository index, is proof.
cat >"$root/native-virtual-installed" <<'EOF'
Package: rpt-native-vinstalled
Status: install ok installed
Architecture: amd64
Version: 1.0
Provides: rpt-native-virtual-cap (= 7.0)
Description: installed-only native virtual satisfier

EOF
compare_plan native-virtual-installed "$root/native-virtual-installed" rpt-native-virtual-user:i386
test "$(wc -l <"$root/rpt-native-virtual-installed-selected")" -eq 2
! grep -Fq 'rpt-native-virtual-impl' "$root/rpt-native-virtual-installed"
# An older installed virtual capability is insufficient. Both solvers must
# select the signed new provider rather than trust its unrelated binary version.
sed 's/Provides: rpt-native-virtual-cap (= 7.0)/Provides: rpt-native-virtual-cap (= 6.0)/' \
  "$root/native-virtual-installed" >"$root/native-virtual-outdated"
compare_plan native-virtual-outdated "$root/native-virtual-outdated" rpt-native-virtual-user:i386
grep -Fxq 'rpt-native-virtual-impl 1.0' "$root/rpt-native-virtual-outdated-selected"
compare_plan native-predepends "$root/empty-status" rpt-pre-native-user:i386
grep -Fxq 'rpt-pre-native-user:i386 1.0' "$root/rpt-native-predepends-selected"
grep -Fxq 'rpt-pre-native-tool 2.0' "$root/rpt-native-predepends-selected"
grep -Fxq 'rpt-pre-native-setup 1.0' "$root/rpt-native-predepends-selected"
test "$(wc -l <"$root/rpt-native-predepends-selected")" -eq 3
compare_plan native-all "$root/empty-status" rpt-all-native-user:i386
grep -Fxq 'rpt-all-native-data 1.0' "$root/rpt-native-all-selected"
test "$(wc -l <"$root/rpt-native-all-selected")" -eq 3
cat >"$root/native-conflicting-installed" <<'EOF'
Package: rpt-alt-installed
Status: hold ok installed
Architecture: amd64
Version: 1.0
Description: held installed native binary blocks first ordered alternative

EOF
# APT may REMOVE a non-held conflicting package instead of selecting
# the second alternative. Hold makes removal forbidden in both planners.
compare_plan native-alts "$root/native-conflicting-installed" rpt-alt-native-user:i386
grep -Fxq 'rpt-alt-native-good 1.0' "$root/rpt-native-alts-selected"
! grep -Fq 'rpt-alt-native-bad' "$root/rpt-native-alts"
test "$(wc -l <"$root/rpt-native-alts-selected")" -eq 3
# Cross-architecture Pre-Depends does not authorize upgrading an existing
# native setup package implicitly.
cat >"$root/native-predepends-old" <<'EOF'
Package: rpt-pre-native-setup
Status: install ok installed
Architecture: amd64
Version: 0.5
Description: old native setup

EOF
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/native-predepends-old" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-pre-native-user:i386 >"$root/predepends-old.log" 2>&1; then
  echo 'RPT improperly upgraded a native Pre-Depends through a foreign root' >&2; exit 1
fi
compare_plan recursive "$root/empty-status" rpt-native-recursive-user:i386
grep -Fxq 'rpt-native-recursive-user:i386 1.0' "$root/rpt-recursive-selected"
grep -Fxq 'rpt-native-recursive-tool 2.0' "$root/rpt-recursive-selected"
grep -Fxq 'rpt-native-recursive-helper 3.0' "$root/rpt-recursive-selected"
grep -Fxq 'rpt-native-recursive-bottom 4.0' "$root/rpt-recursive-selected"
! grep -Fq 'rpt-native-recursive-helper:i386' "$root/rpt-recursive"
test "$(wc -l <"$root/rpt-recursive-selected")" -eq 4
# An already configured native Multi-Arch:no helper can satisfy its native
# parent; do not schedule a foreign copy of a same-name i386 decoy.
cat >"$root/native-recursive-helper-installed" <<'EOF'
Package: rpt-native-recursive-helper
Status: install ok installed
Architecture: amd64
Version: 3.0
Description: installed matching native helper

EOF
compare_plan recursive-installed "$root/native-recursive-helper-installed" rpt-native-recursive-user:i386
test "$(wc -l <"$root/rpt-recursive-installed-selected")" -eq 2
grep -Fxq 'rpt-native-recursive-tool 2.0' "$root/rpt-recursive-installed-selected"
grep -Fxq 'rpt-native-recursive-user:i386 1.0' "$root/rpt-recursive-installed-selected"
# Installed native dependency cannot be silently upgraded by a foreign root.
sed 's/Version: 3.0/Version: 2.0/' "$root/native-recursive-helper-installed" >"$root/native-recursive-old"
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/native-recursive-old" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-native-recursive-user:i386 >"$root/native-recursive-old.log" 2>&1; then
  echo 'RPT allowed an implicit native recursive helper upgrade' >&2; exit 1
fi
compare_plan new-native "$root/empty-status" rpt-new-native-user:i386
compare_plan new-allowed "$root/empty-status" rpt-new-allowed-user:i386
grep -Fxq 'rpt-new-native-leaf 2.0' "$root/rpt-new-native-selected"
grep -Fxq 'rpt-new-allowed-leaf 2.0' "$root/rpt-new-allowed-selected"
grep -Fq 'rpt-new-native-leaf:amd64 2.0' "$root/rpt-new-native"
grep -Fq 'rpt-new-allowed-leaf:amd64 2.0' "$root/rpt-new-allowed"
test "$(wc -l <"$root/rpt-new-native-selected")" -eq 2
test "$(wc -l <"$root/rpt-new-allowed-selected")" -eq 2
# An installed native version mismatch requires an unsupported native upgrade.
cat >"$root/native-leaf-outdated" <<'EOF'
Package: rpt-new-native-leaf
Status: install ok installed
Architecture: amd64
Version: 1.0
Multi-Arch: foreign
Description: installed native old version

EOF
if "$rpt" plan-install-foreign-policy --cache "$root/cache" \
  --status "$root/native-leaf-outdated" --apt-root "$root/rpt-apt" \
  --native-architecture amd64 --foreign-architecture i386 \
  rpt-new-native-user:i386 >"$root/native-leaf-outdated.log" 2>&1; then
  echo 'RPT incorrectly allowed an implicit native dependency upgrade' >&2; exit 1
fi
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
echo 'PASS signed Multi-Arch real-name mismatch virtual fallback, cross-architecture Provides and installed-status safety'

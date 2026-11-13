#!/usr/bin/env bash
# Independent APT simulation oracle; no host dpkg database mutations.
set -euo pipefail
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
mkdir -p "$root/repo" "$root/lists/partial" "$root/cache/archives/partial" "$root/etc/empty-sources" "$root/etc/preferences.d"
: > "$root/status"
cat > "$root/repo/Packages" <<'PACKAGES'
Package: rpt-app
Version: 1.0
Architecture: all
Depends: rpt-lib (>= 2)
Filename: pool/rpt-app_1.0_all.deb
Size: 1
Description: APT resolver fixture app

Package: rpt-lib
Version: 2.0
Architecture: all
Filename: pool/rpt-lib_2.0_all.deb
Size: 1
Description: APT resolver fixture library

Package: rpt-broken
Version: 1.0
Architecture: all
Depends: rpt-missing (>= 1)
Filename: pool/rpt-broken_1.0_all.deb
Size: 1
Description: unsatisfiable dependency fixture

Package: rpt-conflict
Version: 1.0
Architecture: all
Conflicts: rpt-lib
Filename: pool/rpt-conflict_1.0_all.deb
Size: 1
Description: conflict fixture

Package: rpt-alternative
Version: 1.0
Architecture: all
Depends: rpt-missing | rpt-lib
Filename: pool/rpt-alternative_1.0_all.deb
Size: 1
Description: alternative fixture

Package: rpt-virtual-app
Version: 1.0
Architecture: all
Depends: rpt-virtual-api (>= 2)
Filename: pool/rpt-virtual-app_1.0_all.deb
Size: 1
Description: virtual dependency fixture

Package: rpt-provider
Version: 1.0
Architecture: all
Provides: rpt-virtual-api (= 2)
Filename: pool/rpt-provider_1.0_all.deb
Size: 1
Description: versioned virtual provider fixture
Package: rpt-pre-app
Version: 1.0
Architecture: all
Pre-Depends: rpt-foundation (>= 2)
Filename: pool/rpt-pre-app_1.0_all.deb
Size: 1
Description: pre-dependency fixture

Package: rpt-foundation
Version: 2.0
Architecture: all
Filename: pool/rpt-foundation_2.0_all.deb
Size: 1
Description: pre-dependency foundation

Package: rpt-version-mismatch
Version: 1.0
Architecture: all
Depends: rpt-lib (>= 3)
Filename: pool/rpt-version-mismatch_1.0_all.deb
Size: 1
Description: version mismatch fixture

PACKAGES
printf 'deb [trusted=yes] file:%s/repo ./\n' "$root" > "$root/etc/sources.list"
opts=(
  -o "Dir::State::status=$root/status"
  -o "Dir::State::lists=$root/lists"
  -o "Dir::Cache=$root/cache"
  -o "Dir::Etc::sourcelist=$root/etc/sources.list"
  -o "Dir::Etc::sourceparts=$root/etc/empty-sources"
  -o "Dir::Etc::preferences=$root/etc/preferences"
  -o "Dir::Etc::preferencesparts=$root/etc/preferences.d"
  -o "APT::Get::List-Cleanup=0"
  -o "Debug::NoLocking=1"
)
apt-get "${opts[@]}" update >/dev/null
simulate() { apt-get "${opts[@]}" -s -y install "$@" 2>&1; }
assert_installs() {
  local name="$1" output="$2"
  shift 2
  for package in "$@"; do
    grep -Eq "^Inst ${package}([[:space:]]|$)" <<<"$output" || {
      printf 'APT did not install %s for %s:\n%s\n' "$package" "$name" "$output" >&2
      exit 1
    }
  done
}
out="$(simulate rpt-app)"
assert_installs basic "$out" rpt-lib rpt-app
out="$(simulate rpt-alternative)"
assert_installs alternatives "$out" rpt-lib rpt-alternative
out="$(simulate rpt-pre-app)"
assert_installs predepends "$out" rpt-foundation rpt-pre-app
if simulate rpt-version-mismatch >"$root/version-mismatch.log"; then
  echo "APT unexpectedly accepted unsatisfied dependency version" >&2; exit 1
fi
out="$(simulate rpt-virtual-app)"
assert_installs provides "$out" rpt-provider rpt-virtual-app
if simulate rpt-broken >"$root/broken.log"; then
  echo "APT unexpectedly accepted missing dependency" >&2; exit 1
fi
if simulate rpt-app rpt-conflict >"$root/conflict.log"; then
  echo "APT unexpectedly accepted conflicting packages" >&2; exit 1
fi
echo "PASS independent APT simulation oracle: dependencies, alternatives, versioned Provides, unsatisfied dependency, Pre-Depends, version mismatch and Conflicts"

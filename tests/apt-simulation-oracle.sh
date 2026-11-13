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
# Generate a disposable signing identity, scoped entirely to this fixture.
# Both APT and future RPT differential runs can verify the same InRelease.
command -v gpg >/dev/null
command -v gpgv >/dev/null
mkdir -m 700 "$root/gnupg"
gpg --homedir "$root/gnupg" --batch --pinentry-mode loopback \
  --passphrase '' --quick-generate-key \
  'RPT CI fixture <rpt-fixture@example.invalid>' ed25519 sign 0 >/dev/null 2>&1
gpg --homedir "$root/gnupg" --batch --export \
  'RPT CI fixture <rpt-fixture@example.invalid>' > "$root/fixture-keyring.gpg"
test -s "$root/fixture-keyring.gpg"
index_hash="$(sha256sum "$root/repo/Packages" | awk '{print $1}')"
index_size="$(wc -c < "$root/repo/Packages" | tr -d ' ')"
cat > "$root/repo/Release" <<RELEASE
Origin: RPT CI
Label: RPT CI
Suite: fixture
Codename: fixture
Date: $(LC_ALL=C date -Ru)
Architectures: all amd64 arm64
Components: main
Description: Ephemeral signed resolver oracle
SHA256:
 $index_hash $index_size Packages
RELEASE
gpg --homedir "$root/gnupg" --batch --yes --pinentry-mode loopback \
  --passphrase '' --clearsign --digest-algo SHA256 \
  --output "$root/repo/InRelease" "$root/repo/Release"
gpgv --keyring "$root/fixture-keyring.gpg" "$root/repo/InRelease" >/dev/null 2>&1
printf 'deb [signed-by=%s] file:%s/repo ./\n' \
  "$root/fixture-keyring.gpg" "$root" > "$root/etc/sources.list"
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
# Negative trust-chain fixtures: altered index and altered signed metadata
# must fail before any simulated dependency resolution is considered valid.
cp "$root/repo/Packages" "$root/Packages.original"
printf '\\n# tampered after signing\\n' >> "$root/repo/Packages"
if apt-get "${opts[@]}" update >"$root/tampered-index.log" 2>&1; then
  echo "APT accepted a Packages index modified after Release signing" >&2
  exit 1
fi
mv "$root/Packages.original" "$root/repo/Packages"
cp "$root/repo/InRelease" "$root/InRelease.original"
printf '\\n# altered signed metadata\\n' >> "$root/repo/InRelease"
if apt-get "${opts[@]}" update >"$root/tampered-release.log" 2>&1; then
  echo "APT accepted an altered InRelease" >&2
  exit 1
fi
mv "$root/InRelease.original" "$root/repo/InRelease"
echo "PASS independent APT simulation oracle: dependencies, alternatives, versioned Provides, unsatisfied dependency, Pre-Depends, version mismatch and Conflicts"

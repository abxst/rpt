#!/usr/bin/env bash
# Compare remove, reverse-dependency cascade, alternatives and autoremove with
# APT's read-only simulator against the exact same disposable dpkg status.
set -euo pipefail
rpt="${1:?RPT binary required}"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
arch="$(dpkg --print-architecture)"
mkdir -p "$root/lists/partial" "$root/cache/archives/partial" "$root/etc/sources.d"
: > "$root/etc/sources.list"

cat > "$root/status" <<EOF
Package: rootpkg
Status: install ok installed
Architecture: $arch
Version: 1.0
Depends: libpkg (>= 1.0)
Description: explicitly installed root package

Package: libpkg
Status: install ok installed
Architecture: $arch
Version: 1.0
Description: required library

Package: altroot
Status: install ok installed
Architecture: $arch
Version: 1.0
Depends: firstpkg | secondpkg
Description: manually installed alternate root

Package: firstpkg
Status: install ok installed
Architecture: $arch
Version: 1.0
Description: first alternative

Package: secondpkg
Status: install ok installed
Architecture: $arch
Version: 1.0
Description: second alternative

Package: orphanpkg
Status: install ok installed
Architecture: $arch
Version: 1.0
Description: installed automatic orphan

Package: criticalpkg
Status: install ok installed
Architecture: $arch
Version: 1.0
Essential: yes
Description: protected package that RPT must refuse to remove

EOF
cat > "$root/extended_states" <<EOF
Package: libpkg
Architecture: $arch
Auto-Installed: 1

Package: firstpkg
Architecture: $arch
Auto-Installed: 1

Package: secondpkg
Architecture: $arch
Auto-Installed: 1

Package: orphanpkg
Architecture: $arch
Auto-Installed: 1

EOF

opts=(
  -o "Dir::State::status=$root/status"
  -o "Dir::State::extended_states=$root/extended_states"
  -o "Dir::State::lists=$root/lists"
  -o "Dir::Cache=$root/cache"
  -o "Dir::Etc::sourcelist=$root/etc/sources.list"
  -o "Dir::Etc::sourceparts=$root/etc/sources.d"
  -o "Debug::NoLocking=1"
  -o "APT::Get::List-Cleanup=0"
)
before_status="$(sha256sum "$root/status" | cut -d' ' -f1)"
before_auto="$(sha256sum "$root/extended_states" | cut -d' ' -f1)"

compare_removals() {
  local label="$1" plan="$2" reference="$3"
  python3 - "$label" "$plan" "$reference" <<'PY'
import json, sys
label, path, reference = sys.argv[1:]
plan = json.load(open(path))
assert plan['removable'], f"{label}: RPT reported blockers: {plan['blockers']}"
assert plan['executable'] is False, f"{label}: RPT unexpectedly authorized host execution"
rpt = sorted(p.split(':', 1)[0] for p in plan['remove'])
apt = sorted(line.split()[1].split(':', 1)[0]
             for line in open(reference) if line.startswith('Remv '))
assert apt == rpt, f"{label}: APT={apt} vs RPT={rpt}"
assert apt, f"{label}: fixture produced no removal candidates"
PY
}
# Removing a required library also removes its dependent app. Neither
# simulation should remove an unrelated automatic orphan.
apt-get "${opts[@]}" -s -y remove libpkg >"$root/apt-required"
"$rpt" plan-remove --status "$root/status" --architecture "$arch" \
  --cascade libpkg >"$root/rpt-required.json"
compare_removals "reverse dependency cascade" "$root/rpt-required.json" "$root/apt-required"

# A surviving alternative means removing firstpkg must keep altroot.
apt-get "${opts[@]}" -s -y remove firstpkg >"$root/apt-single-alt"
"$rpt" plan-remove --status "$root/status" --architecture "$arch" \
  firstpkg >"$root/rpt-single-alt.json"
compare_removals "installed alternative" "$root/rpt-single-alt.json" "$root/apt-single-alt"

# Removing *both* providers must also remove their installed consumer.
apt-get "${opts[@]}" -s -y remove firstpkg secondpkg >"$root/apt-both-alt"
"$rpt" plan-remove --status "$root/status" --architecture "$arch" \
  --cascade firstpkg secondpkg >"$root/rpt-both-alt.json"
compare_removals "joint provider removal" "$root/rpt-both-alt.json" "$root/apt-both-alt"

# APT manual/automatic markers: keep dependencies of manual roots, but
# collect the unreachable automatic orphan.
apt-get "${opts[@]}" -s -y autoremove >"$root/apt-autoremove"
"$rpt" plan-autoremove --status "$root/status" \
  --extended-states "$root/extended_states" --architecture "$arch" \
  >"$root/rpt-autoremove.json"
compare_removals "autoremove reachability" "$root/rpt-autoremove.json" "$root/apt-autoremove"

# Positive execution planning must not bypass Essential/Protected guards.
"$rpt" plan-remove --status "$root/status" --architecture "$arch" \
  criticalpkg >"$root/rpt-critical.json"
python3 - "$root/rpt-critical.json" <<'PY'
import json,sys
plan=json.load(open(sys.argv[1]))
assert not plan['removable'] and not plan['executable']
assert any('Essential' in blocker for blocker in plan['blockers'])
PY

test "$(sha256sum "$root/status" | cut -d' ' -f1)" = "$before_status"
test "$(sha256sum "$root/extended_states" | cut -d' ' -f1)" = "$before_auto"
echo "PASS read-only APT/RPT removal differential: reverse dependency cascade, installed alternatives, joint provider removal, autoremove and Essential blocker"

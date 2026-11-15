#!/usr/bin/env bash
# M16: Read-only APT-like operation CLI contract, no dpkg/apt writes.
set -euo pipefail
test "$(id -u)" -ne 0
rpt="${1:-./target/debug/rpt}"
work="$(mktemp -d /tmp/rpt-m16-cli.XXXXXXXX)"
trap 'rm -rf -- "$work"' EXIT
cat > "$work/status" <<'EOF'
Package: client
Architecture: amd64
Version: 1
Status: install ok installed
Depends: librpt (>= 1)

Package: librpt
Architecture: amd64
Version: 1
Status: install ok installed

Package: orphan
Architecture: amd64
Version: 1
Status: deinstall ok config-files

EOF
cat > "$work/extended_states" <<'EOF'
Package: librpt
Architecture: amd64
Auto-Installed: 1

EOF
before="$(sha256sum "$work/status" | awk '{print $1}')"
marks_before="$(sha256sum "$work/extended_states" | awk '{print $1}')"
"$rpt" apt-simulate-remove --status "$work/status" librpt >"$work/blocked.json"
python3 - "$work/blocked.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["schema_version"] == 1
assert r["operation"] == "remove"
assert r["outcome"] == "blocked"
assert r["blockers"], r
assert r["viable_in_supported_subset"] is False
assert r["simulation_only"] is True
assert r["apt_parity_verified"] is False
assert r["host_execution_allowed"] is False
PY
"$rpt" apt-simulate-remove --status "$work/status" --cascade librpt >"$work/cascade.json"
python3 - "$work/cascade.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["outcome"] == "planned"
assert set(r["selected_removals"]) == {"librpt:amd64","client:amd64"}, r
assert r["detailed_audit"]["mode"] == "remove"
PY
"$rpt" apt-simulate-purge --status "$work/status" orphan >"$work/purge.json"
python3 - "$work/purge.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["operation"] == "purge"
assert r["selected_removals"] == ["orphan:amd64"], r
PY
"$rpt" apt-simulate-autoremove --status "$work/status" \
    --extended-states "$work/extended_states" >"$work/autoremove.json"
python3 - "$work/autoremove.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["operation"] == "autoremove"
assert r["outcome"] == "no_changes", r
assert r["selected_removals"] == [], r
assert len(r["apt_marks_sha256"]) == 64
PY
"$rpt" apt-simulate-autoremove --status "$work/status" \
    --extended-states "$work/missing-marks" >"$work/missing.json"
python3 - "$work/missing.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["operation"] == "autoremove"
assert r["outcome"] == "no_changes"
assert r["apt_marks_sha256"] is None
PY
# No guessed results for unsigned/missing repository metadata or
# unsupported policy widths: fail closed without touching dpkg.
if "$rpt" apt-simulate-upgrade --status "$work/status" \
      --cache "$work/no-signed-cache" --max-roots 129 >"$work/unsafe.json" 2>/dev/null; then
  echo 'M16 incorrectly accepted unsupported system upgrade width' >&2; exit 1
fi
if "$rpt" apt-simulate-full-upgrade --status "$work/status" \
      --cache "$work/no-signed-cache" --max-removals 4 >"$work/unsafe.json" 2>/dev/null; then
  echo 'M16 incorrectly accepted unsupported removal width' >&2; exit 1
fi
if "$rpt" apt-simulate-remove --status "$work/status" --architecture "../amd64" \
      librpt >"$work/unsafe.json" 2>/dev/null; then
  echo 'M16 accepted unsafe architecture scope' >&2; exit 1
fi
test "$(sha256sum "$work/status" | awk '{print $1}')" = "$before"
test "$(sha256sum "$work/extended_states" | awk '{print $1}')" = "$marks_before"
echo 'PASS M16 read-only APT operation simulation CLI, blocked removals, purge, autoremove and negative scopes'

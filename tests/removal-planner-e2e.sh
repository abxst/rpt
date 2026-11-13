#!/usr/bin/env bash
set -euo pipefail
rpt="${1:?rpt binary required}"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT
cat > "$tmp/status" <<'EOF'
Package: appfoo
Architecture: amd64
Version: 1
Status: install ok installed
Depends: libfoo (>= 1) | libother

Package: libfoo
Architecture: amd64
Version: 1
Status: install ok installed

Package: libother
Architecture: amd64
Version: 1
Status: install ok installed

Package: orphanfoo
Architecture: amd64
Version: 1
Status: install ok installed

EOF
cat > "$tmp/extended_states" <<'EOF'
Package: libfoo
Architecture: amd64
Auto-Installed: 1

Package: libother
Architecture: amd64
Auto-Installed: 1

Package: orphanfoo
Architecture: amd64
Auto-Installed: 1

EOF
"$rpt" plan-remove --status "$tmp/status" --architecture amd64 libfoo > "$tmp/one.json"
"$rpt" plan-remove --status "$tmp/status" --architecture amd64 libfoo libother > "$tmp/both.json"
"$rpt" plan-remove --status "$tmp/status" --architecture amd64 --cascade libfoo libother > "$tmp/cascade.json"
"$rpt" plan-autoremove --status "$tmp/status" --extended-states "$tmp/extended_states" --architecture amd64 > "$tmp/auto.json"
python3 - "$tmp" <<'PY'
import json, pathlib, sys
root=pathlib.Path(sys.argv[1])
one=json.loads((root/'one.json').read_text())
both=json.loads((root/'both.json').read_text())
cascade=json.loads((root/'cascade.json').read_text())
auto=json.loads((root/'auto.json').read_text())
assert one['removable'] and not one['blockers']
assert not both['removable'] and any('appfoo' in x for x in both['blockers'])
assert cascade['removable'] and cascade['remove'] == ['appfoo:amd64','libfoo:amd64','libother:amd64']
assert auto['removable'] and auto['remove'] == ['orphanfoo:amd64']
assert all(not x['executable'] for x in (one,both,cascade,auto))
PY
echo "PASS read-only remove/cascade/autoremove planner"

#!/usr/bin/env bash
# M20: bounded read-only dpkg health audit; no host package mutation.
set -euo pipefail
test "$(id -u)" -ne 0
rpt="$1"
work="$(mktemp -d /tmp/rpt-m20-dpkg-audit.XXXXXXXX)"
trap 'rm -rf -- "$work"' EXIT
cat > "$work/clean-status" <<'EOF'
Package: installed
Architecture: amd64
Version: 1.0
Status: hold ok installed

Package: stale-config
Architecture: amd64
Version: 0.9
Status: deinstall ok config-files

EOF
cat > "$work/broken-status" <<'EOF'
Package: installed
Architecture: amd64
Version: 1.0
Status: install ok installed

Package: interrupted
Architecture: amd64
Version: 2.0
Status: install reinstreq half-configured

EOF
before="$(sha256sum "$work/broken-status" | cut -d' ' -f1)"
"$rpt" dpkg-audit --status "$work/clean-status" --require-clean > "$work/clean.json"
python3 - "$work/clean.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["healthy"] is True and r["fully_installed"] == 1
assert r["residual_config"] == 1 and r["issues"] == []
assert r["read_only"] and not r["host_execution_allowed"]
PY
"$rpt" dpkg-audit --status "$work/broken-status" > "$work/broken.json"
python3 - "$work/broken.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["healthy"] is False and len(r["issues"]) == 1, r
assert r["issues"][0]["package"] == "interrupted"
assert r["issues"][0]["kind"] == "reinstreq"
assert r["status_sha256"] and len(r["status_sha256"]) == 64
PY
if "$rpt" dpkg-audit --status "$work/broken-status" --require-clean > /dev/null 2>&1; then
    echo 'M20 --require-clean accepted a broken dpkg database' >&2; exit 1
fi
if "$rpt" apt-simulate-upgrade --status "$work/broken-status" \
    --cache "$work/missing-signed-cache" > /dev/null 2>"$work/refusal.txt"; then
    echo 'M20 upgrade accepted broken dpkg state' >&2; exit 1
fi
grep -q 'dpkg health' "$work/refusal.txt"
cp "$work/clean-status" "$work/duplicate"
cat "$work/clean-status" >> "$work/duplicate"
if "$rpt" dpkg-audit --status "$work/duplicate" >/dev/null 2>&1; then
    echo 'M20 audit accepted duplicate package identities' >&2; exit 1
fi
ln -s "$work/clean-status" "$work/status-link"
if "$rpt" dpkg-audit --status "$work/status-link" >/dev/null 2>&1; then
    echo 'M20 audit accepted symlinked dpkg status' >&2; exit 1
fi
for operation in plan-system-upgrade-policy plan-auto-full-upgrade; do
    if "$rpt" "$operation" --status "$work/broken-status" \
        --cache "$work/missing-signed-cache" >/dev/null 2>"$work/refusal.txt"; then
        echo "M21 $operation accepted broken original dpkg state" >&2; exit 1
    fi
    grep -q 'dpkg health' "$work/refusal.txt"
done
if "$rpt" plan-full-upgrade-scenario --status "$work/broken-status" \
    --cache "$work/missing-signed-cache" --remove installed \
    >/dev/null 2>"$work/refusal.txt"; then
    echo 'M21 explicit removals masked broken original dpkg state' >&2; exit 1
fi
grep -q 'dpkg health' "$work/refusal.txt"
# M28: direct install policy is not an escape hatch around the same broken
# dpkg status checked by apt-simulate and full-upgrade.
if "$rpt" plan-install-batch-policy --status "$work/broken-status" \
    --cache "$work/missing-signed-cache" --architecture amd64 installed \
    >/dev/null 2>"$work/refusal.txt"; then
    echo 'M28 signed native install accepted broken dpkg state' >&2; exit 1
fi
grep -q 'dpkg health' "$work/refusal.txt"
if "$rpt" plan-install-foreign-policy --status "$work/broken-status" \
    --cache "$work/missing-signed-cache" \
    --native-architecture amd64 --foreign-architecture i386 installed:i386 \
    >/dev/null 2>"$work/refusal.txt"; then
    echo 'M28 signed foreign install accepted broken dpkg state' >&2; exit 1
fi
grep -q 'dpkg health' "$work/refusal.txt"
# The real QEMU crash oracle found dpkg-query overlays numbered updates/0000
# while RPT formerly inspected only stale, apparently clean raw status bytes.
mkdir -p "$work/pending/updates"
cp "$work/clean-status" "$work/pending/status"
cat >"$work/pending/updates/0000" <<'EOF'
Package: installed
Architecture: amd64
Version: 2.0
Status: install ok half-configured

EOF
pending_sha="$(sha256sum "$work/pending/status" | cut -d' ' -f1)"
"$rpt" dpkg-audit --status "$work/pending/status" >"$work/pending.json"
python3 - "$work/pending.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert not r["healthy"] and r["pending_status_updates"] == 1, r
assert any(issue["kind"] == "pending_status_updates" for issue in r["issues"])
assert r["host_execution_allowed"] is False
PY
if "$rpt" dpkg-audit --status "$work/pending/status" --require-clean >/dev/null 2>&1; then
    echo 'P0: RPT accepted a dpkg database with unmerged numbered updates' >&2; exit 1
fi
if "$rpt" plan-install-batch-policy --status "$work/pending/status" \
    --cache "$work/missing-signed-cache" --architecture amd64 installed \
    >/dev/null 2>"$work/refusal.txt"; then
    echo 'P0: signed planner trusted stale dpkg status with numbered updates' >&2; exit 1
fi
grep -q 'dpkg health' "$work/refusal.txt"
if "$rpt" apt-simulate-remove --status "$work/pending/status" installed \
    >/dev/null 2>"$work/refusal.txt"; then
    echo 'P0: APT remove simulator ignored pending dpkg updates' >&2; exit 1
fi
grep -q 'dpkg health' "$work/refusal.txt"
test "$(sha256sum "$work/pending/status" | cut -d' ' -f1)" = "$pending_sha"
test "$(sha256sum "$work/broken-status" | cut -d' ' -f1)" = "$before"
echo 'PASS M20/M21/M28/M31 dpkg status and numbered updates journal preflights'

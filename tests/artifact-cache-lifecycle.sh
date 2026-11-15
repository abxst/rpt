#!/usr/bin/env bash
# M18: non-APT owned artifact cache lifecycle and safe cleanup acceptance.
set -euo pipefail
test "$(id -u)" -ne 0
rpt="${1:-./target/debug/rpt}"
work="$(mktemp -d /tmp/rpt-m18-archive.XXXXXXXX)"
trap 'rm -rf -- "$work"' EXIT
root="$work/rpt-artifacts"
"$rpt" artifact-cache-init --root "$root" >"$work/created.json"
python3 - "$work/created.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["initialized"] and r["applied"]
assert r["host_package_execution_allowed"] is False
assert r["operation"] == "initialize"
PY
test "$(cat "$root/.rpt-artifact-cache")" = 'rpt-artifact-cache-v1'
mkdir -p "$root/pool/main"
printf 'abc' >"$root/pool/main/demo.deb"
printf 'x' >"$root/pool/main/demo.deb.rpt-77.part"
printf 'keep' >"$root/pool/main/README"
"$rpt" artifact-cache-clean --root "$root" >"$work/dry.json"
python3 - "$work/dry.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["candidate_files"]==2 and r["candidate_bytes"]==4, r
assert r["applied"] is False
PY
test -f "$root/pool/main/demo.deb"
"$rpt" artifact-cache-clean --root "$root" --apply >"$work/applied.json"
python3 - "$work/applied.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["candidate_files"]==2 and r["applied"] is True
assert r["host_package_execution_allowed"] is False
PY
test ! -e "$root/pool/main/demo.deb"
test ! -e "$root/pool/main/demo.deb.rpt-77.part"
test "$(cat "$root/pool/main/README")" = keep
test -f "$root/.rpt-artifact-cache"
test -f "$root/.rpt-artifact-lock"
printf 'keep-this-too' >"$root/pool/main/safe.deb"
printf 'external' >"$work/external"
ln -s "$work/external" "$root/pool/main/symlink.deb"
if "$rpt" artifact-cache-clean --root "$root" --apply >"$work/unsafe.json" 2>/dev/null; then
  echo 'M18 accepted symlinked cache entry' >&2; exit 1
fi
test -f "$root/pool/main/safe.deb"
test "$(cat "$work/external")" = external
rm "$root/pool/main/symlink.deb"
ln "$work/external" "$root/pool/main/hardlinked.deb"
if "$rpt" artifact-cache-clean --root "$root" --apply >"$work/unsafe.json" 2>/dev/null; then
  echo 'M18 accepted outside hardlink alias' >&2; exit 1
fi
test -f "$root/pool/main/safe.deb"
test "$(cat "$work/external")" = external
if "$rpt" artifact-cache-clean --root /var/cache/apt/archives --apply >"$work/unsafe.json" 2>/dev/null; then
  echo 'M18 accepted host APT archive root' >&2; exit 1
fi
echo 'PASS M18 marked RPT-only archive lifecycle; dry-run, explicit clean and host alias refusal'

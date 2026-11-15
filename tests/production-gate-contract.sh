#!/usr/bin/env bash
# M12 synthetic protocol contract only. No synthetic pass means real APT parity.
set -euo pipefail
gate="${1:-scripts/production-gate.py}"
tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT
commit=0123456789abcdef0123456789abcdef01234567
if python3 "$gate" --expected-commit "$commit" --require-complete >"$tmp/blocked.json"; then
  echo "missing acceptance evidence unexpectedly passed" >&2; exit 1
else
  status=$?
  test "$status" -eq 3
fi
python3 - "$tmp" "$commit" <<'PY'
import hashlib
import json
from pathlib import Path
import sys

root = Path(sys.argv[1])
commit = sys.argv[2]
def witness(name):
    path = root / name
    path.write_text("SYNTHETIC FIXTURE ONLY: " + name + "\n")
    return {"file": name, "sha256": hashlib.sha256(path.read_bytes()).hexdigest()}

gates = {gate: {"status": "pass",
                "positive": witness(f"{gate}-positive.log"),
                "negative": witness(f"{gate}-negative.log")}
         for gate in "ABCDEFGHI"}
targets = [f"{distro}-{release}-{arch}"
           for distro, release in (
               ("ubuntu", "22.04"), ("ubuntu", "24.04"), ("ubuntu", "26.04"),
               ("debian", "12"), ("debian", "13"))
           for arch in ("amd64", "arm64")]
matrix = [{"target": target, "status": "pass", "isolation": "vm",
           "host_audit": witness("host-" + target + ".log")}
          for target in targets]
(root / "manifest.json").write_text(json.dumps({
    "schema_version": 1, "source_commit": commit,
    "gates": gates, "matrix": matrix}, indent=2))
PY
python3 "$gate" --manifest "$tmp/manifest.json" \
  --expected-commit "$commit" --require-complete >"$tmp/full.json"
python3 - "$tmp/full.json" <<'PY'
import json, sys
result = json.load(open(sys.argv[1]))
assert result["qualification"] == "EVIDENCE_COMPLETE_REVIEW_REQUIRED"
assert not result["missing_parity_gates"]
assert not result["missing_vm_targets"]
assert result["manual_security_review_required"] is True
assert result["host_execution_allowed"] is False
PY

# Missing gate fails closed (exit 3); malicious/tampered witness is invalid (exit 2).
python3 - "$tmp/manifest.json" <<'PY'
import json, sys
path = sys.argv[1]
data = json.load(open(path))
data["gates"]["I"] = {"status": "blocked"}
open(path, "w").write(json.dumps(data))
PY
if python3 "$gate" --manifest "$tmp/manifest.json" \
    --expected-commit "$commit" --require-complete >/dev/null 2>&1; then
  echo "missing benchmark gate passed" >&2; exit 1
else
  test "$?" -eq 3
fi
python3 - "$tmp/manifest.json" <<'PY'
import json, sys
path = sys.argv[1]
data = json.load(open(path))
data["gates"]["I"] = {"status": "pass", "positive": {
    "file": "I-positive.log", "sha256": "0" * 64},
    "negative": {"file": "I-negative.log", "sha256": "0" * 64}}
open(path, "w").write(json.dumps(data))
PY
if python3 "$gate" --manifest "$tmp/manifest.json" \
    --expected-commit "$commit" --require-complete >/dev/null 2>&1; then
  echo "tampered evidence hash passed" >&2; exit 1
else
  test "$?" -eq 2
fi

# Current branch is never magically production qualified.
python3 "$gate" --require-complete >"$tmp/current.json" 2>/dev/null && {
  echo "missing production evidence accepted" >&2; exit 1;
} || test "$?" -eq 3
grep -Fq '"host_execution_allowed": false' "$tmp/current.json"
echo "PASS M12 evidence gate negative paths, commit binding, witness hashing and review-only output"

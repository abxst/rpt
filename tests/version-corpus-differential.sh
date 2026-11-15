#!/usr/bin/env bash
# M10 differential corpus: compare RPT's Debian version ordering against dpkg.
# Deterministic, unprivileged, no repository or host package mutations.
set -euo pipefail
rpt="${1:-./target/debug/rpt}"
python3 - "$rpt" <<'PY'
import random
import subprocess
import sys

rpt = sys.argv[1]
rng = random.Random(20261115)
base = [
    "0:1.0", "1.0", "1.0~rc1", "1.0~~pre", "1.0~beta2",
    "1.0+git1", "1.0+git2", "1.0-1", "1.0-2", "1.0-1ubuntu1",
    "1.0-1ubuntu2", "1.0.1", "1.01", "1.1", "1.10",
    "2.0", "2.0~rc2", "2.0-0", "1:1.0", "2:0.1",
    "0:0.9", "0.9", "1.0-1+b1", "1.0-1+b2",
]
cases = {(a, b) for a in base for b in base}
for _ in range(256):
    a = rng.choice(base)
    b = rng.choice(base)
    cases.add((a, b))

for a, b in sorted(cases):
    if subprocess.run(["dpkg", "--compare-versions", a, "lt", b]).returncode == 0:
        expected = -1
    elif subprocess.run(["dpkg", "--compare-versions", a, "gt", b]).returncode == 0:
        expected = 1
    else:
        assert subprocess.run(["dpkg", "--compare-versions", a, "eq", b]).returncode == 0
        expected = 0
    result = subprocess.run([rpt, "version-compare", "--driver", "deb", a, b],
                            capture_output=True, text=True)
    if result.returncode != 0 or result.stdout.strip() != str(expected):
        raise AssertionError(f"version mismatch {a!r} / {b!r}: "
                             f"APT/dpkg={expected}, RPT={result.stdout!r}, "
                             f"stderr={result.stderr!r}")
print(f"PASS M10 deterministic Debian version differential: {len(cases)} ordered pairs")
PY

#!/usr/bin/env bash
# Unprivileged end-to-end checks; no dpkg, mount or network.
set -euo pipefail
dry="${1:-./target/debug/rpt-transaction-dry-run}"
inspect="${2:-./target/debug/rpt-journal-inspect}"
dry="$(realpath "$dry")"
inspect="$(realpath "$inspect")"
tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT
"$dry" --journal-dir "$tmp/journal" --id success
"$inspect" "$tmp/journal" --json > "$tmp/success.json"
grep -q '"phase": "succeeded"' "$tmp/success.json"
if "$dry" --journal-dir "$tmp/journal" --id failure --simulate-failure "simulated failure" > "$tmp/failure.out" 2>&1; then
  echo "expected simulated failure" >&2; exit 1
fi
grep -q 'simulated failure' "$tmp/failure.out"
"$inspect" "$tmp/journal" --json > "$tmp/after-failure.json"
grep -q '"phase": "failed"' "$tmp/after-failure.json"
if "$dry" --journal-dir "$tmp/journal" --id success > "$tmp/duplicate.out" 2>&1; then
  echo "duplicate transaction ID unexpectedly accepted" >&2; exit 1
fi
grep -q 'invalid transaction phase transition' "$tmp/duplicate.out"
mkdir -p "$tmp/orphan"
printf '%s\n' '{"id":"orphan","phase":"running","error":null}' > "$tmp/orphan/orphan.json"
if "$dry" --journal-dir "$tmp/orphan" --id blocked > "$tmp/blocked.out" 2>&1; then
  echo "unfinished journal unexpectedly accepted" >&2; exit 1
fi
test ! -e "$tmp/orphan/blocked.json"
if "$inspect" "$tmp/orphan" --json > "$tmp/orphan.json" 2> "$tmp/orphan.err"; then
  echo "inspector unexpectedly accepted unfinished journal" >&2; exit 1
fi
grep -q '"phase": "running"' "$tmp/orphan.json"
echo "PASS: CLI success, failure, duplicate ID and interrupted journal"

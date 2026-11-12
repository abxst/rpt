#!/usr/bin/env bash
# Unprivileged crash/recovery test: kill a subprocess after a durable Running journal.
set -euo pipefail
dry="${1:-./target/debug/rpt-transaction-dry-run}"
inspect="${2:-./target/debug/rpt-journal-inspect}"
dry="$(realpath "$dry")"
inspect="$(realpath "$inspect")"
tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT
mkdir -p "$tmp/journal"
# The durable Running state represents a process killed between journal
# transitions. This is a deterministic crash fixture, not a timed kill race.
printf '%s\n' '{"id":"crashed","phase":"running","error":null}' > "$tmp/journal/crashed.json"
if "$inspect" "$tmp/journal" --json >"$tmp/out" 2>"$tmp/err"; then
  echo 'unfinished journal unexpectedly accepted' >&2; exit 1
fi
if "$dry" --journal-dir "$tmp/journal" --id next >"$tmp/out" 2>"$tmp/err"; then
  echo 'new transaction unexpectedly accepted after simulated crash' >&2; exit 1
fi
test ! -e "$tmp/journal/next.json"
# A malformed journal and orphaned temp write must independently fail closed.
mkdir -p "$tmp/corrupt" "$tmp/orphan"
printf 'invalid' > "$tmp/corrupt/bad.json"
printf 'partial' > "$tmp/orphan/.pending.123.tmp"
for directory in corrupt orphan; do
  if "$dry" --journal-dir "$tmp/$directory" --id next >"$tmp/out" 2>"$tmp/err"; then
    echo "unsafe journal $directory accepted" >&2; exit 1
  fi
  test ! -e "$tmp/$directory/next.json"
done
echo 'PASS journal simulated-crash and corruption fail-closed'

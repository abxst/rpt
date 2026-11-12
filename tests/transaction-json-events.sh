#!/usr/bin/env bash
set -euo pipefail
binary="$(realpath "${1:-./target/debug/rpt-transaction-dry-run}")"
tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT
"$binary" --journal-dir "$tmp/journal" --id json-success --json 2>"$tmp/success.jsonl"
if "$binary" --journal-dir "$tmp/journal" --id json-fail --simulate-failure 'fixture error' --json 2>"$tmp/failure.jsonl"; then
  echo 'expected nonzero exit for simulated failure' >&2; exit 1
fi
python3 - "$tmp/success.jsonl" "$tmp/failure.jsonl" <<'PY'
import json, sys
success = [json.loads(line) for line in open(sys.argv[1], encoding="utf-8")]
failure = [json.loads(line) for line in open(sys.argv[2], encoding="utf-8")]
assert [event["event"] for event in success] == ["queued", "succeeded"]
assert [event["event"] for event in failure] == ["queued", "failed"]
assert all(event["transaction_id"] == "json-success" and event["mode"] == "dry_run" for event in success)
assert all(event["transaction_id"] == "json-fail" and event["mode"] == "dry_run" for event in failure)
assert success[-1]["error"] is None
assert "fixture error" in failure[-1]["error"]
PY
echo 'PASS transaction JSONL event diagnostics'

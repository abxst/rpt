#!/usr/bin/env bash
# Public CLI must reject unsafe foreign-architecture scopes before any cache I/O.
set -euo pipefail
rpt="${1:?RPT executable required}"
root="$(mktemp -d)"
trap 'rm -rf "$root"' EXIT
: > "$root/status"
"$rpt" --help | grep -Fq 'plan-install-foreign-policy'
expect_reject() {
  local name="$1" expected="$2"
  shift 2
  if "$rpt" plan-install-foreign-policy --cache "$root/missing-cache" \
    --status "$root/status" --apt-root "$root/apt" "$@" \
    >"$root/$name.stdout" 2>"$root/$name.stderr"; then
    echo "Foreign policy CLI unexpectedly accepted $name" >&2
    exit 1
  fi
  grep -Fq "$expected" "$root/$name.stderr"
}
expect_reject same-arch 'distinct native and foreign architectures' \
  --native-architecture amd64 --foreign-architecture amd64 example:amd64
expect_reject all-arch 'invalid concrete preview architecture' \
  --native-architecture amd64 --foreign-architecture all example:all
expect_reject invalid-arch 'invalid concrete preview architecture' \
  --native-architecture amd64 --foreign-architecture invalid/../arch example:invalid
expect_reject zero-limit 'max_packages must be 1..=5000' \
  --native-architecture amd64 --foreign-architecture i386 --max-packages 0 example:i386
echo 'PASS foreign-architecture CLI fail-closed scope validations'

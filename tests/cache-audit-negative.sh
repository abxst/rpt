#!/usr/bin/env bash
# M23: unprivileged offline audit must not trust arbitrary directory names.
set -euo pipefail
test "$(id -u)" -ne 0
rpt="$1"
root="$(mktemp -d /tmp/rpt-m23-audit.XXXXXXXX)"
trap 'rm -rf -- "$root"' EXIT
if "$rpt" cache-audit --cache "$root" >/dev/null 2>&1; then
    echo 'M23 accepted empty cache' >&2; exit 1
fi
mkdir "$root/aaaaaaaaaaaaaaaaaaaaaaaa"
if "$rpt" cache-audit --cache "$root" >/dev/null 2>&1; then
    echo 'M23 accepted repo missing provenance/signature' >&2; exit 1
fi
rmdir "$root/aaaaaaaaaaaaaaaaaaaaaaaa"
ln -s /tmp "$root/aaaaaaaaaaaaaaaaaaaaaaaa"
if "$rpt" cache-audit --cache "$root" >/dev/null 2>&1; then
    echo 'M23 followed symlink repository cache entry' >&2; exit 1
fi
if "$rpt" cache-audit --cache "$root/aaaaaaaaaaaaaaaaaaaaaaaa" >/dev/null 2>&1; then
    echo 'M23 followed symlink cache root' >&2; exit 1
fi
echo 'PASS M23 offline empty/unsigned/symlink cache rejection'

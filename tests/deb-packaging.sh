#!/usr/bin/env bash
# M25: build and unpack experimental .deb; no host package mutation.
set -euo pipefail
test "$(id -u)" -ne 0
test "$#" -eq 1
binary="$1"
tmp="$(mktemp -d /tmp/rpt-m25-package.XXXXXXXX)"
trap 'rm -rf -- "$tmp"' EXIT
if [[ -f /var/lib/dpkg/status ]]; then
  before="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)"
else
  before=""
fi
bash scripts/build-deb.sh "$binary" "$tmp/packages" > "$tmp/built.txt"
package="$(cat "$tmp/built.txt")"
test -f "$package"
test "$(dpkg-deb -f "$package" Package)" = rpt
case "$(dpkg-deb -f "$package" Version)" in
  *'~experimental1') ;;
  *) echo 'M25 package claims a non-experimental version' >&2; exit 1 ;;
esac
test "$(dpkg-deb -f "$package" Architecture)" = "$(dpkg --print-architecture)"
mkdir "$tmp/unpacked"
dpkg-deb -x "$package" "$tmp/unpacked"
test -x "$tmp/unpacked/usr/bin/rpt"
test ! -e "$tmp/unpacked/usr/bin/apt"
test ! -e "$tmp/unpacked/usr/bin/apt-get"
test ! -e "$tmp/unpacked/usr/bin/dpkg"
test ! -e "$tmp/unpacked/DEBIAN"
"$tmp/unpacked/usr/bin/rpt" --version | grep -q '^rpt '
if bash scripts/build-deb.sh "$binary" "$tmp/packages" >/dev/null 2>&1; then
  echo 'M25 package builder overwrote an existing archive' >&2; exit 1
fi
if [[ -n "$before" ]]; then
  test "$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)" = "$before"
fi
echo 'PASS M25 native Debian package ELF/arch guard and unpack-only host-safe acceptance'

#!/usr/bin/env bash
# Build an experimental rpt .deb; NEVER install it or shadow system apt.
set -euo pipefail
export LC_ALL=C
if [[ $# -ne 2 ]]; then
  echo "Usage: $0 /path/to/native/rpt-binary output-directory" >&2
  exit 2
fi
binary="$(realpath -- "$1")"
outdir="$2"
[[ -f "$binary" && -x "$binary" && ! -L "$binary" ]] || {
  echo 'rpt binary must be an ordinary executable file' >&2; exit 1;
}
command -v readelf >/dev/null
command -v dpkg-deb >/dev/null
architecture="$(dpkg --print-architecture)"
machine="$(readelf -h "$binary" | sed -n 's/^ *Machine: *//p')"
case "$architecture:$machine" in
  'amd64:Advanced Micro Devices X86-64'|'arm64:AArch64') ;;
  *) echo "ELF machine $machine does not match native Debian architecture $architecture" >&2; exit 1 ;;
esac
cd "$(dirname -- "$0")/.."
base_version="$(sed -nE 's/^version *= *"([0-9]+\.[0-9]+\.[0-9]+)"$/\1/p' Cargo.toml | head -1)"
[[ "$base_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo 'invalid package version in Cargo.toml' >&2; exit 1;
}
version="$base_version~experimental1"
mkdir -p -- "$outdir"
outdir="$(realpath -- "$outdir")"
printf -v filename 'rpt_%s_%s.deb' "$version" "$architecture"
output="$outdir/$filename"
[[ ! -e "$output" && ! -L "$output" ]] || {
  echo "refusing to overwrite Debian package: $output" >&2; exit 1;
}
stage="$(mktemp -d)"
trap 'rm -rf -- "$stage"' EXIT
install -d -m 0755 "$stage/DEBIAN" "$stage/usr/bin"
install -m 0755 "$binary" "$stage/usr/bin/rpt"
size="$(du -k "$stage/usr/bin/rpt" | awk '{print $1}')"
# Bind libc dependency to the actual native ELF symbol floor; avoid claiming
# that a binary linked on a newer distro runs on an older libc.
minimum_glibc="$(readelf --version-info "$binary" |
  grep -oE 'GLIBC_[0-9]+\.[0-9]+' |
  sed 's/GLIBC_//' | sort -Vu | tail -1)"
[[ "$minimum_glibc" =~ ^[0-9]+\.[0-9]+$ ]] || {
  echo 'unable to determine ELF GLIBC runtime floor' >&2; exit 1;
}
cat > "$stage/DEBIAN/control" <<EOF
Package: rpt
Version: $version
Architecture: $architecture
Maintainer: RPT project maintainers
Installed-Size: $size
Section: admin
Priority: optional
Depends: libc6 (>= $minimum_glibc), liblzma5 | liblzma5t64, gpgv, ca-certificates
Description: Experimental Rust package transport and APT subset engine
 This package installs only the standalone rpt CLI under /usr/bin/rpt.
 It does not replace apt, apt-get, apt-cache, dpkg, or system services.
 This build is not approved to perform live host package transactions.
EOF
chmod 0644 "$stage/DEBIAN/control"
dpkg-deb --root-owner-group --build "$stage" "$output" >/dev/null
echo "$output"

#!/usr/bin/env bash
# Read-only differential check against Ubuntu APT metadata on the CI runner.
set -euo pipefail
export LC_ALL=C
repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmp="$(mktemp -d)"
trap 'rm -rf -- "$tmp"' EXIT
shopt -s nullglob

# The system APT often stores indexes in .lz4 files; RPT currently only
# accepts uncompressed indexes. Decompress into a separate temporary copy.
count=0
for path in /var/lib/apt/lists/*_binary-amd64_Packages /var/lib/apt/lists/*_binary-amd64_Packages.lz4 /var/lib/apt/lists/*_binary-amd64_Packages.gz; do
  [[ -f "$path" ]] || continue
  name="$(basename "$path")"
  name="${name%.lz4}"
  name="${name%.gz}"
  case "$path" in
    *.lz4) lz4 -dc -- "$path" > "$tmp/$name" ;;
    *.gz) gzip -dc -- "$path" > "$tmp/$name" ;;
    *) cp -- "$path" "$tmp/$name" ;;
  esac
  ((count+=1))
done
if (( count == 0 )); then
  echo 'FAIL: Ubuntu runner lacks APT package indexes' >&2
  exit 1
fi
echo "Indexes ready: $count"
tested=0
for pkg in bash coreutils curl openssl python3; do
  # apt-cache madison enumerates repository versions, excluding installed-only versions.
  apt-cache madison "$pkg" | awk -F'|' -v pkg="$pkg" '
    { name=$1; ver=$2; gsub(/^[ \t]+|[ \t]+$/, "", name); gsub(/^[ \t]+|[ \t]+$/, "", ver);
      if (name == pkg && ver != "") print ver }
  ' | sort -u > "$tmp/apt-$pkg"
  "$repo_root/target/debug/rpt" index-versions --lists-dir "$tmp" --package "$pkg" --architecture amd64 | sort -u > "$tmp/rpt-$pkg"
  if [[ ! -s "$tmp/apt-$pkg" ]]; then
    echo "FAIL: apt-cache madison provided no versions for $pkg" >&2
    exit 1
  fi
  if ! diff -u "$tmp/apt-$pkg" "$tmp/rpt-$pkg"; then
    echo "FAIL: RPT and APT version sets differ for $pkg" >&2
    exit 1
  fi
  printf 'PASS %-12s %s version(s)\n' "$pkg" "$(wc -l < "$tmp/rpt-$pkg")"
  ((tested+=1))
done
echo "PASS: $tested packages match apt-cache madison across $count real indexes"

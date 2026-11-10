#!/usr/bin/env bash
# Safe APT migration preparation. This intentionally NEVER replaces /usr/bin/apt:
# RPT does not yet implement APT's command and option contract.
set -euo pipefail
TAG="${RPT_TAG:-v0.1.0-alpha.13}"
PREFIX="${RPT_PREFIX:-/opt/rpt}"
APT_PATH="/usr/bin/apt"
ASSET_BASE="https://github.com/abxst/rpt/releases/download/${TAG}"
usage() {
  cat <<'USAGE'
Usage: sudo bash scripts/stage-apt.sh {stage|status|restore|activate}
  stage     Download/verify RPT and save aptbak; leave system apt unchanged.
  status    Check original apt, backup and staged RPT.
  restore   Restore /usr/bin/apt from aptbak ONLY if it is an RPT symlink.
  activate  Refused: RPT is not yet CLI/behavior compatible with APT.
Environment: RPT_TAG (release tag), RPT_PREFIX (default /opt/rpt).
USAGE
}
require_root() { if (( EUID != 0 )); then echo "Requires root (sudo)" >&2; exit 1; fi; }
backup_path() { printf '%s/aptbak' "$PREFIX"; }
status() {
  printf 'apt: '; ls -l "$APT_PATH"
  printf 'backup: '; if [[ -e "$(backup_path)" ]]; then ls -l "$(backup_path)"; else echo 'not staged'; fi
  printf 'rpt: '; if [[ -x "$PREFIX/rpt" ]]; then "$PREFIX/rpt" --version; else echo 'not staged'; fi
  echo 'APT replacement: unsupported until APT parity is fully verified'
}
stage() {
  require_root
  [[ -f "$APT_PATH" && ! -L "$APT_PATH" ]] || { echo "apt must be a regular executable; refusing" >&2; exit 1; }
  mkdir -p "$PREFIX"
  if [[ ! -e "$(backup_path)" ]]; then
    cp -p -- "$APT_PATH" "$(backup_path)"
  else
    cmp -s -- "$APT_PATH" "$(backup_path)" || { echo "aptbak differs from apt; refusing overwrite" >&2; exit 1; }
  fi
  local tmp
  tmp="$(mktemp -d)"
  trap 'rm -rf -- "$tmp"' RETURN
  curl -fL --retry 3 --proto '=https' --tlsv1.2 "$ASSET_BASE/rpt-linux-amd64.tar.gz" -o "$tmp/rpt-linux-amd64.tar.gz"
  curl -fL --retry 3 --proto '=https' --tlsv1.2 "$ASSET_BASE/SHA256SUMS" -o "$tmp/SHA256SUMS"
  (cd "$tmp" && sha256sum -c SHA256SUMS)
  tar -tzf "$tmp/rpt-linux-amd64.tar.gz" | grep -qx 'rpt' || { echo "unexpected release archive" >&2; exit 1; }
  tar -xzf "$tmp/rpt-linux-amd64.tar.gz" -C "$tmp" -- rpt
  install -m 0755 "$tmp/rpt" "$PREFIX/rpt"
  "$PREFIX/rpt" --help >/dev/null
  echo "Staged $TAG in $PREFIX; apt remains unchanged."
}
restore() {
  require_root
  [[ -f "$(backup_path)" ]] || { echo "Missing aptbak" >&2; exit 1; }
  if [[ -L "$APT_PATH" && "$(readlink "$APT_PATH")" == "$PREFIX/rpt" ]]; then
    rm -- "$APT_PATH"
    cp -p -- "$(backup_path)" "$APT_PATH"
    echo "Original apt restored."
  elif [[ -f "$APT_PATH" ]] && cmp -s -- "$APT_PATH" "$(backup_path)"; then
    echo "Original apt already present; no changes."
  else
    echo "apt is not the expected RPT link or backup; refusing destructive restore" >&2
    exit 1
  fi
}
case "${1:-}" in
  stage) stage ;;
  status) status ;;
  restore) restore ;;
  activate) echo "BLOCKED: RPT lacks APT 1:1 command/argument semantics and dpkg transaction support. /usr/bin/apt untouched." >&2; exit 2 ;;
  *) usage; exit 2 ;;
esac

#!/usr/bin/env bash
# M17 policy syntax oracle; works entirely against disposable APT config files.
set -euo pipefail
test "$(id -u)" -ne 0
rpt="${1:-./target/debug/rpt}"
work="$(mktemp -d /tmp/rpt-m17-apt-conf.XXXXXXXX)"
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/apt.conf.d"
cat > "$work/apt.conf.d/10base" <<'EOF'
/* ignored Default-Release from comment */
Dpkg { Progress-Fancy "true"; };
APT { Default-Release "noble"; }; // inline
EOF
"$rpt" apt-config-inspect --apt-root "$work" >"$work/nested.json"
python3 - "$work/nested.json" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
assert r["default_release"]=="noble",r
assert r["supported_configuration_subset"] is True
assert r["host_execution_allowed"] is False
assert r["apt_parity_verified"] is False
PY
# APT's flat :: form must be equivalent to a nested scope.
printf '# harmless comment\nAPT::Default-Release "noble-updates";\n' >"$work/apt.conf.d/10base"
"$rpt" apt-config-inspect --apt-root "$work" >"$work/flat.json"
python3 - "$work/flat.json" <<'PY'
import json,sys
assert json.load(open(sys.argv[1]))["default_release"]=="noble-updates"
PY
for invalid in \
  '#include "/tmp/host";' \
  'Dir::Etc::preferences "/tmp/host";' \
  'APT::Install-Recommends "false";' \
  'APT { Default-Release "noble";' \
  'APT::Default-Release "noble"; APT::Default-Release "stable";'; do
  printf '%s\n' "$invalid" >"$work/apt.conf.d/10base"
  if "$rpt" apt-config-inspect --apt-root "$work" >"$work/untrusted.json" 2>/dev/null; then
    echo "M17 accepted unsupported APT config: $invalid" >&2
    exit 1
  fi
done
printf 'APT::Default-Release "noble";\n' >"$work/apt.conf.d/10base"
printf 'APT::Default-Release "stable";\n' >"$work/apt.conf.d/20second"
if "$rpt" apt-config-inspect --apt-root "$work" >"$work/untrusted.json" 2>/dev/null; then
  echo 'M17 accepted duplicate target releases across config fragments' >&2
  exit 1
fi
rm "$work/apt.conf.d/20second"
mv "$work/apt.conf.d/10base" "$work/outside.conf"
ln -s "$work/outside.conf" "$work/apt.conf.d/10base"
if "$rpt" apt-config-inspect --apt-root "$work" >"$work/untrusted.json" 2>/dev/null; then
  echo 'M17 followed symlinked APT config fragment' >&2
  exit 1
fi
echo 'PASS M17 apt.conf flat/nested/comments/duplicate/unknown/include/symlink CLI oracle'

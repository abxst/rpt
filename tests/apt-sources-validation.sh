#!/usr/bin/env bash
# M19: validate signed sources only, without HTTP or dpkg/APT mutation.
set -euo pipefail
test "$(id -u)" -ne 0
rpt="${1:-./target/debug/rpt}"
native="$(dpkg --print-architecture)"
work="$(mktemp -d /tmp/rpt-m19-sources.XXXXXXXX)"
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/sources.list.d"
cat >"$work/sources.list" <<EOF
deb [arch=$native signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg trusted=no] https://example.org/ubuntu noble main
EOF
cat >"$work/sources.list.d/extra.sources" <<EOF
# Audited source
Types: deb
URIs: https://mirror.example.org/ubuntu
Suites: noble
Components: main universe
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
Architectures: $native
Enabled: yes
Trusted: no
EOF
before="$(sha256sum "$work/sources.list" | awk '{print $1}')"
"$rpt" sources-validate --root "$work" >"$work/valid.json"
python3 - "$work/valid.json" "$native" <<'PY'
import json,sys
r=json.load(open(sys.argv[1]))
native=sys.argv[2]
assert r["schema_version"]==1 and r["host_execution_allowed"] is False
assert r["apt_parity_verified"] is False
assert r["validated_repository_subset"] is True
assert len(r["repositories"])==2, r
for repo in r["repositories"]:
    assert repo["architectures"]==[native], repo
    assert repo["keyring"]=="/usr/share/keyrings/ubuntu-archive-keyring.gpg"
PY
for bad in \
  "deb [signed-by=/tmp/key.gpg trusted=yes] https://example.org/ubuntu noble main" \
  "deb [signed-by=/tmp/key.gpg check-valid-until=no] https://example.org/ubuntu noble main" \
  "deb [signed-by=/tmp/key.gpg arch+=amd64] https://example.org/ubuntu noble main" \
  "deb https://example.org/ubuntu noble main"; do
    printf '%s\n' "$bad" >"$work/sources.list"
    if "$rpt" sources-validate --root "$work" >"$work/rejected.json" 2>/dev/null; then
      echo "M19 accepted unsafe legacy source: $bad" >&2; exit 1
    fi
done
printf 'deb [arch=%s signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://example.org/ubuntu noble main\n' "$native" >"$work/sources.list"
cat >>"$work/sources.list.d/extra.sources" <<'EOF'
Check-Valid-Until: no
EOF
if "$rpt" sources-validate --root "$work" >"$work/rejected.json" 2>/dev/null; then
  echo 'M19 accepted unsupported Deb822 field' >&2; exit 1
fi
sed -i '/^Check-Valid-Until:/d' "$work/sources.list.d/extra.sources"
cat >"$work/sources.list.d/conflicting.sources" <<'EOF'
Types: deb
URIs: https://example.org/ubuntu
Suites: noble
Components: universe
Signed-By: /tmp/rogue-key.gpg
EOF
if "$rpt" sources-validate --root "$work" >"$work/rejected.json" 2>/dev/null; then
  echo 'M19 accepted conflicting signed repository definitions' >&2; exit 1
fi
rm "$work/sources.list.d/conflicting.sources"
mv "$work/sources.list.d/extra.sources" "$work/elsewhere.sources"
ln -s "$work/elsewhere.sources" "$work/sources.list.d/extra.sources"
if "$rpt" sources-validate --root "$work" >"$work/rejected.json" 2>/dev/null; then
  echo 'M19 followed symlinked signed source file' >&2; exit 1
fi
test "$(sha256sum "$work/sources.list" | awk '{print $1}')" = \
  "$(printf 'deb [arch=%s signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg] https://example.org/ubuntu noble main\n' "$native" | sha256sum | awk '{print $1}')"
test -n "$before"
echo 'PASS M19 signed APT source syntax, native-arch filtering, hostile option/alias refusal'

#!/usr/bin/env bash
# M8: deliberate dpkg fault injection against a signed M7 combined transaction.
# This MUST be run only by tests/signed-plan-differential.sh on a disposable VM.
# It never writes host dpkg/APT state, clears a journal or rolls back in place.
set -euo pipefail

work="${1:?signed differential workspace required}"
arch="${2:?target architecture required}"
rpt="${3:-./target/debug/rpt}"
baseline="$work/native-combined-restored"
artifacts="$work/native-combined-artifacts"
cache="$work/rpt-cache-v2"
apt_root="$work/rpt-apt"
test -d "$baseline"
test -d "$artifacts"
test -x "$rpt"
test "$(id -u)" -ne 0
test "$arch" = "$(dpkg --print-architecture)"
test "$work" != / && [[ "$work" == /tmp/rpt-signed.* ]] && [[ ! -L "$work" ]]

host_status="$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)"
host_marks=missing
if [[ -f /var/lib/apt/extended_states ]]; then
  host_marks="$(sha256sum /var/lib/apt/extended_states | cut -d' ' -f1)"
fi
host_holds="$(apt-mark showhold | LC_ALL=C sort)"
host_audit="$(dpkg --audit)"
check_host() {
  local marks=missing
  [[ ! -f /var/lib/apt/extended_states ]] ||
    marks="$(sha256sum /var/lib/apt/extended_states | cut -d' ' -f1)"
  if [[ "$(sha256sum /var/lib/dpkg/status | cut -d' ' -f1)" != "$host_status" ||
        "$marks" != "$host_marks" ||
        "$(apt-mark showhold | LC_ALL=C sort)" != "$host_holds" ||
        "$(dpkg --audit)" != "$host_audit" ]]; then
    echo "M8 fault injection modified the CI host dpkg/APT state" >&2
    return 1
  fi
}
trap check_host EXIT

baseline_status="$(sudo sha256sum "$baseline/var/lib/dpkg/status" | cut -d' ' -f1)"
test "$(sudo dpkg-query --admindir="$baseline/var/lib/dpkg" \
  -W '-f=${Status}|${Version}' rpt-sandbox-demo)" = 'install ok installed|1.0'
test "$(sudo dpkg-query --admindir="$baseline/var/lib/dpkg" \
  -W '-f=${Status}|${Version}' rpt-sandbox-obsolete)" = 'install ok installed|1.0'
test -z "$(sudo dpkg --root="$baseline" --audit)"

# Each case gets a fresh root, exact per-root signed intent, approval, journal,
# mandatory checkpoint and independently restored NEW guest.
for mode in prerm-failure sigterm-postinst sigkill-postinst; do
  guest="$work/native-chaos-$mode"
  checkpoint="$work/native-chaos-$mode-checkpoint"
  restored="$work/native-chaos-$mode-restored"
  journal="$work/native-chaos-$mode-journal"
  intent="$work/native-chaos-$mode-intent.json"
  id="chaos-$mode"
  log="$work/native-chaos-$mode.log"
  sudo cp -a -- "$baseline" "$guest"
  test "$(sudo sha256sum "$guest/var/lib/dpkg/status" | cut -d' ' -f1)" = "$baseline_status"
  if [[ "$mode" == prerm-failure ]]; then
    sudo touch "$guest/etc/rpt-ci-fail-remove"
  else
    sudo touch "$guest/etc/rpt-ci-hold-postinst"
  fi
  test ! -e "$guest/tmp/rpt-upgrade-postinst-running"

  "$rpt" native-rootfs-prepare-full-upgrade \
    --rootfs "$guest" --cache "$cache" --apt-root "$apt_root" \
    --architecture "$arch" --max-roots 128 --max-packages 200 \
    --max-removals 3 --automatic --output "$intent"
  python3 - "$intent" "$guest" <<'PY'
import json, sys
x = json.load(open(sys.argv[1]))
assert x["session"] == "native-rootfs:" + sys.argv[2]
assert x["selection_mode"] == "automatic"
assert [(r["name"], r["installed_version"]) for r in x["selected_removals"]] == [
    ("rpt-sandbox-obsolete", "1.0")]
assert [(p["name"], p["version"]) for p in x["packages"]] == [
    ("rpt-sandbox-helper", "1.0"), ("rpt-sandbox-demo", "2.0")]
assert x["executable"] is False
PY
  approved="$(sha256sum "$intent" | cut -d' ' -f1)"
  args=(native-rootfs-apply-full-upgrade --rootfs "$guest" --id "$id"
    --transaction "$intent" --approve-intent-sha256 "$approved"
    --checkpoint-rootfs "$checkpoint" --artifacts "$artifacts"
    --cache "$cache" --apt-root "$apt_root"
    --journal-dir "$journal" --timeout-seconds 180)

  if [[ "$mode" == prerm-failure ]]; then
    if sudo "$rpt" "${args[@]}" >"$log" 2>&1; then
      echo "M8 prerm injection unexpectedly committed" >&2
      exit 1
    fi
    grep -Fq 'RPT M8 deliberate prerm failure' "$log"
    expected_phase=failed
  else
    # New session keeps the test process and all of its supervised child
    # process groups distinct from the runner shell. The PID file is written
    # by the shell that execs RPT (not by sudo).
    pidfile="$work/native-chaos-$mode.pid"
    sudo -n setsid sh -c 'printf "%s\n" "$$" > "$1"; shift; exec "$@"' \
      m8-launch "$pidfile" "$rpt" "${args[@]}" >"$log" 2>&1 &
    launcher=$!
    ready=false
    for ((n=0; n<1200; n++)); do
      if [[ -e "$guest/tmp/rpt-upgrade-postinst-running" ]]; then
        ready=true
        break
      fi
      # sudo/setsid may detach even while RPT and its guest dpkg live.
      # Observe the actual exec PID, never the possibly exited launcher.
      if [[ -s "$pidfile" ]]; then
        candidate="$(cat "$pidfile")"
        if [[ "$candidate" =~ ^[1-9][0-9]*$ ]] &&
           ! ps -p "$candidate" -o pid= >/dev/null 2>&1; then
          break
        fi
      fi
      sleep 0.1
    done
    if [[ "$ready" != true || ! -s "$pidfile" ]]; then
      echo "M8 postinst fault target never entered its guarded sleep" >&2
      cat "$log" >&2
      wait "$launcher" || :
      exit 1
    fi
    target="$(cat "$pidfile")"
    if [[ ! "$target" =~ ^[1-9][0-9]*$ ]] ||
       [[ "$(ps -o sid= -p "$target" | tr -d ' ')" != "$target" ]]; then
      echo "M8 refusing signal: target is not a dedicated session leader" >&2
      wait "$launcher" || :
      exit 1
    fi
    if [[ "$mode" == sigterm-postinst ]]; then
      sudo kill -TERM "$target"
      expected_phase=interrupted
    else
      # SIGKILL bypasses RPT's signal handler. Kill the entire *dedicated*
      # test session, not the runner process group, so dpkg/postinst cannot
      # continue modifying the guest after recovery inspection.
      sudo pkill -KILL -s "$target"
      expected_phase=running
    fi
    wait "$launcher" || :
    # A killed dpkg may briefly remain as an already-dead zombie. Only
    # executable/non-zombie descendants could mutate the guest after kill.
    session_has_live_process() {
      ps -e -o sid=,stat= | awk -v sid="$target" \
        '$1 == sid && $2 !~ /^Z/ { found=1 } END { exit !found }'
    }
    for ((n=0; n<100; n++)); do
      if ! session_has_live_process; then break; fi
      sleep 0.1
    done
    if session_has_live_process; then
      echo "M8 postinst descendants remain alive; unsafe to inspect/restore" >&2
      exit 1
    fi
  fi

  record="$journal/$id.json"
  test -s "$record"
  python3 - "$record" "$expected_phase" <<'PY'
import json, sys
r = json.load(open(sys.argv[1]))
assert r["phase"] == sys.argv[2], r
if r["phase"] in ("failed", "interrupted"):
    assert r["error"]
else:
    assert r["error"] is None
PY
  guest_after_fault="$(sudo sha256sum "$guest/var/lib/dpkg/status" | cut -d' ' -f1)"
  sudo "$rpt" native-rootfs-recovery-inspect \
    --rootfs "$guest" --journal-dir "$journal" \
    >"$work/native-chaos-$mode-recovery.json"
  python3 - "$work/native-chaos-$mode-recovery.json" "$id" "$expected_phase" <<'PY'
import json, sys
x = json.load(open(sys.argv[1]))
assert x["needs_operator_review"] is True, x
r = [a for a in x["journal"]["records"] if a["id"] == sys.argv[2]]
assert len(r) == 1 and r[0]["phase"] == sys.argv[3], x
assert x["journal"]["requires_manual_recovery"] == (sys.argv[3] != "failed")
PY
  # Read-only inspection does not repair dpkg, clear the journal or mutate
  # the original failed guest, even for a clean-looking failed pre-remove.
  test "$(sudo sha256sum "$guest/var/lib/dpkg/status" | cut -d' ' -f1)" = "$guest_after_fault"
  test -s "$record"
  receipt="$journal/native-rootfs-snapshots/$id-before.json"
  test -s "$receipt"
  sudo "$rpt" native-rootfs-snapshot-verify --receipt "$receipt" \
    >"$work/native-chaos-$mode-checkpoint-verify.json"
  sudo "$rpt" native-rootfs-snapshot-restore \
    --receipt "$receipt" --new-rootfs "$restored" \
    >"$work/native-chaos-$mode-restore.json"
  test "$(sudo sha256sum "$restored/var/lib/dpkg/status" | cut -d' ' -f1)" = "$baseline_status"
  test -z "$(sudo dpkg --root="$restored" --audit)"
  test "$(sudo dpkg-query --admindir="$restored/var/lib/dpkg" \
    -W '-f=${Status}|${Version}' rpt-sandbox-demo)" = 'install ok installed|1.0'
  test "$(sudo dpkg-query --admindir="$restored/var/lib/dpkg" \
    -W '-f=${Status}|${Version}' rpt-sandbox-obsolete)" = 'install ok installed|1.0'
  test "$(sudo sha256sum "$guest/var/lib/dpkg/status" | cut -d' ' -f1)" = "$guest_after_fault"
  check_host
  echo "PASS M8 $mode: $expected_phase journal, recovery review, sealed checkpoint, new clean guest, host unchanged"

  if [[ "$mode" == sigkill-postinst ]]; then
    # A mutable snapshot is NOT trustworthy: refuse both verification and
    # restore after a single-byte change. Never heal the evidence in place.
    sudo sh -c 'printf "\nM8-corruption\n" >> "$1"' \
      sh "$checkpoint/var/lib/dpkg/status"
    if sudo "$rpt" native-rootfs-snapshot-verify --receipt "$receipt" \
      >"$work/native-chaos-tamper-verify.log" 2>&1; then
      echo "M8 accepted a corrupted checkpoint" >&2; exit 1
    fi
    if sudo "$rpt" native-rootfs-snapshot-restore \
      --receipt "$receipt" --new-rootfs "$work/native-chaos-tampered-new" \
      >"$work/native-chaos-tamper-restore.log" 2>&1; then
      echo "M8 restored a corrupted checkpoint" >&2; exit 1
    fi
    test ! -e "$work/native-chaos-tampered-new"
    echo 'PASS M8 corrupted checkpoint rejected without creating a new guest'
  fi
done
echo 'PASS M8 native signed failure/TERM/KILL recovery acceptance'

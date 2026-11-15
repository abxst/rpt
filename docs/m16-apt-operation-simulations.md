# M16 — Unified APT-like operation simulations (functionality-first)

M16 exposes five additional **read-only** operations using the existing
native Debian package-state, signed policy resolver, whole-installed-state
upgrade audit and bounded removal planner.

## Commands

```sh
rpt apt-simulate-remove --status /var/lib/dpkg/status example
rpt apt-simulate-remove --status /var/lib/dpkg/status --cascade example
rpt apt-simulate-purge --status /var/lib/dpkg/status example
rpt apt-simulate-autoremove --status /var/lib/dpkg/status --extended-states /var/lib/apt/extended_states
rpt apt-simulate-upgrade --cache ./rpt-lists --status /var/lib/dpkg/status --apt-root /
rpt apt-simulate-full-upgrade --cache ./rpt-lists --status /var/lib/dpkg/status --apt-root /
```

Every new command emits a JSON report with:
- Operation, outcome: `planned`, `blocked` or `no_changes`.
- `viable_in_supported_subset`: whether the restricted read-only
  dependency/preflight model found no blockers, **not permission to apply**.
- `selected_changes`, `selected_removals`, explicit `blockers` and
  `detailed_audit` from the underlying Rust solver.
- SHA-256 of the exact dpkg status snapshot and, for autoremove, APT
  extended state snapshot. Both files must remain identical by the
  time the simulation finishes or the result fails closed.
- `simulation_only: true`, `apt_parity_verified: false`,
  `host_execution_allowed: false` on every result.

## Semantics and boundaries

- Remove refuses to silently remove reverse dependents; an explicit
  `--cascade` computes a dependency-closure proposal. Blocked removals
  report blockers, but never perform a mutation.
- Purge includes `deinstall ok config-files` residual-state packages.
- Autoremove preserves every reachable direct/alternative dependency and
  recommendation from manual/protected roots. Missing
  `extended_states` is accepted as no automatic marks; it must **not**
  infer orphan packages. This is intentionally more conservative than
  APT's exact autoremove ranking.
- Upgrade uses the existing signed, policy-aware whole-installed-state
  no-removal planner. Full-upgrade allows at most three automatic
  removals among strictly validated signed conflicts and fails closed
  on ambiguous minimum-size scenarios. These are **not** complete
  `apt-get upgrade` or `dist-upgrade` algorithms.
- All simulations reject non-regular status/mark files and bounds over
  64 MiB. They never lock dpkg, execute a maintainer script, modify
  `/usr/bin/apt`, copy a package to a live root or grant production
  permission.
- Any missing or unverified signed repository candidate required by
  upgrade/full-upgrade fails closed; read-only offline plans do not
  authorize downloads or installations.

## Acceptance

`tests/apt-operation-simulations.sh` executes actual CLI commands
against disposable dpkg/extended-state fixtures with a negative
dependency-blocker case, explicit cascade, residual purge, autoremove,
missing marks, unsupported policy bounds and unchanged snapshots.
Unit tests exercise the Rust API with the same safety invariants.

## Remaining gaps

Full APT command/config parity, complete SAT/alternative/Multi-Arch
resolution, transactional maintainer-script execution and true RPT
power-cut recovery **are not implemented by this milestone**.
M15's raw-dpkg QEMU power-cut oracle covers a separate infrastructure
requirement; it is not an RPT transaction acceptance pass.

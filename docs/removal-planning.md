# Read-only removal and autoremove planning

The removal planner is a **diagnostic**, never an instruction to run dpkg. Every JSON report has `executable: false`.

## Explicit removal

```sh
rpt plan-remove --status /var/lib/dpkg/status --architecture amd64 libexample
rpt plan-remove --status /var/lib/dpkg/status --architecture amd64 --cascade libexample
```

- Uses **installed** dpkg status, including Depends, Pre-Depends, installed binary identities, versions and virtual Provides.
- Evaluates dependency alternatives against the remaining installed set; an installed alternative can preserve a reverse dependent.
- Without `--cascade`, unsatisfied reverse dependencies are returned as blockers. With cascade, the planner computes a bounded fixed point of packages that would become unsatisfied, including transitive reverse dependencies.
- Held, Essential and Protected packages remain blockers even in cascade mode.
- No filesystem, maintainer-script, trigger or dpkg mutations occur.

## Autoremove

```sh
rpt plan-autoremove \
  --status /var/lib/dpkg/status \
  --extended-states /var/lib/apt/extended_states \
  --architecture amd64
```

The planner conservatively starts from all packages that are manual, held/protected, or on a foreign architecture. It keeps every installed package reachable through Depends and Pre-Depends, plus Recommends, including every valid alternative/provider. Only unreachable packages explicitly marked `Auto-Installed: 1` are proposed as potential orphans.

Unrecorded auto/manual marks count as **manual**, not automatic. Missing or ambiguous APT extended state fails closed. This approach can retain *more* packages than APT and is intentionally not an APT-equivalent autoremove algorithm.

## Boundaries

- Unsupported architecture profiles, `:any` and complex relationship forms fail closed rather than silently drop constraints.
- Multi-Arch dependency semantics, APT NeverAutoRemove policies, autoremove sections, auto/manual selection flags, recommends configuration and essential package handling are incomplete relative to APT.
- Versioned Provides are matched by the declared virtual capability version, not the provider package version.
- `removable: true` means only that this **restricted diagnostic** found no dependency blockers; it is not a system-wide safety guarantee.
- No execution path is connected to these planners. Actual package mutation must not be enabled until complete APT parity, dpkg state coordination and recovery validation.

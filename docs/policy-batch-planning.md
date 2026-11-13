# Policy-verified multi-root planner

Example:

```sh
rpt plan-install-batch-policy --cache ./rpt-lists --status /var/lib/dpkg/status --apt-root / --architecture amd64 curl git nginx
```

This milestone unifies multiple requested package roots in a single resolver state while selecting repository candidates through the existing verified-origins, APT preferences, target release and phased-update policy path. Dependency closure is iterated with a bounded 16-round convergence check; every planned candidate is checked again against policy. Existing cross-root conflicts, holds, foreign Multi-Arch and negative-relation guards still apply.

This is **not** full APT parity: alternative providers, mixed architectures, complex dependency cycles, upgrades, removals, autoremove and execution require further work. Unsupported cases fail closed. The command is strictly read-only and cannot authorize dpkg changes.

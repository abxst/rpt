# Removal impact audit

```sh
rpt removal-impact --status /var/lib/dpkg/status --architecture amd64 libfoo
```

Reads installed dpkg status without changing it. Reports direct installed reverse dependencies and held/Essential/Protected targets, rejecting unsupported dependency alternatives and qualifiers rather than claiming safe removal. `safe_to_remove` only means no blockers were detected by this restricted diagnostic, **not** that removal is safe under full APT semantics. `executable` is always false. No autoremove, Provides resolution, maintainer scripts or dpkg execution.

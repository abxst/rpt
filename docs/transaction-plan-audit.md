# Transaction plan audit (read-only)

```sh
rpt plan-transaction-audit --cache ./rpt-lists --status /var/lib/dpkg/status --apt-root / --architecture amd64 curl git nginx
```

This command runs the candidate-policy-verified multi-root resolver, then emits a JSON report classifying planned binaries as `install`, `upgrade`, `downgrade`, or `reinstall` using Debian version ordering. It refuses duplicate binary identities. Output has `executable: false` unconditionally.

**Not supported:** removal, autoremove, full APT parity, dpkg execution. No package or filesystem state is modified. A downgrade classification is diagnostic only, not permission to downgrade.

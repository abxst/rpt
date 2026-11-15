# M22 — Unified signed install simulation

The existing `rpt apt-simulate-install --json` now uses the same read-only typed `AptSimulation` engine as remove/purge/autoremove and upgrade/full-upgrade. This fixes an inconsistent JSON shape: consumers receive the exact input dpkg status SHA256, `outcome`, `selected_changes`, `viable_in_supported_subset`, optional `selected`, and an explicit no-host-execution decision.

Example (only after an authenticated `rpt update`):

```sh
rpt apt-simulate-install --json --cache ./rpt-lists --status ./test-status --apt-root ./apt-root --architecture amd64 bash
rpt apt-simulate-install --json --reinstall --cache ./rpt-lists --status ./test-status --apt-root ./apt-root --architecture amd64 bash
```

The previous `selected` list remains compatible with M11: it lists the selected signed resolver candidates (including already installed versions). `selected_changes` now describes actionable proposals in the supported subset: an exact same-version candidate is a no-op unless `--reinstall` is explicitly requested. The full candidate classification is available in `detailed_audit`. Dpkg status must remain byte-identical before and after planning. An interrupted dpkg status fails before any network/cache work.

The independent signed APT differential fixture validates `selected` against `apt-get -s install` and verifies the additional read-only report fields. Unit tests cover reinstall selection and no-op reporting.

**Limits:** This is not full APT resolver parity, not implementation of `apt-get --reinstall`, and not an executable transaction. Signatures authenticate the existing verified cache only; package scripts, conffile prompts, multiarch edge cases and host execution remain unsupported.

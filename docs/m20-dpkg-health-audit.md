# M20 — Read-only dpkg health diagnostics

RPT adds dpkg-audit to inspect the current or an explicitly selected dpkg status snapshot, **without running dpkg or repairing anything**. Example:

    rpt dpkg-audit --status /tmp/disposable-guest/var/lib/dpkg/status
    rpt dpkg-audit --status /tmp/disposable-guest/var/lib/dpkg/status --require-clean

The JSON includes the SHA-256 digest of the input status bytes, counts of fully installed and residual-config packages, and records of interrupted or inconsistent states. A normal deinstall/config-files residual is not treated as a broken transaction. Install/hold selections on an uninstalled package, half-installed, unpacked, half-configured and trigger-pending states, as well as reinstreq, are flagged. Unknown status codes, duplicate identities, malformed RFC822, symlink status files and oversized inputs fail closed.

For automation, --require-clean returns nonzero when interrupted/inconsistent package state is found, after printing the JSON report. The read-only apt-simulate-upgrade and apt-simulate-full-upgrade commands refuse to produce plans when this audit detects broken state. The original status files are never modified.

**Limits:** This diagnostic is not dpkg --audit compatibility, does not inspect the dpkg info directory, pending triggers, conffile conflicts, maintainer scripts or external filesystem damage. It does not perform full dependency consistency analysis. A green report is **not** authorization to run a package transaction on a production host. Parent-directory aliasing and concurrent external writers are not a complete filesystem security boundary; run recovery testing inside disposable VM guests.

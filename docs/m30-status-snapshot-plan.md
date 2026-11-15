# M30 — Bind signed package plans to one unchanged dpkg status snapshot

Even after adding dpkg health checks (M20/M21/M28) and cooperative repository index read leases (M29), a separate process could mutate dpkg status during a long multi-round dependency solve. Before M30 a read-only planner could silently return a candidate computed from an obsolete installed catalog.

The shared `dpkg_health::clean_status_digest` reads and hashes the complete, bounded, regular, non-symlink `Status` file while requiring no broken package records. `verify_unchanged_clean_file` repeats the same audit after solving and refuses the candidate if the hash differs. These helpers are now used by native multi-root signed candidate-policy planning, explicit foreign-architecture candidate-policy planning, and direct `system_upgrade::preview`, including its no-op case.

Unit tests cover unchanged, modified-but-healthy and modified-and-broken status. The existing signed APT/RPT differential fixtures confirm the positive planning cases, while M28 negative CLI fixtures check preflight on broken state. `apt-simulate-*` retains its own independent start/end SHA256 checks.

This is a **best-effort read-only snapshot binding**, not transaction-level locking against other dpkg writers or a replacement for guest checkpoint/recovery; a malicious concurrent writer or an ABA rewrite may not be detected. Do not enable live-host package execution.

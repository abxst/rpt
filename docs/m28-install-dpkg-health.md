# M28 — Dpkg health preflight for direct signed install planners

M20/M21 introduced a read-only dpkg health audit and blocked some system upgrade and guest preparation paths on broken package state. Direct signed install and explicit foreign-architecture policy planners previously still read only the fully installed dpkg catalog, potentially hiding half-installed, unpacked or trigger-pending entries.

M28 moves the same mandatory `dpkg_health::require_clean_file` check into:

- `install_plan::preview_many_with_policy_options`, the common native signed install planner (batch, `apt-simulate-install`, guest prepare/apply, and dependency closure);
- `install_plan::preview_explicit_architecture_with_policy`, the foreign-architecture signed install planner.

Both entry points fail before loading repository cache or selecting package candidates when dpkg reports incomplete/`reinstreq` state. The existing M26 enabled-source cache verification follows. CLI negative fixtures verify both commands refuse a broken original dpkg database even with an absent cache and preserve the exact status SHA256.

The guard is read-only and does not attempt `dpkg --configure -a`, `--force`, recovery, package execution or rollback. A clean status report is not proof that arbitrary maintainer scripts and host transactions are safe; production qualification remains blocked.

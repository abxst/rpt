# M26 — Mandatory signed source-set binding during policy planning

Previously M24 provided an explicit `rpt cache-audit --apt-root` check, but normal signed candidate-policy planning could still use cached Packages indexes after the operator disabled or removed the repository. M26 closes that bypass for the core read-only policy planners:

- `install_plan::preview_many_with_policy_options`: all signed native/batch installation planning, including `apt-simulate-install`, journaled guest plan preparation and independent revalidation during guest apply;
- `install_plan::preview_explicit_architecture_with_policy`: signed foreign-architecture planning;
- `system_upgrade::discover_signed_upgrades`: no-removal system upgrade discovery, including automatic/explicit full-upgrade orchestration and guest full-upgrade preparation.

All three reuse M24 `audit_configured_cache` to verify the GPG-signed InRelease, Release freshness, exact Packages hashes, configured URI/suite/keyring, enabled components and source architecture constraints. Unsupported or absent current source configuration fails closed before candidate policy can use cached binary metadata. The normal `rpt update` path is unaffected.

The CI signed APT/RPT differential suite adds disabled-source and changed Signed-By negative fixtures for native and upgrade paths; the native+foreign architecture differential adds a disabled-source negative fixture for the foreign-policy planner. Existing signed positive cases and VM guest acceptance continue to run.

**Remaining limits:** This is an expensive per-call point-in-time read-only verification, not an atomic lease against untrusted concurrent writers. Non-policy legacy cache listing commands may still show old cached metadata, but are not execution authorization. Full APT resolver/config parity, maintainer script sandboxing, and live-host transaction safety remain unproven. Do not install RPT as a host APT replacement.

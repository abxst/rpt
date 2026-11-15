# M21 — Block signed planning on interrupted dpkg state

M20 detects nonterminal dpkg Status states in a read-only snapshot. M21 promotes the same conservative health preflight to the **core signed upgrade planners** and disposable guest preparation, rather than only the apt-simulate entry points.

Protected entry points:

- plan-system-upgrade-policy / system_upgrade::preview
- plan-auto-full-upgrade / system_auto_upgrade::preview
- plan-full-upgrade-scenario / system_full_upgrade::preview
- native-rootfs-prepare and shared native_rootfs::prepare for signed guest install intents

All operations fail before metadata discovery / creating a new guest intent when half-installed, unpacked, half-configured, trigger-pending or reinstreq package state exists. Full-upgrade cannot discard the interrupted record through a hypothetical removal. Normal deinstall/config-files residual entries remain allowed.

The CLI E2E test verifies that direct signed planners return an explicit dpkg-health refusal even without a repository cache, and that original status bytes remain unchanged. Existing independent application-time dpkg --audit, journal locks, and guest-only execution restrictions remain in force.

**This is not full dpkg --audit equivalence or safe live-host recovery.** External dpkg writers, all maintainer-script effects, security boundary around privileged scripts, full resolver parity, and power-loss-safe rollback remain unsupported. Production activation is blocked.

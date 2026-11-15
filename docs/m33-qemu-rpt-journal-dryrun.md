# M33 — RPT-owned dry-run journal across an actual QEMU hard power cut

M15 proves hard-killing QEMU during a real guest dpkg postinst yields incomplete package state and that a sealed offline v1 QCOW2 can be restored to a new disk. M31 additionally tests RPT dpkg-health audit in both failed and recovered guests. Neither proved a journal written by **RPT itself** survived the hard cut.

M33 introduces an explicitly nonmutating dry-run failpoint `rpt-transaction-dry-run --pause-after-running-ms 180000`, bounded to at most 300 seconds, which persists the existing journal `Running` phase first, then waits. The QEMU workflow compiles the exact-head RPT dry-run and journal-inspection binaries and copies them to the guest baseline.

In the crash overlay, a systemd unit runs the RPT dry-run and must persist `m33-powercut.json` with `phase=running`. The independent test then invokes a **separate actual guest dpkg** v2 postinst and SIGKILLs the entire QEMU process only after both the RPT journal phase and dpkg postinst marker exist. Upon reboot, `rpt-journal-inspect --json` must return failure and show the interrupted RPT journal in Running. RPT`s `dpkg-audit` also detects the broken guest dpkg database. A different recovered disk from the untouched baseline has neither incomplete journal nor dirty dpkg status.

This is stronger than a synthetic process-only `kill -9` because the journal lives on a guest virtual disk while its OS is abruptly powered off. **However, RPT itself did not drive the dpkg installation**; this tests only RPT`s nonmutating journal durability/inspection, not real RPT transaction durability, rollback of installed packages, or safe live-host execution. The M15 evidence `rpt_guest_transaction_executed` and `production_ready` fields correctly remain `false`.

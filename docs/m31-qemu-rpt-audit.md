# M31 — Run RPT read-only dpkg audit across actual QEMU power-cut and recovery

The existing M15 QEMU acceptance uses independently signed Ubuntu 24.04 QCOW2, installs a disposable package via guest dpkg, SIGKILLs the QEMU process during a blocked postinst, and restores from a sealed baseline into a new disk. This is genuine guest power loss but **not** a powered-off RPT transaction.

M31 builds `target/debug/rpt` from the exact pull request commit as part of the QEMU acceptance workflow and copies it into `/home/ubuntu/rpt-audit` inside the guest before sealing the clean baseline. After the hard power cut and reboot, the guest runs `rpt dpkg-audit` against its actual `/var/lib/dpkg/status`; the JSON must have `healthy: false`, nonempty issues, `read_only: true`, and `host_execution_allowed: false`. `--require-clean` must fail. After a fresh independent restore overlay is booted, the same RPT binary must report `healthy: true`, zero issues and pass `--require-clean`.

All JSON evidence is saved as individual files alongside independent `dpkg --audit` and QCOW2 checks. The existing M15 evidence schema remains unchanged and continues to explicitly state `rpt_guest_transaction_executed: false` and `production_ready: false`.

Run manually **only on a disposable VM host** with QEMU and a built binary:

```sh
cargo build --locked --bin rpt
RPT_QEMU_BIN="$PWD/target/debug/rpt" bash tests/qemu-powercut-oracle.sh
```

This proves only that the current RPT status inspector identifies a real interrupted guest database and a clean restored database. It does not prove RPT journal durability, power-loss safety for RPT-owned installs, atomic rollback or APT behavior parity. The production gate remains closed.

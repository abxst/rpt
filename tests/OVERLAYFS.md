# OverlayFS privileged integration fixture

This is an **opt-in** test for a disposable Linux VM with OverlayFS, `unshare`, `mount`, and `CAP_SYS_ADMIN`. It is intentionally **not** run by ordinary GitHub Actions CI, and it does not use Ubuntu package mirrors.

```sh
cargo build --bin rpt-overlay-probe
sudo bash tests/overlayfs-isolation.sh ./target/debug/rpt-overlay-probe
```

The script creates a fresh fixture under `/var/tmp`, runs the probe inside a private mount namespace, exercises copy-up, whiteout, new-file and directory creation, verifies the original lower files remain intact, and confirms the mount is not visible in the calling namespace after exit. The trap refuses to delete the staging tree if the mount unexpectedly remains visible.

**Limitations:** A successful probe does not establish a secure sandbox for untrusted maintainer scripts, a crash-safe commit, or consistency of a concurrently changing lower filesystem. The test requires elevated privileges and should be run on a disposable VM. Failure must be investigated before adding a real dpkg executor.

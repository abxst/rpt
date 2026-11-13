# Policy-aware install planning from a persistent sandbox

RPT can now resolve install/upgrade candidate plans against the **current dpkg
database inside an RPT-owned persistent Docker container**. This eliminates
the need to manually supply a host dpkg status file for sandbox planning.

```sh
rpt sandbox-session-plan-install \
  --session dev01 --cache ./rpt-lists --apt-root / \
  --architecture amd64 curl git

rpt sandbox-session-prepare-download-plan \
  --session dev01 --cache ./rpt-lists --apt-root / \
  --architecture amd64 --output sandbox-locked.json curl git

rpt fetch --manifest sandbox-locked.json --output ./rpt-artifacts

rpt sandbox-session-install --session dev01 --id tx-install-1 \
  --manifest sandbox-locked.json --artifacts ./rpt-artifacts \
  --cache ./rpt-lists --architecture amd64
```

Implementation:

- Validates Docker session identity and reads the guest's actual dpkg status.
- Stores a private, automatically deleted, mode-0600 temporary snapshot
  for the existing candidate-policy multi-root solver.
- Selects candidates from authenticated RPT repository metadata with
  pinning/target-release/phased-update checks already supported by RPT.
- Locks the exact package/architecture/version/download hash into the same
  signed manifest used by `rpt fetch` and `sandbox-session-install`.
- When the sandbox has already installed the selected candidate version,
  the download manifest is empty. A later change in guest dpkg state affects
  the next planning call, rather than reading an unrelated host status file.
- Reuses the sandbox's journaling and package control preflight from earlier
  milestones.

**Limitations:** Changes to the sandbox dpkg status between planning and
installation are not yet bound into the manifest; dpkg still checks dependency
constraints but there is not an atomic APT transaction lock across separate
commands. Incomplete alternative-provider, architecture, removal and conffile
semantics remain. Do not enable host package execution based on these tests.

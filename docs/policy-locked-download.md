# Policy-locked dependency download manifest

RPT now has a read-only bridge between the candidate-policy multi-root solver and the verified download engine.

```sh
rpt update --root /etc/apt --output ./rpt-lists
rpt prepare-download-plan \
  --cache ./rpt-lists --status /var/lib/dpkg/status --apt-root / \
  --architecture amd64 --max-packages 200 \
  --output ./locked-manifest.json curl git
rpt fetch --manifest ./locked-manifest.json --output ./rpt-artifacts
```

The planner first selects an authenticated policy candidate for every staged dependency, then **revalidates** GPG signature, Release freshness, index hashes, filename and artifact SHA256 for the exact selected `(package, architecture, version)` tuple. It never substitutes a newest available package for a pinned candidate.

The output packages contain `name`, `url`, `sha256`, `size`, `filename`, `version` and `architecture`. The existing `rpt fetch` reader ignores the additional identity fields but still enforces the signed artifact content hash and size. Installed packages already at the exact selected version are omitted from download.

The export refuses conflicting same-identity signed payloads, duplicate planned identities, foreign architecture items, colliding output filenames and missing exact archives. A no-op plan emits an empty manifest.

**Boundary:** An authenticated artifact is not equivalent to a safe install. This command does not evaluate maintainer scripts, dpkg triggers, live filesystem safety or recovery. The JSON contains `executable: false`; no dpkg mutation is permitted. APT solver, pinning and phased rollout parity remain incomplete. Use RPT's existing `fetch` only to acquire and verify artifacts.

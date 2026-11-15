# M24 — Bind signed cache audit to current APT source policy

An archive can remain cryptographically valid after an operator removes its source. The M23 offline cache validator proves signature and bytes but cannot decide whether a repository is still allowed by the **current** Signed-By sources. M24 adds an opt-in source-set gate:

```sh
rpt cache-audit --cache ./rpt-lists --apt-root /path/to/isolated/apt
```

With `--apt-root`, the command must parse the strict M19 source subset and requires a one-to-one match between enabled `(URI, suite)` repositories and all audited cache entries. Every Signed-By path must be the currently configured keyring; every cached index must be inside an enabled source component and its allowed architectures. Empty source sets, missing/stale cache repos and conflicts fail closed. This gate performs no network or package execution.

The signed differential CI runs a real verified local repository through both audit modes, then supplies an empty source config to ensure stale cached metadata is refused.

**Remaining limitation:** Only this explicit audit command enforces current source-set membership. Other legacy cached candidate queries must not be assumed to enforce it automatically. Race-free filesystem protection, full APT semantics and host execution remain blocked.

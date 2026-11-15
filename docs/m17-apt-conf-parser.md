# M17 — strict nested apt.conf parser and config inspection

RPT previously accepted only one-line `APT::Default-Release "noble";`
statements and rejected valid nested APT configuration blocks.

M17 adds a small bounded tokenizer and recursive apt.conf subset parser.
It now supports both standard spellings, even with multiple statements on
one line or comments:

```
APT::Default-Release "noble";
APT { Default-Release "noble"; };
Dpkg::Progress-Fancy "true";
```

`rpt apt-config-inspect --apt-root <path>` emits JSON with the validated
`default_release` and explicit `apt_parity_verified: false` and
`host_execution_allowed: false` fields. The resolver's existing
`policy::read_default_release` now uses the same parser for both flat
and nested APT configuration.

### Explicit boundaries

- Only `APT::Default-Release` plus the non-policy, cosmetic
  `Dpkg::Progress-Fancy` and `APT::Color` settings are accepted.
  Unsupported active configuration (including `Dir::Etc` overrides,
  `Acquire` settings and `APT::Install-Recommends`) is rejected rather
  than silently misapplied.
- `#include`, `#includedir`, duplicate target-release declarations
  across or within files, malformed comments/quoting, and symlinked
  config fragments fail closed.
- 1 MiB/file, 4096 tokens, 16 levels nesting, 128-byte key and
  8192-byte quoted string limits.
- `APT_CONFIG` environment override is still unsupported and explicitly
  rejected.
- The ordinary default-priority `policy::ensure_no_custom_policy`
  continues to reject any target-release declaration, even if syntactically
  valid. Only signed candidate-policy-aware code may use the target.
- Does not modify apt.conf, dpkg status, host package database, apt lists
  or `/usr/bin/apt`.

### Test coverage

Unit tests include flat and nested forms, multi-statement lines,
comments, duplicate-key ambiguity, blocked indirection, unsupported
semantics, unclosed blocks/strings and malformed booleans. The CLI
acceptance script checks read-only JSON and rejects duplicate config
across fragments and symlinked fragments.

**This is configuration syntax coverage, not full APT CLI/config parity.**

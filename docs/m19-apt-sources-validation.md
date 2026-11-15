# M19 — Validated signed repository sources and architecture filters

This milestone extends the read-only Debian source-definition subset
used by RPT's authenticated `update` workflow. It is not full APT sources
compatibility.

## Supported sources

```
deb [arch=amd64 signed-by=/usr/share/keyrings/ubuntu-archive-keyring.gpg trusted=no] https://archive.ubuntu.com/ubuntu noble main
```

```text
Types: deb
URIs: https://archive.ubuntu.com/ubuntu
Suites: noble noble-updates
Components: main universe
Signed-By: /usr/share/keyrings/ubuntu-archive-keyring.gpg
Architectures: amd64 arm64
Enabled: yes
Trusted: no
```

Legacy bracket options are recognized in either order and accept
`arch=`, `signed-by=` and `trusted=no`. `signed-by` remains
mandatory and must reference one normalized absolute keyring path.
Deb822 now fails closed on unknown fields, invalid Enabled/Trusted values,
ambiguous duplicate fields and invalid/duplicate architecture declarations.
`Trusted: yes` always fails; no source can bypass signature verification.

If a source restricts `Architectures`, additional architectures
explicitly requested by the user are constrained to that allowed set;
RPT never fetches an index outside the source's own architecture filter.
Foreign-only repository sources are not yet supported by the native
index selection and are ignored; a completely foreign-only source set
is rejected.

Conflicting declarations for the same repository URI and suite are
rejected rather than selecting one keyring or architecture subset.
Source files and `sources.list.d` must be real non-symlinked entries.
Each file is capped at 1 MiB; `sources.list.d` at 256 entries.

## Inspection without downloads

```sh
rpt sources-validate --root /etc/apt
```

Returns JSON describing the normalized repository definitions including
Signed-By and allowed Architectures. No HTTP or GPG verification occurs
during this *syntax-only* inspection, and no package state is mutated:
`apt_parity_verified: false`, `host_execution_allowed: false`.

## Explicit omissions

- No support for inline multiline `Signed-By` public keys, `Trusted: yes`,
  disabling Valid-Until, authentication/proxy source overrides,
  Deb822 architecture add/remove qualifiers, or exotic suites.
- Existing signed `InRelease` verification and Release checksum validation
  continue to occur in `rpt update`, **not** in `sources-validate`.
- Foreign-architecture-only source declarations and arbitrary APT source
  configuration options fail closed or are skipped when no native index
  can be selected; this is not complete APT multiarch support.
- RPT never writes to the system's actual APT lists during this workflow.

Native ARM64 and AMD64 CI run source syntax tests using the actual host
native architecture. Unit tests include extra-arch index restriction,
unknown fields, trust bypass, conflicting repository identity and
symlink rejection.

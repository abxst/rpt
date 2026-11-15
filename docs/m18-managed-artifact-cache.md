# M18 — Explicitly owned artifact archive cache

RPT can now clean its own downloaded `.deb` archives without ever deleting
`/var/cache/apt/archives` or changing host dpkg/APT package state.

## Commands

```sh
# Only succeeds for a NEW or completely empty ordinary directory:
rpt artifact-cache-init --root ./rpt-artifacts

# Download signed RPT manifests normally into the marked directory:
rpt fetch --manifest exact-manifest.json --output ./rpt-artifacts

# Default is a read-only JSON preview:
rpt artifact-cache-clean --root ./rpt-artifacts

# Explicit destructive cache cleanup:
rpt artifact-cache-clean --root ./rpt-artifacts --apply
```

`artifact-cache-init` creates an exact `.rpt-artifact-cache` marker and
`.rpt-artifact-lock` file inside the otherwise empty directory, then
fsyncs control files. No pre-populated directory may be adopted, and
system directories (`/`, `/etc`, `/usr`, `/var/lib`, `/var/cache/apt`,
`/run`, `/proc`, `/sys`, etc.) are rejected.

`artifact-cache-clean` requires this exact ownership marker, takes an
exclusive cooperative file lock and scans its entire subtree before
removing anything. Symlinks, cross-device mount points, external
hardlink aliases, special files and more than 50,000 entries or 32
levels abort the operation. Only regular files ending in `.deb` and
unfinished RPT `.rpt-*.part` archives are eligible; unrelated files
and the cache marker/lock are preserved. The default is preview-only,
reporting candidate count and size.

RPT downloader takes a shared lock for the whole fetch batch if its
destination is an initialized managed cache; this prevents
RPT-cooperating concurrent fetches and cleanup from overlapping.

### Limitations

- **Never** treat these operations as `apt-get clean` or `autoclean`
  parity. They work only in an explicitly initialized RPT cache, not
  the host's APT archive directory. `autoclean` based on signed
  candidate metadata is not implemented.
- Even with a full preflight, path-component swaps by **uncooperative
  external writers** remain a time-of-check/time-of-use risk. Do not
  invoke `--apply` on a directory concurrently writable by untrusted
  users/processes. A future hardened directory-FD/openat2 cleanup can
  close this gap.
- Cleanup is not crash-atomic. A disk error during deletion can leave a
  partially cleaned archive cache; it does not affect the installed
  package state.
- If the caller chooses a non-managed destination for `rpt fetch`,
  the downloader retains its old behavior and archive cleanup will
  refuse that destination rather than guessing ownership.

Unit and end-to-end CLI tests cover ownership, preview, explicit
deletion, file preservation, hostile symlinks and hardlinks, and
protected APT host cache paths. Production host package replacement
remains BLOCKED.

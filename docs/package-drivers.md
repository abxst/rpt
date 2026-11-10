# Package driver architecture

RPT's goal is first to match the native package manager's behavior for a
single supported distribution family. A package-driver boundary allows future
native compatibility without rewriting download scheduling, error reporting
or orchestration.

## Layout

```text
src/
  drivers/
    mod.rs        registry, PackageDriver trait, distro discovery, native types
    deb.rs        Debian/Ubuntu versioning, dpkg status, metadata planning
    rpm.rs        RESERVED: Red Hat/Fedora/DNF/RPM driver
    apk.rs        RESERVED: Alpine/apk-tools driver
    pacman.rs     RESERVED: Arch/libalpm/pacman driver
  cache_query.rs       existing Debian Packages/dpkg format parser
  metadata_query.rs    existing Debian Packages metadata parser
  install_plan.rs      experimental Debian-specific dry-run planner
  package_provenance.rs signed Debian Release/Packages/.deb acquisition
  update.rs            Debian InRelease-driven updater
  download.rs          shared HTTP transport (not a native package manager)
```

## Native backend contract

`PackageDriver` exposes native version comparison, installed-state lookup,
dependency-plan preview, trusted download-manifest preparation and the package
artifact extension. Unsupported operations return errors; they do not run
`apt` or silently use Debian version comparisons for RPM/APK/pacman packages.
No driver currently supports transactions or installation.

`rpt driver-info [--driver deb|rpm|apk|pacman]` shows the driver contract.
Without explicit selection it reads `/etc/os-release` to discover the native
family. **Detection is not evidence that the driver works.**

The native configuration and metadata models differ by family:

| Driver | Repository metadata | Installed state | Version comparison | Transactions |
|---|---|---|---|---|
| deb | InRelease/Release + Packages | dpkg status | Debian epochs/upstream/revision | dpkg + APT state |
| rpm | repomd.xml + primary / updateinfo | RPM database | RPM EVR, epoch/version/release | RPM/DNF transaction and scripts |
| apk | APKINDEX + repository signatures | apk installed database | apk-tools version semantics | apk state, triggers |
| pacman | signed sync databases | libalpm local database | ALPM vercmp | libalpm transaction/hooks |

Only the Debian/Ubuntu driver is partially implemented today. The others are
explicit placeholders. A working RPM/APK/pacman backend will need its own
repository parser, signatures/trust policy, dependency syntax, candidate policy,
versioning, installed-state semantics, transaction adapter and isolated VM tests.

## Implementation boundaries

- Keep network scheduling, file I/O, progress and diagnostics distro-neutral.
- Place native package identity/version/repository metadata/resolver/installed
  state and transaction logic behind the driver contract.
- Do not pretend that `.rpm`, `.apk`, `.pkg.tar.zst` are `.deb` archives.
- Do not normalize native version strings by applying Debian algorithms.
- All mutation methods must default to unsupported until tested against the
  native package manager on disposable root filesystems.
- Never use detection or a flag as permission to overwrite native executables.
- Any extension must preserve user-defined repository signature policy.

## Adoption sequence

1. Finish full APT behavioral parity in the `deb` driver, including dpkg
   transaction safety and recovery.
2. Add native format parsers/fixtures and semantic tests per family.
3. Implement one new native driver at a time with equivalent parity gates.
4. Optimize network and cache layers only after correctness is demonstrated.

The current code routes `version-compare`, `installed-version`,
`plan-install` and `prepare-download` through the driver boundary.
Other Debian-only commands retain their existing behavior and have not yet been
migrated; further migrations must be explicit and tested.

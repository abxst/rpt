# APT replacement acceptance criteria

**Priority: behavioral APT replacement first; Rust-specific performance features second.**

RPT is currently experimental: it can execute dpkg inside isolated Docker sandboxes, and now inside a separately prepared **disposable native rootfs** with explicit test-only marking. **Live host package execution is disabled.** The existence of a command or a green build does **not** mean compatibility.

## Release policy

- `alpha` / prerelease: partial implementation with explicit limitations.
- `1.0` (APT replacement candidate): all blocking suites below pass on clean, upgraded, broken-state, and offline Ubuntu test images. No alpha release is advertised as interchangeable with `/usr/bin/apt`.
- The deployment gate `scripts/stage-apt.sh activate` **must remain closed** until these acceptance tests pass in isolated VMs and canary machines.
- Do not move or overwrite `/usr/bin/apt` (or `apt-get`, `apt-cache`) during testing. This project cannot assume a running system's package database is disposable.

## Parity gates

| Gate | Minimum evidence | Status |
|---|---|---|
| A: apt CLI/config | `apt`, `apt-get`, `apt-cache` command/options/exit-code matrix, `apt.conf`, preferences and sources parsing | NOT MET |
| B: trusted metadata | InRelease/Release.gpg; Signed-By, date/expiry and Release-info changes, by-hash, mirrors, proxies, auth, source architecture, incremental updates | PARTIAL |
| C: candidate policy | pin priorities, NotAutomatic, target release, phased updates, held packages, multiarch; compare `apt-cache policy` and candidates | NOT MET |
| D: resolver | alternatives, versioned Provides, Pre-Depends, Conflicts/Breaks/Replaces, cycles, multiarch, Recommends, autoremove; differential `apt-get -s` | NOT MET |
| E: artifact acquisition | signed Release => Packages => .deb SHA256 and size, cache replay and tamper tests, mirror failover, disk limits | PARTIAL |
| F: package transactions | dpkg locks, unpack/configure/deconfigure, maintainer scripts, conffiles, debconf, triggers, root permissions, holds | NOT MET |
| G: failure recovery | interrupted dpkg, blocked scripts, signal/power loss, corrupt downloads, full disk, rollback/recovery and idempotent retries | NOT MET |
| H: system behavior | upgrade/full-upgrade, remove/purge, reinstall, autoremove, clean/autoclean, noninteractive and dry-run parity | NOT MET |
| I: benchmark | parity-complete workload on equal VMs; CPU/RAM/I/O/network/time with regression thresholds | DEFERRED |

Each gate needs **positive and negative** fixtures and reproducible CI evidence. Distinguish tests running against a disposable dpkg database from tests against the host. Mutating tests run only in isolated rootfs/VMs, never directly on the GitHub host runner.

## Differential test design

1. Prepare disposable Ubuntu 24.04 root filesystems with matched apt sources, signing keyrings, config, architectures, package state and cache snapshots.
2. Generate fixture classes: fresh install, same-version installed, downgrade, holds, pins, alternatives, virtual packages, Conflicts, Pre-Depends, packages with conffiles and maintainer scripts, half-configured, broken dependency graph.
3. Collect APT's reference output via `apt-cache policy`, `apt-get -s install/remove/upgrade/full-upgrade/autoremove` and package database snapshots.
4. Run RPT on the same fixture, normalize display-only differences, compare selected versions, install/remove set, dependency order, prompts, exit statuses, disk/network writes and resulting dpkg status.
5. Treat *every* unexplained difference as a failure. Include offline, 404, transient 503, invalid signature, expired Release, inconsistent By-Hash, truncated .deb, interrupted transaction, and disk-full cases.
6. Only after full parity, run repeatable APT-versus-RPT performance benchmarks. The existing parallel downloader is not evidence of faster end-to-end upgrades.

## Native off-host rootfs execution milestone (test-only)

RPT now offers `native-rootfs-prepare-transaction` and
`native-rootfs-apply-transaction`. Unlike the independent semantics oracle,
these commands invoke **RPT's own policy-bound signed package executor** using
real `dpkg --root`, against a separate disposable Linux filesystem. The executor
does not call Docker; the signed CI fixture may use Docker export to obtain a
complete test filesystem, then uses RPT/dpkg for all mutations.

The executor requires an explicit rootfs marker, a directory beneath
`/tmp` or `/var/tmp`, no mounts or bind mounts, and explicitly refuses
`/`. It rechecks the GPG-provenance-backed manifest, candidate policy,
dpkg status digest, archive bytes, embedded Debian control identity,
and exact installed versions before recording success. It is journaled and
process supervised, but **does not provide live host execution, secure isolation
of privileged maintainer scripts, automatic rootfs rollback, live-host transactions,
or complete APT parity**. Failures leave the disposable rootfs for inspection.

See [native rootfs transactions](native-rootfs-transactions.md).

## Native dpkg rootfs behavior oracle on disposable CI VMs

The independent `.github/workflows/native-rootfs-oracle.yml` workflow runs on
GitHub-hosted Ubuntu Linux **VM runners**. For each guest image (Ubuntu
22.04 and 24.04) it exports a complete, separate Linux root filesystem
into a private temporary tree, and runs **real dpkg with
`--root=<that tree>`**. It does not bind the host root, mount /proc,
or pass the VM host's dpkg state into the guest.

`tests/native-rootfs-transaction-oracle.sh` builds its own controlled
`.deb` packages and checks: real unpack/configure; dependency-first
Pre-Depends ordering; maintainer script execution; local conffile changes
surviving `--force-confold` upgrades; expected .dpkg-dist creation; clean
dpkg audit after successful operations; a failed postinst producing broken
guest status; and recovery from a complete pre-transaction rootfs snapshot
with the failed filesystem quarantined. An additional controlled SIGKILL
during postinst repeats the rollback checks. Snapshot restore is explicit
and performed only after the known test maintainer process has exited.

The test hashes `/var/lib/dpkg/status` and APT extended states on the
**host VM**, and verifies its hold list and dpkg audit, before and after
the guest actions, including cleanup on failure.

**Crucial boundary:** This is a **native dpkg semantics oracle**, not an
RPT-native transaction engine. RPT itself still executes packages only
inside Docker sandboxes. The test validates the VM rootfs behavior that
a future RPT native-root adapter must match; it does not assert that RPT
already supports executing or rolling back arbitrary host packages.
No production host execution or automated rollback is enabled.

## Disposable Linux VM-hosted transaction acceptance

The separate `.github/workflows/vm-transaction-acceptance.yml` workflow
runs on GitHub-managed Ubuntu 22.04 and Ubuntu 24.04 **VM hosts** and
executes the full signed RPT transaction/recovery fixture in isolated
Docker guest root filesystems (Ubuntu Jammy and Debian Bookworm).
It exercises package install, dependency ordering, real upgrade,
failure/SIGKILL, Docker snapshot restoration and journal assertions.

The wrapper `tests/vm-host-transaction-acceptance.sh` compares the VM
host's `/var/lib/dpkg/status`, APT extended states, hold list and dpkg
audit before and after the entire fixture, including on failure. A changed
host package database fails CI. The sandbox guest image is limited to
an explicit CI allowlist and defaults to Debian Bookworm.

**Scope:** The runner itself is a disposable VM but the dpkg transaction
runs inside a Docker guest. This is **not** native package execution or
filesystem rollback on the VM host, and should not be misrepresented as
such. Production-host transaction parity, transactional rollback and
maintenance-script side effects outside the guest remain untested.
Native host mutation continues to be disabled in RPT.

## Foreign dependency on Architecture: all binaries

The read-only foreign candidate-policy planner now retains exact
`PackageKey { name, architecture }` identities through its mandatory
dependency closure. When an i386 package requires an unqualified binary
that exists only as `Architecture: all`, the planner accepts the
`all` binary **only if it declares `Multi-Arch: foreign`**. It
verifies the candidate against authenticated repository origins and
APT pinning, and reports the real architecture. It must never
fabricate `:i386` metadata for an architecture-independent package.

Signed APT/RPT differential tests include a foreign i386 application that
depends on both a real i386 library and a common `all` binary. The
selected packages and versions are compared to `apt-get -s`. If a
same-name i386 and `all` candidate are both present, the conservative
planner continues to refuse the ambiguous binary rather than inventing
APT preference rules. Explicit cross-architecture qualifiers, `:any`,
virtual providers and mixed native/foreign transaction execution remain
outside the supported subset. No foreign dpkg execution is enabled.

## Signed foreign index acquisition and APT Multi-Arch differential

The repository updater can now explicitly download additional Debian
architecture indexes, retaining the mandatory native index:

    rpt update --driver deb --root ./apt-config --output ./rpt-lists \
      --additional-architecture i386

Each foreign `binary-<arch>/Packages` file must have a SHA-256 entry
authenticated by the same InRelease signature as the native index.
Missing, altered or unsigned foreign indexes fail the entire repository
refresh rather than returning a partially verified cache. By default
RPT still downloads **only its native architecture**; at most four
distinct additional architectures can be requested. This is metadata
acquisition only and does not call `dpkg --add-architecture`.

The signed `tests/signed-multiarch-differential.sh` fixture configures a
temporary, two-architecture amd64/i386 repository with a freshly generated
GPG key and isolated APT state. It compares `apt-get -s` and RPT for
a foreign-only dependency closure and a same-version
`Multi-Arch: same` binary with an already installed native partner.
It also demands rejection of a mismatched installed native version and
modified signed-cache bytes.

This remains a limited **read-only** parity milestone. Source entries
currently must be usable by the native-architecture updater; foreign-only
Deb822 `Architectures:` source selection, unqualified cross-architecture
providers, Multi-Arch: foreign/allowed, synchronized upgrades, and full
APT removal/replacement semantics are not implemented. Foreign package
installation on the host or in a sandbox is still prohibited.

## Explicit foreign-architecture candidate-policy preview

The existing restricted foreign-architecture resolver is exposed through the
read-only command `plan-install-foreign-policy`. Example:

    rpt plan-install-foreign-policy --native-architecture amd64 \
      --foreign-architecture i386 --cache ./rpt-lists \
      --status ./dpkg-status --apt-root ./apt-config libsample:i386

The root package must have an explicit `:i386` identity. Its candidate
version and every resolved dependency must be backed by signed repository
metadata for that **exact foreign architecture**. The planner validates the
foreign/native architecture pair, max transaction size, installed status,
cross-architecture Conflicts/Breaks and Multi-Arch: same co-installation rules.
It refuses implicit substitutions and unsupported Multi-Arch semantics.

CI exercises invalid scopes, traversal-looking architecture strings and
package-count bounds through the public CLI; existing resolver tests cover
foreign identity pairing and incompatible installed versions. There is not
yet a full signed APT/RPT foreign-architecture differential and this
preview **never authorizes host or sandbox dpkg execution**. Gate D remains
incomplete.

## Held packages and negative pinning differential

The signed `tests/signed-plan-differential.sh` fixture checks that an exact
`Pin-Priority: -1` excludes a recommended-only binary from both APT's
simulated `install` result and RPT's opt-in Recommends plan, while preserving
its required root package. This exercises policy candidate absence rather
than an artificially missing repository file.

The fixture also compares APT's explicit-install refusal of a held binary
(`apt-get -s -y install` without an override) with RPT's fail-closed
read-only plan. Both operations use an isolated dpkg status with
`hold ok installed` and a signed higher version in the repository.
Nothing changes in the host dpkg database.

These checks document only a narrow subset of Gate C/D/H. They do not
authorize RPT to change holds, emulate all APT pinning expressions or
permit downgrades based on priorities above 1000; those are gated on
broader differential validation.

## Verified Recommends policy (opt-in)

The native-architecture, GPG/Release/SHA256-verified candidate planner can
optionally evaluate Debian `Recommends` relationships. Use
`rpt plan-install-batch-policy --include-recommends ...` for a read-only
preview. Recommends are soft requirements: if a recommended package is absent,
pinned out, conflicts with the mandatory closure, or cannot resolve, the
mandatory installation remains valid. Any malformed relationship syntax is
still rejected. Selected optional binaries are subject to the **same**
candidate pinning, release verification, version and artifact hash rules as
mandatory dependencies.

Sandbox planning, manifest generation and status-bound install transactions
also accept `--include-recommends`. The selected mode is serialized into
the bound transaction intent, and the executor rechecks the same mode before
performing any Docker-only dpkg operation. Older intents default to `false`.
Existing RPT commands still default to mandatory-only planning; users must
opt in until broader APT policy equivalence is proven.

CI compares selected exact package versions with isolated `apt-get -s`
under `APT::Install-Recommends=true` and `false` against the same signed
fixture. Multi-root resolution, full Multi-Arch provider selection, complex
Recommends interactions and ordering heuristics are not yet fully compatible.
This remains a **partial Gate D/H improvement**, not production authorization.

## Installed-state removal and autoremove differential

The isolated `tests/apt-removal-autoremove-differential.sh` fixture now
compares the **exact removal set** from `apt-get -s` with RPT's conservative
read-only removal planners for a matched, disposable dpkg status graph.
It covers removing a required library and its reverse dependency, preserving
a dependent when a second alternative remains installed, removing both
alternatives and their consumer, and autoremove reachability from manual roots.
It additionally checks that RPT rejects removal of an Essential package.

All comparisons use local empty APT sources and explicitly scoped dpkg status,
auto/manual marks, cache and lists. Both inputs remain byte-for-byte unchanged.
This adds evidence for **part** of Gates D and H. It does not imply that
RPT's remove/autoremove behavior is generally equivalent to APT, particularly
with Multi-Arch, versioned providers, protected packages, APT-specific
autoremove rules, and broken installed state. Host dpkg mutation is disabled.

## Ordered alternative selection and joint-root differential

RPT now considers dependency alternatives in Debian control-file order
rather than sorting candidates alphabetically or refusing when multiple
branches are viable. A candidate branch is accepted only after its complete
dependent closure and negative relationships pass the existing bounded
solver checks; failed branches are discarded. Already-installed satisfying
alternatives still avoid unnecessary installation. Multiple ambiguous
virtual providers remain fail-closed until provider-ranking semantics can
be validated against APT.

The signed APT/RPT fixture compares the exact selected binary/version set
for a case in which the first alternative sorts *after* the second, and
also verifies a two-root candidate-policy graph and a joint-root conflict.
The test runs against the same signed repository, uses `apt-get -s`, and
does not mutate the host dpkg database.

This closes a **specific** Gate D gap. It does not complete Gate D:
broader cross-root dependency search, Multi-Arch behavior, Recommends selection,
autoremove parity, holds/pinning corner cases, conffiles and full APT solver
heuristics still need differential evidence. Host dpkg execution remains
disabled.

### Explicit root lookahead during alternative backtracking

Joint batch planning now treats every *concrete, explicitly requested root*
as mandatory before resolving the first package. When an earlier root has
`Depends: first | second`, and the first option conflicts with a later
explicit root, the bounded resolver can reject that branch and select the
next viable dependency. It checks both forward and reverse versioned
`Conflicts`/`Breaks`, as well as rejecting matching `Replaces`, using
the verified policy-selected metadata for both packages. The earlier
single-root preference remains unchanged. Ordered choices are bounded
by the existing 4,096 branch-attempt budget.

A signed same-repository APT fixture compares the exact package/version
set for both joint-root argument orders and requires preservation of both
requested roots. Unit tests exercise reverse versioned `Breaks`,
an out-of-range `Breaks`, and impossible `Replaces`/`Conflicts`
combinations. This is **not a complete multi-root SAT resolver**:
globally nested alternative decisions, removal/replacement, foreign
co-installation, installed virtual providers and full APT heuristics still
need further validation. No production dpkg execution is authorized.

## Installed virtual Provides in native-architecture dependency plans

The read-only solver now checks the **actual installed dpkg status**
`Provides` declarations before downloading another provider for a
native or `Architecture: all` virtual dependency. Both unversioned
capabilities and `Provides: virtual-api (= 2)` can satisfy a matching
`Depends: virtual-api (>= 2)`; the provider's own package version is
not substituted for the provided virtual version. An installed provider
can satisfy the dependency even if it has no candidate in the signed
repository, because no new binary is required.

The solver keeps a proof of each installed virtual match and verifies it
again against the *final planned* binary versions. If that provider is
upgraded during the joint transaction or is an explicitly requested
upgrade root, the verified target `Provides` declaration must still
satisfy the dependency. It never treats a foreign installed provider
as native, or implicitly applies `:any`/foreign architecture
substitution. Unsupported or ambiguous provider selection remains
fail-closed; no package removals or host mutations are authorized.

A signed APT/RPT differential fixture simulates an installed provider
that is intentionally absent from the repository and compares exact
selected package/version sets with `apt-get -s`. A versioned dependency
exceeding the provided capability must be rejected by both. Unit tests
cover mismatched versions, foreign package identities, and a later
requested upgrade dropping its original virtual capability.

This is **partial Gate D parity** only, not full virtual-provider
ranking, alternatives, Multi-Arch, or live APT replacement.

## Bounded virtual-provider alternatives under mandatory conflicts

For `Depends` and `Recommends`, the experimental native-only solver
can enumerate up to **32** exact catalog binary identities that advertise
a matching virtual `Provides` capability. Every choice runs through the
existing branch budget (4,096 attempts) and full dependent closure,
`Conflicts`, `Breaks`, `Replaces`, mandatory-root constraints, and
architecture/status guards. A versioned dependency is checked against the
declared virtual **Provides version** (not the provider binary version).

A provider is selected only if it is the **unique viable choice** for
that virtual requirement after the remainder of the dependency groups
is resolved. If two candidates both work, RPT refuses to invent a
ranking rule. If none works, RPT refuses the plan. The signed
`apt-get -s` differential fixture checks both root argument orders:
one provider is incompatible with a separately requested package, and
the other is uniquely viable. Unit tests exercise transitive branch
failures, versioned capabilities, foreign binaries, ambiguous provider
refusal, 32-provider bound and Pre-Depends ambiguity.

This is **partial Gate D** only. Pure virtual roots and ambiguous
virtual `Pre-Depends` remain fail-closed. APT's provider preferences
for multiple viable candidates, globally nested solver decisions,
installed dependency-chain constraints, package removals, real
Multi-Arch and full pinning policy still need differential coverage.
Candidate policy is rechecked before returning a plan, but this
does not authorize dpkg transactions on the live host.

## Pin-aware virtual provider candidate discovery

Before choosing among providers for a **single mandatory virtual
`Depends`**, RPT now enumerates the matching native/`all` binary
identities and verifies each binary's exact policy candidate from
authenticated repository origins. Candidate selection uses the existing
restricted APT preference/target-release evaluator. If an explicit
negative pin excludes a provider, that candidate is removed from the
*repository* choice set before bounded search; it cannot create false
multi-provider ambiguity. A provider pinned to a different verified
binary version is evaluated using that candidate's actual
`Provides`, not the latest repository version.

Installed dpkg `Provides` remains independent: pinning an unavailable
repository candidate does not erase a capability already present in
installed status. The final plan still revalidates its selected package
versions against authenticated candidate policy and does not authorize
host package writes.

The signed differential fixture checks `apt-get -s` against RPT for
an application with two possible virtual providers, one pinned to
priority `-1`, plus the rejection case when **both** providers are
pinned out. Discovery is bounded to 128 distinct provider identities
per round. This is **partial Gates C/D**; general alternative
expressions, virtual `Pre-Depends`, multiple equally viable providers,
cross-architecture substitution and unconstrained APT policy are
still deliberately unsupported.

## Global replay across dependency decisions of multiple roots

The experimental native-only solver now records *committed* decisions for
textual dependency alternatives, identified by the owning package and
group position. If a later requested root's transitive dependency causes
the overall plan or final safety validation to fail, the solver can replay
the earlier plan from its last untried alternative, with preceding choices
fixed and descendants re-evaluated. It does not simply union separate
root plans or omit a requested package. A single shared budget of **4,096
branch attempts across all replays** bounds exploration.

This extends the previous direct-root lookahead: for example,
`first -> preferred | fallback`, `second -> blocker`, where
`preferred Conflicts: blocker` is now solvable by choosing `fallback`
even when `first` was processed first. Unit tests cover this and nested
dependencies, reverse versioned Breaks and impossibility with
`Replaces`. A signed `apt-get -s` fixture checks both root orders:
when APT successfully resolves the same graph, binary/version sets must
match exactly. Some APT resolver versions reject this graph despite
there being a conflict-free alternative; that difference is explicitly
reported as a **known compatibility gap**, never counted as parity.

Global replay currently applies to mandatory textual choices with
individual concrete candidate identities. It **does not** relax
multi-provider ambiguity, rank equally viable virtual providers,
automatically remove packages, fix installed-state corruption, or
provide a full SAT solver. The same strict status, candidate, Multi-Arch,
and file-ownership safety guards remain active. This is a **partial
Gate D improvement**, not permission to execute on the host.

## Resolver and policy regression milestone

The isolated `apt-cache policy` fixture exercises priority 999/1000/1001
against RPT's read-only candidate diagnostic. The in-process resolver matrix
additionally checks alternative backtracking, versioned conflicts, versioned
virtual Provides, Pre-Depends ordering and dependency-cycle refusal.

**These are not yet end-to-end APT resolver differential tests.** The
in-process catalog does not reproduce the complete `apt-get -s` transaction
semantics, and no real dpkg mutation is authorized. Gate C and Gate D
remain **NOT MET** until matched APT/RPT fixtures cover the full policy and
dependency graph with negative cases.

## Current implemented commands

`sources`, `version-compare`, `inspect-index`, `index-versions`, `update`, `cache-versions`, `installed-version`, `policy-preview`, `show`, `search`, `list`, `depends`, `rdepends`, `plan-install`, `prepare-download`, `fetch`.

`plan-install` is a deliberately incomplete read-only planner. `prepare-download` and `fetch` only acquire artifacts. RPT can exercise `install`/`remove`/`upgrade` only inside isolated Docker sandbox sessions; it must not be used as the host APT executable. **Never use RPT as the system APT executable on the current release.**

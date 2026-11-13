# Joint multi-root planning milestone

`rpt plan-install-batch --cache ./rpt-lists --status /var/lib/dpkg/status --architecture amd64 appone apptwo`

This is a read-only exploratory planner, not APT policy parity or a dpkg executor.

Unlike concatenating independent package plans, the command resolves requested roots in a single solver state. Shared dependencies are emitted once; incompatible roots are rejected. It enforces the existing installed-package and Multi-Arch safety checks and bounded solver limits.

**Known gaps:** candidate policy/pinning and phased rollout are not applied by this command; no removals, upgrades, holds overrides, maintainer scripts, triggers or filesystem mutations are supported. A future policy-aware multi-root solver must select verified candidates before planning. Never use the preview as authorization to install packages.

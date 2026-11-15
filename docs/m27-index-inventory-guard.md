# M27 — Refuse undeclared Packages indexes inside signed caches

An otherwise-valid cached InRelease signature and its declared Packages hashes do not make every `index-*` file next to them trusted. Some read-only cache catalog readers enumerate all `index-*` filenames. Without an inventory check, an attacker could plant a second synthetic Packages index beside valid authenticated files, potentially influencing exploratory catalog resolution.

M27 adds a centralized index inventory guard in the shared signed provenance validator. Each declared local index must have a canonical `index-<digits>.{xz,gz,raw}` name and appear only once. Every directory entry beginning with `index-` must correspond to one of those declared filenames and be a regular file; an extra file or symlink fails closed. The existing GPG signature, fresh Release, exact Release-path checksum, size and index hash validations remain mandatory.

Unit tests cover valid, duplicate, extra and symlinked local indexes. Signed end-to-end CI constructs an authenticated local repository, copies the verified cache, injects an extra unsigned `index-999.raw`, and requires both `cache-audit --apt-root` and `apt-simulate-install` to refuse it before package selection.

**Limits:** Only the signed provenance-validation paths are protected by this new check; raw legacy exploratory cache-query commands are not a trusted package-execution path. Concurrent untrusted writers remain a race and must not share the cache with privileged execution. Live host package transactions remain disabled.

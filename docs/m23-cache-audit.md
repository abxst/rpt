# M23 — Verify isolated signed repository cache offline

`rpt cache-audit --cache ./rpt-lists` prints a read-only JSON report for every hexadecimal repository directory. It fails if no repositories exist, a cache entry is a symlink, a repository has invalid or oversized metadata, duplicate source identity, invalid GPG `InRelease` signature, expired Release, or changed index SHA256/size. It uses locally configured Signed-By keyrings and `gpgv`; no network connection is required.

The audit reuses the actual provenance validator used for exact package-origin planning, not just a filename/hash summary. The independent signed APT differential CI validates a successfully downloaded local signed repo and a deliberately tampered cached Packages archive. Additional CLI fixtures reject empty, incomplete and symlinked caches.

This is a **point-in-time** audit, not a tamper-proof or race-free lock: a separate untrusted process can change cache files after the report. Existing fetch/install preparation must independently reverify provenance and exact hashes. The result includes `host_execution_allowed: false` and does not establish complete APT compatibility or live-host transaction safety.

# M25 — Experimental .deb for disposable native VM testing

On a native amd64 or arm64 Debian/Ubuntu build machine:

```sh
cargo build --release --locked
bash scripts/build-deb.sh ./target/release/rpt ./out
```

The script derives the Cargo package version and creates `rpt_0.1.0~experimental1_amd64.deb` or native arm64, validates the ELF architecture and refuses archive overwrite. The package installs only `/usr/bin/rpt` with declared runtime dependencies, no `apt` or `dpkg` wrappers and no maintainer scripts.

CI inspects and unpacks the archive with `dpkg-deb -x` to a temporary folder, then runs the CLI without installing the package on the runner. Signed package publication and cross-distribution runtime compatibility are not fully certified.

**Not a production APT replacement.** Use only in disposable VM experiments. Live-host RPT package execution remains blocked; packaging does not satisfy release parity gates.

# Dev container (`nowayos-dev`)

The pinned toolchain for all four NoWayOS repos: one image, the same on
every developer machine and in CI. Editors run on the host; builds, QEMU and
the checks run in here.

## Build it

From the `nowayos` repo root:

```sh
make container                  # podman (default)
make container ENGINE=docker    # or Docker
```

A first build, with no layer cache, took 3 min 46 s on an Apple-silicon Mac
(2026-11-01, the base image already pulled); about 4 s when every layer comes
from the cache. The image is about 4.7 GB. The image is built under a candidate tag and only replaces
`nowayos-dev:0.1` once two checks pass:

- it is labelled with the sha256 of the `Containerfile` it was built from
  (`nowayos.containerfile.sha256`). Every run through
  `tools/make/container.mk` compares that label with the checked-out
  `Containerfile` and refuses a stale image ("rebuild it with
  `make container`");
- its Debian packages must match the reviewed list for its architecture,
  `debian-packages-<arch>.lock`. They come from the Debian archive as it
  stood at one moment - `DEBIAN_SNAPSHOT` in the `Containerfile`, read from
  snapshot.debian.org - so every build installs the same versions, however
  much later it runs. Moving that moment is a reviewed change: set the new
  one, then `make container NO_CACHE=1 UPDATE_DEBIAN_LOCK=1` rewrites the
  list for commit, with the diff to review.

`make container` reuses the engine's layer cache. `NO_CACHE=1` rebuilds
every layer: use it after moving `DEBIAN_SNAPSHOT` (with
`UPDATE_DEBIAN_LOCK=1`, as above). An image built before a list update still runs: only the
`Containerfile` is checked at run time.

Architectures: arm64 (Apple silicon) is built and tested. The Containerfile
also has pinned, checked downloads for amd64 (x86-64), and
`debian-packages-amd64.lock` lists its Debian packages, written by installing
the image's packages from the same snapshot for amd64 under emulation. The
whole amd64 image has **not yet been built or tested**: on an Apple-silicon
Mac, rustc crashes under x86-64 emulation, so that needs a real x86-64
machine.

## Use it

- `make shell` (in nowayos) opens a shell with the workspace at `/work`.
- Every repo's Makefile includes `tools/make/container.mk` and runs targets in
  the container with `$(IN_CONTAINER) make TARGET CARGO=cargo`; `make ci` does
  this in each repo.
- The command runs as your host user, so files it writes in the repos are
  yours. It runs with no Linux capabilities and cannot gain privileges
  (`--cap-drop=ALL --security-opt=no-new-privileges`; `CONTAINER_HARDENING`).
  Cargo's caches live in the named volume `nowayos-cargo` and target
  directories in `nowayos-target`, never on the bind mount.
  `podman volume rm nowayos-cargo nowayos-target` resets both.
- On an SELinux Linux host add `CONTAINER_BIND_OPTS=:z`. Rootless podman is
  detected and uses `--userns=keep-id`.
- A terminal is allocated only when make's own input and output are one, so
  `make shell` and `make qemu` are interactive and piped runs are not.

## What is inside

| Component | Version | Checked by |
| --- | --- | --- |
| Base image | `debian:bookworm` | index digest `sha256:f37a335e82bca302e955fa39f9dfe28f1be618f016f8a2b56318e5a5111afc26` |
| Debian packages | listed in `debian-packages-<arch>.lock` | Debian archive signatures; the list, by `make container` |
| rustup | 1.29.1 | sha256 per architecture (`RUSTUP_SHA256_*`) |
| Rust (libraries, host tools) | 1.93.1: rustfmt, clippy, target `aarch64-unknown-none` | rustup channel manifests |
| Rust (protection domains) | nightly-2026-04-04 (rust-sel4 v5.0.0's pin): rust-src, rustfmt, clippy, miri | rustup channel manifests |
| cargo-deny | 0.20.2 (MIT OR Apache-2.0) | sha256 per architecture (`CARGO_DENY_SHA256_*`) |
| seL4 Microkit SDK | 2.3.1, at `/opt/microkit/sdk` (`MICROKIT_SDK`) | sha256 per architecture (`MICROKIT_SHA256_*`) |
| seL4 for the Pi 5 | `rpi5b_2gb`'s kernel and `platform_gen.json`, every configuration, built in the `sel4-rpi5b` stage from seL4 16.0.0 and Microkit 2.3.1's `build_sdk.py` with seL4's RAM ending at `0x3f40_0000`, below the framebuffer; its build tools - cmake, ninja, dtc, xmllint, Python's ply, jinja2, yaml, lxml and pyfdt - stay in that stage | the commits (`SEL4_COMMIT`, `MICROKIT_COMMIT`), Debian versions and pyfdt's sha256 (`PYFDT_SHA256`) in the Containerfile; the RAM's end, by the build |
| QEMU | Debian `qemu-system-arm` (`qemu-system-aarch64` 7.2) | Debian archive signatures |
| U-Boot source | 2026.07, at `/opt/u-boot` (`UBOOT_SOURCE`); built by `make uboot` | sha256 (`UBOOT_SHA256`); release signature checked once, key `F3CE A874 3D60 E019 2F9B 4C7A 2BE2 A0F5 0ABF E40A` (Tom Rini, U-Boot maintainer) |
| U-Boot build tools | Debian `bc`, `bison`, `flex` (and `m4`) | Debian archive signatures |
| Raspberry Pi firmware (SD-card files) | release 1.20260915, at `/opt/rpi-firmware/boot` (`RPI_FIRMWARE`); used by `make sdcard` | every file's sha256 in `rpi-firmware.sha256`, written from the release commit's git tree |
| bindgen support | Debian `libclang-dev` (clang 14) | Debian archive signatures |
| Kani (bounded model checker, `make kani`) | 0.68.0, at `/opt/kani` (`KANI_HOME`): its release bundle with CBMC 6.11.0, the `cargo-kani` launcher, and Kani's own Rust nightly (2026-08-21); on arm64, `kani-driver` rebuilt from the release's source, because the bundle's needs glibc 2.39 | the bundle's sha256 per architecture (`KANI_BUNDLE_SHA256_*`); the launcher by version, `--locked`; the source by commit (`KANI_COMMIT`, `KANI_CHARON_COMMIT`); the nightly through rustup's channel manifest |

Every pin is an `ARG` at the top of the `Containerfile`, except U-Boot's, which sit just above its own layer: an `ARG` is an implicit input to every `RUN` after it, so changing a U-Boot pin rebuilds only that layer.

## Licences in the image

Everything here is a development tool: it builds and tests our code, and none
of it is linked into, or shipped with, anything we build — except the seL4
kernel, which every `loader.img` contains (those images stay local; shipping
is a separate decision).

| What | Licence | Status |
| --- | --- | --- |
| seL4 kernel, inside the Microkit SDK | GPL-2.0-only | development use approved by the project owner, 2026-09-30 |
| QEMU (`qemu-system-arm` and its Debian dependencies) | GPL-2.0 | development use approved by the project owner, 2026-09-30 |
| GNU toolchain (`build-essential`: GCC 12, binutils, GNU make), bash, coreutils | GPL-3.0-or-later | outside the house list - **signed off for development use**; GCC is the linker for build scripts and proc macros |
| git | GPL-2.0 | outside the house list - **signed off for development use**; cargo-deny needs it to fetch the advisory database |
| glibc | LGPL-2.1-or-later | outside the house list - **signed off for development use** |
| LLVM/libclang 14 (`libclang-dev`) | Apache-2.0 WITH LLVM-exception | outside the house list - **signed off for development use**; bindgen uses it for rust-sel4's `sel4-sys` |
| Python 3.11 | PSF-2.0 | outside the house list - **signed off for development use**; runs `tools/` |
| curl | curl licence (MIT-style) | outside the house list - **signed off for development use** |
| Microkit SDK documentation | CC-BY-SA-4.0 | outside the house list - **signed off for development use** |
| U-Boot source (`/opt/u-boot`) | GPL-2.0-or-later | development use approved by the project owner; `u-boot.bin` stays local |
| Raspberry Pi firmware: `start4.elf`, `fixup4.dat` | Broadcom binary licence (`LICENCE.broadcom`): redistribution without modification, only for developing for, running or using a Raspberry Pi device | **signed off for development use**; never on a non-Pi target |
| Raspberry Pi firmware: `bcm2712-rpi-5-b.dtb`, `overlays/` | GPL-2.0 OR MIT (the dtb); mostly GPL-2.0 (the overlays, `COPYING.linux`) | **signed off for development use** |
| `bc`, `bison`, `m4`, `flex` (U-Boot's build tools) | GPL-2.0-or-later (`bc`), GPL-3.0-or-later (`bison`, `m4`), BSD-style FLEX licence (`flex`) | outside the house list - **signed off for development use**. U-Boot's OpenSSL- and GnuTLS-based host tools are turned off, so those libraries are not installed |
| Microkit SDK capDL initialiser (packed into every `loader.img`) | audited for 2.3.1 (`tools/initialiser-audit.toml`): 41 crates - Microkit's own, 22 from rust-sel4, 15 from crates.io, and Rust 1.94.0's prebuilt `core`, `alloc` and `compiler_builtins` - all under house-list licences but `compiler_builtins` (MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)) | signed off 2026-10-04 (extending its earlier sign-off to the initialiser's copy of `compiler_builtins`) |
| OpenSSL 3 (`openssl`) | Apache-2.0 | house list; `tools/update-bundles` drives it to make each run's throwaway test keys and signatures |
| Rust toolchains, rustup, cargo-deny | MIT OR Apache-2.0 (Rust's standard library includes some crates under other permissive terms) | house list |
| Kani: `cargo-kani`, `kani-driver`, `kani-compiler` and its libraries; the SAT solvers MiniSat, CaDiCaL and Kissat | MIT OR Apache-2.0 (Kani); MIT (the solvers) | house list |
| CBMC and its goto tools, inside Kani's bundle; the crate `foldhash`, compiled into Kani | BSD-4-Clause, with the advertising clause (CBMC); Zlib (`foldhash`) | outside the house list - **signed off for development use** |
| Other Debian base packages | the usual mix (see `/usr/share/doc/*/copyright` in the image) | development tools only |

The seL4 kernel and QEMU were approved for development use on 2026-09-30
, the build tools listed above on 2026-10-02, and U-Boot's
build tools and the Pi firmware files the same day. The
capDL initialiser was audited on 2026-10-04 and signed off the same day. Any package added to the image is
listed here and signed off before it is installed.

## Changing a pin

1. Edit the `ARG` in the `Containerfile`. For downloads, take the sha256 from
   the upstream release and check it against the file you downloaded.
   For the Microkit SDK also check the `.asc` signature (signing key
   `FE91 4864 43B0 F4EB 9ECC 3652 4D86 8A34 EDF3 FDCA`); for U-Boot, the
   `.sig` (key `F3CE A874 3D60 E019 2F9B 4C7A 2BE2 A0F5 0ABF E40A`, from
   keys.openpgp.org), with `gpgv`, which the image has. For the Raspberry Pi
   firmware, rewrite `rpi-firmware.sha256` from the new release commit's git
   tree, checking each downloaded file's git blob hash against it.
2. Microkit, rust-sel4 (`pds/Cargo.toml`) and the nightly
   (`pds/rust-toolchain.toml`) move together, in one reviewed change; then
   check `pds/std-crates.txt` (`make std-crates-check`).
   A new Microkit SDK also needs, in the same change: the seL4 verification
   position re-checked (`tools/sel4-verification.toml`; `make ci` fails
   until it is), and the initialiser audit re-run for the new SDK:
   its crates, licences and advisories into `tools/initialiser-audit.toml`,
   with any new licence
   signed off first. Until then every image's SBOM marks the initialiser
   'composition unaudited'.
3. Bump the image tag (`IMAGE` in `tools/make/container.mk` and the `version`
   label), rebuild with `make container` (the package list is checked; see
   above), and run `make ci`.
4. Record the new pins in the release notes.

Known gaps: the image is a local build, not yet pushed to a registry and
pinned by digest (needs a registry decision). The Debian snapshot moves
only when someone moves it, so the image takes Debian's security updates
only then: move it with each release.

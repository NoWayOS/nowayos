# Instructions for AI agents

For any AI coding assistant working in this repository. Read
[CONTRIBUTING.md](CONTRIBUTING.md) too: these rules come from it and from the
gates in [Defence Agent](https://github.com/NoWayOS/defence-agent).

NoWayOS is a Single Purpose OS: seL4 + Microkit + Rust protection domains,
typed IPC, drivers and a declarative UI core on `embedded-graphics`. It never
depends on a product built on it, and holds no product's business logic or
configuration.

## Building

- **Install nothing on the host:** no toolchains, no `rustup`, no
  `cargo install`, no package managers. Every `make` target runs in the dev
  container (`make container` builds it once). `make` lists the targets;
  `tools/README.md` describes each.
- Never run plain `cargo` on the host. The host loop is opt-in, for a person
  who already has Rust 1.93.1: `make test IN=host CARGO="cargo +stable"`, and
  never in `pds/`.
- Before proposing a change: `make test`, `make lint`, then `make ci`.

## Rules the code keeps

- The trusted path is seL4 + Microkit + Rust: no browser engine, WebView,
  window system or C GUI runtime. Rendering is `embedded-graphics`.
- `unsafe` is forbidden in every crate under `crates/`; only the crates
  `tools/unsafe-allowlist.toml` lists may use it, and `make unsafe-audit`
  checks. seL4 and Microkit bindings come from rust-sel4, never hand-written.
- Deployed crates are `#![no_std]`, allocate nothing and use bounded storage.
- Never panic on external input: no `unwrap`, `expect`, `panic!` or `todo!` in
  library code, no unchecked indexing or arithmetic; every `#[allow]` gives a
  reason.
- A parser of untrusted input rejects oversized input before copying, returns
  typed errors, has property tests and a fuzz target in `fuzz/fuzz_targets/`.
  A message decoder between protection domains also has Kani proofs in its
  crate's `src/proofs.rs`.
- Messages between protection domains are fixed-size records, versioned and
  validated on receipt, copied out of shared memory before they are checked.
- A system grants the narrowest rights. Every grant is listed in
  `systems/NAME.grants`; widening one is a security change, and the pull
  request must say so. System descriptions name devices, never addresses.
- Applications see canonical input events only, never device protocols.
- Never claim "formally verified" without naming the exact seL4 configuration.

## Licences and sources

- Our code is MIT OR Apache-2.0; every source file starts with an
  `SPDX-License-Identifier` line, and no file carries a copyright line.
- Add a dependency only if its whole tree is MIT, Apache-2.0, BSD-2-Clause,
  BSD-3-Clause or ISC (`make deny` checks), and state its licence. For anything
  else, stop and ask a maintainer.
- Never copy, port or translate GPL code. Drivers are clean-room: work from
  datasheets and specifications, and list every source read in the pull
  request.

## Changes and pull requests

- Never weaken a gate to make a change pass: no deleted or skipped tests, no
  new allowlist entries, `deny.toml` exceptions or `#[allow]`s without saying
  why, no edits to the files Defence Agent's `OWNERS.md` lists as gate-owned.
- Do not add a `Signed-off-by` line for a person: the person who submits the
  change signs it off (`git commit -s`).
- Say in the pull request that an assistant helped, and with what.
- Test data is synthetic. No secrets, keys or credentials, not even test keys:
  a test makes its keys at run time.

# Security policy

NoWayOS is a Single Purpose OS: seL4, Microkit and Rust protection domains,
their drivers, typed messages and UI core. If you find a security flaw in it,
please tell us privately, so that we can fix it before anyone else can use it.

## Reporting a vulnerability

**Please do not open a public issue, pull request or discussion.** Report it
privately, either:

- through GitHub's private vulnerability reporting: **Report a vulnerability**
  on this repository's **Security** tab; or
- by email to **security@nowayos.com**.

Please tell us what is affected (crate, protection domain, system, commit or
release), how to reproduce it, and what an attacker could do with it. Use
synthetic test cases only: never send personal data or anyone's real files.

## Scope

In scope: the code in this repository — its crates, protection domains, Microkit
systems, board files and build tooling — on `main` and in each supported
release, built as its documentation says.

Out of scope:

- **Third-party code**, including the seL4 kernel, Microkit, rust-sel4, U-Boot,
  the Raspberry Pi firmware and our Rust dependencies. Please report those to
  their maintainers — and tell us too, if NoWayOS's use of them is affected.
- **Configurations the documentation calls development-only**: debug kernels,
  the QEMU test systems and test instruments.
- **Physical attacks on a Raspberry Pi 5.** The Pi is NoWayOS's prototype
  target, not a hardened one: it is not expected to resist invasive hardware
  attacks, glitching, or anyone with debug access.

## What we aim to do

- To acknowledge your report within **5 working days**.
- To give you our assessment — whether we consider it a vulnerability, and how
  severe — within **15 working days**.
- To keep you informed at least every **30 days** until it is resolved.
- To agree a disclosure date with you: by default when the fix is released, and
  **no later than 90 days** after your report unless we both agree otherwise.
- To credit you in the advisory, by the name you choose, unless you would
  rather not be credited.

## Supported versions

Until the first release, only `main` is supported. Once releases begin, each
release states how long it receives security fixes.

## How fixes are announced

Each fixed vulnerability gets a security advisory where this repository is
hosted, and a line in the release notes' security changes. If a crate from this
repository is published on crates.io, the advisory also goes to the RustSec
advisory database, which `cargo audit` and `cargo deny` read. An advisory says
what is affected, how severe it is, the fixed version, any workaround, and who
reported it.

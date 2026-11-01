# Contributing to NoWayOS

Thank you for helping. NoWayOS is a Single Purpose OS on seL4, Microkit and
Rust, and every change to it is judged by the same gates, written down in
[Defence Agent](https://github.com/NoWayOS/defence-agent): what each checks,
the threat it answers, and who may change it. Read its `GATES.md` before your
first pull request; this page is the short version.

## Before you start

- **Start from an issue** a maintainer has accepted (gate T1). Open one with
  the bug or feature template, or say on an existing one that you would like
  to take it.
- **A security problem is never an issue:** report it privately, as
  [SECURITY.md](SECURITY.md) says.
- Read [DEVELOPING.md](DEVELOPING.md) for the layout, the commands and the
  ground rules.

## Build and check

Everything builds and runs in the pinned dev container: your machine needs
Podman (or Docker, with `ENGINE=docker`) and GNU Make, and nothing else. Clone
into a folder of its own, as the README's "Try it" shows, then:

```sh
make container   # the dev image, once
make test lint   # while you work
make ci          # before you open a pull request
```

`make ci` is the gates' build stage: a pull request must pass it. On a fresh
clone it runs Kani and fuzzing only where your change reaches; `make ci
CI_SCOPE=full` runs every proof and fuzz target, with about 10 GB of memory for
each proof it runs at once.

## What a change keeps

- No `unsafe` outside the crates `tools/unsafe-allowlist.toml` lists; no
  `unwrap`, `expect` or `panic!` on input from outside; no unchecked indexing or
  arithmetic. The workspace lints enforce most of it.
- A parser of untrusted input has property tests and a fuzz target; a decoder
  of a message between protection domains also has Kani proofs.
- A change that adds or widens a capability in a system description changes
  its `.grants` file, and the pull request says so: it is a security change.
- Drivers are written clean-room, from datasheets and specifications. List
  every source you read in the pull request. Never copy, port or translate
  GPL code, by hand or with an AI assistant.
- Dependencies are MIT, Apache-2.0, BSD-2-Clause, BSD-3-Clause or ISC. Anything
  else needs a maintainer's written agreement first: ask in the issue.
- No secrets, keys or credentials anywhere, not even for tests. Test data is
  synthetic.

## Pull requests

- Keep each one small and about one thing, with its tests.
- **Every commit carries a `Signed-off-by` line** (`git commit -s`): with it
  you certify the [Developer Certificate of Origin 1.1](https://developercertificate.org/),
  that you wrote the change or have the right to submit it under the
  project's licence. Gate P1 fails a commit without one.
- Fill in the pull-request template: what it changes, the grants, the sources,
  and any AI assistance.
- Your contribution is licensed as the project is, MIT OR Apache-2.0
  ([README](README.md#licence)).

## AI assistants

You may use one. Say so in the pull request, and which parts it wrote: you
answer for every line as if you had written it. [AGENTS.md](AGENTS.md) holds
the instructions for an assistant working in this repository.

## For maintainers

Until outside contributors join, the maintainers' own commits need no
`Signed-off-by` line; every contributor's commit does. An assistant never
signs off on anyone's behalf.

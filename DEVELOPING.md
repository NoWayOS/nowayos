# Developing NoWayOS

What is in NoWayOS, how to build and test it, and the rules it keeps. What NoWayOS is and why it exists: [README.md](README.md).

## Status

What exists today (the README's Status says what runs where):

- the library crates in `crates/` (below): host-tested, with no `unsafe` code,
  and every one but the host-only `nowayos-testkit` builds for bare-metal
  `aarch64-unknown-none` with no heap;
- glyph atlases of the signed-off font, made at build time by a tool of
  their own (`make atlases`) and checked in: every printable Latin-1
  character at the four sizes the themes use, with metrics and kerning, for
  proportional text. Layout measures text with them (`nowayos-ui`), and
  both themes draw it from them, anti-aliased (`nowayos-render-eg`): 24 px
  in the light theme, 36 px in the large-text one. The built-in monospace
  fonts remain for bring-up and debug screens only;
- a demo in host simulation (`make demo`): a scripted session of taps,
  clicks and keys, every frame checked against an approved image (to click
  through the UI yourself, `make qemu SYSTEM=ui-interactive VNC=1`);
- more than fifty Microkit systems that boot in QEMU (`qemu_virt_aarch64`) and
  check their own serial output:
  - `hello` prints `NOWAYOS: rust pd starting` and
    `NOWAYOS: hello from rust`;
  - `ipc-demo`: two protection domains exchange typed input
    events through a shared-memory ring, and a malformed record is rejected;
  - `isolation-test`: two probes attempt accesses they were not
    granted, seL4 stops both, and a bystander keeps running;
  - `pl011-demo`: the first driver PD writes to the PL011 UART
    through typed views of its registers (`pds/hw`), with no `unsafe` code
    of its own;
  - `dma-demo`: a device writes into a protection domain's DMA
    memory by the physical address the system description gave it;
  - `display-test`: the display PD for QEMU's `ramfb`
    device shows a test pattern, and `make qemu-test` compares QEMU's
    screen with its approved image, pixel for pixel;
  - `virtio-input-test`: the virtio-input driver PD
    turns QEMU's keyboard and tablet events, typed and clicked over QMP,
    into canonical input events through `nowayos-hw`'s virtio layer, and
    drops and counts what it does not know or cannot fit;
  - `input-broker-test`: the input broker PD merges the
    keyboard's and the tablet's rings and a flooding test driver's into one
    stream for the UI, each event checked against the screen and each
    source held to its own rate: the flood and a malformed record are
    dropped and counted, and the keyboard and tablet are unaffected;
  - `uart-input-test` (a development route only): keys typed on a
    terminal attached to the debug UART - QEMU's serial port, the Pi 5's
    Debug Probe - decoded into canonical key events and sent through the
    input broker; malformed sequences dropped and counted. It runs on the
    Pi 5 too, the keys typed through the probe by `make pi-run`;
  - `usb-test` (spike S1): a USB host PD for each xHCI controller -
    QEMU's `qemu-xhci`, RP1's two on the Pi 5 - identifies it, halts and
    resets it, and runs it from its own DMA memory, a No Op command coming
    back through DMA; it runs on the Pi 5 too;
  - `usb-input-test`: QEMU's USB keyboard, mouse and
    tablet enumerated by the USB host PD and driven over QMP - typed, moved,
    clicked, unplugged with a key held and plugged back, a key held until it
    repeats - their events reaching the UI's stand-in through the
    input broker;
  - `usb-audio-test`: the USB host PD's test program
    streams QEMU's USB speaker - an isochronous OUT endpoint fed a 440 Hz
    tone, its transfers queued ahead and refilled each tick, every interval
    served or counted missed; on the Pi 5, `usb-stream-test` streams a
    camera's microphone and a speaker, and `usb-camera-test` a
    webcam's own Motion JPEG frames, whole, into a ring a client PD reads,
    only while it wants them;
  - `usb-audio-loop-test`: the USB host PD itself streams
    QEMU's USB speaker for a client PD, which sends it a 440 Hz tone as
    16-bit PCM over the audio channel's rings (`nowayos-audio`); on the
    Pi 5, `usb-audio-loop-pi-test` also records a microphone - capturing
    only while the client asks - and plays the recording back;
  - `supervisor-test`: the fault policy's supervisor stops and
    reports a faulted child, then resets the board through PSCI; the system
    boots again, with a marker written before the fault gone. The supervisor
    is a library: each system runs it from a program that names its
    children;
  - `supervisor-panic`: the same for a child that panics, in the
    debug and the release configuration, reported through a console PD;
  - `supervisor-counts`: the fault policy's counts, from
    a read-only test stand-in, below their limits - at a fault the
    supervisor reports the counts the next boot would hold and resets;
  - `failsafe-restart-limit`, `failsafe-sensitive-limit`,
    `failsafe-no-counts`, `failsafe-counts-cut-short`,
    `failsafe-counts-version` and `failsafe-counts-check`:
    counts at a limit, or that cannot be read, put the device in its
    fail-safe state at start-up - every child stopped before it runs but
    those kept, a child the supervisor's program does not name included,
    and a fault in a kept child resets nothing;
  - `supervisor-empty-list`: a supervisor whose program
    names no children still reads its counts, and at a limit stops every
    child - it fails closed;
  - `supervisor-bad-list`: a list of children the
    supervisor's library refuses - a child of class `app` kept, an `id`
    named twice, one out of range - is reported entry by entry, and at a
    limit no child is kept;
  - `supervisor-failsafe-release`: the fail-safe state in
    the debug and the release configuration, reported through a console PD
    it keeps running - an unnamed child stopped in both;
  - `restart-test`: one PD restarted cleanly on its own - its
    parent restarts it at its restart entry, which puts back its `.data` and
    `.bss` from its read-only restart image before the runtime starts; its
    client sees its new restart epoch and sends again the work in flight;
  - `input-isolation`: probes mapping exactly what the UI maps
    beside the broker try to write a driver's ring and the UI's own
    read-only ring from the broker; seL4 stops both;
  - `net-test`: the NIC driver PD for virtio-net passes
    Ethernet frames between QEMU's network card and one client through
    sDDF's network queues, never mapping the packet buffers; the client
    sends an ARP request for QEMU's gateway and prints the reply, and five
    malformed descriptors are dropped and counted;
  - `net-isolation`: probes mapping exactly what the driver's
    client maps try to write the driver's DMA memory and the read-only
    receive buffers; seL4 stops both;
  - `net-stack-test`: the network stack PD runs smoltcp
    over the virtio-net driver's queues, seeded from its seed ring by a test
    PD in the entropy PD's place, on the timer PD's monotonic time, and
    takes a DHCP lease from QEMU's user-mode network (10.0.2.15/24, gateway
    10.0.2.2);
  - `net-stack-no-seed`: the same with no seed ring - the
    stack says it is not ready and never brings its interface up;
  - `net-stack-bad-frames`: the stack against a test
    instrument in the driver's place, which passes back oversize frames,
    malformed descriptors and frames smoltcp cannot use: each oversize and
    malformed one dropped and counted, without a panic;
  - `tcp-test`: the stack carries a TCP connection for a
    socket client PD over `nowayos-socket`'s records to an echo service on
    the host - bytes both ways, then a clean close - and refuses an address
    and a port its allow list does not hold, saying why;
  - `signature-test`: a test PD checks, with
    `nowayos-signature`, an ECDSA P-256 signature OpenSSL made once, and
    one-bit changes of it, which are refused, and times each check;
  - `budget-test` and `budget-test-widened` (time isolation in QEMU): a
    PD that loops for ever, held to a budget below its period, leaves the
    heartbeat below it every beat on time; with its budget widened to its
    period, it starves the heartbeat, which stops;
  - `getrandom-test`: every way into getrandom in a PD
    - 0.2, 0.3, 0.4 and the generators on them - is refused until
    `nowayos-entropy`'s generator is seeded, then draws from it;
  - `tls-test` and `tls-test-small-arena`: a rustls
    client and server on `nowayos-tls-crypto` complete a TLS 1.3 handshake
    inside one PD, on a fixed heap arena, the client pinning the server's
    raw public key - with the plain Certificate message and the largest
    rustls accepts; on an arena below the peak, the PD stops at the failed
    allocation;
  - `update-test`: the update PD takes the run's
    bundles - keys made for the run and deleted - from a test PD in the
    TLS PD's place: each bad one refused for its reason with no slot
    marked, the good one written to the slot not running, checked and
    marked for one trial; its image built twice from the same inputs is
    the same;
  - `timer-demo`: the timer PD gives two clients a time that
    never goes back and timeouts that fire in deadline order, none early,
    and refuses malformed requests - woken by the CPU's timer on QEMU and,
    on the Pi 5, by BCM2712's system timer (its own variant,
    `systems/rpi5b_2gb/timer-demo.system`);
  - `clock-test`: the clock PD holds wall-clock time as
    an interval from the image's build time. A test PD in
    the TLS PD's place is told the time is not known and a date check is
    refused; its sample a day after the floor is taken and the check
    passes; samples earlier than the floor, narrower than their round trip
    or from an Instant still to come are refused, each with its own error;
    a PD that may only read the time cannot move it; and 300 ms on the
    interval has moved on and widened exactly as the drift bound says;
  - `ui-demo`: the demo in QEMU - an input PD plays the
    scripted session to the UI PD, which draws into a framebuffer a display
    stub reads; every frame's hash equals its approved golden image's;
  - `ui-interactive`: the same UI PD on QEMU's own screen
    (ramfb) and keyboard and tablet, through the input broker - click
    through it yourself with `make qemu SYSTEM=ui-interactive VNC=1`;
    `make qemu-test` plays the session over QMP and compares QEMU's screen
    after each step with the step's approved golden image;
  - `ui-isolation`: probes mapping exactly what the UI and the
    display map try to write the read-only framebuffer and the display's
    index on the Present ring, and to read the private page of a stand-in
    for the display and the UART's physical address, where nothing is
    mapped in the UI; seL4 stops every one, and the stand-in keeps running;
  - `ui-isolation-widened`: the same with one grant widened on
    purpose, which must show an isolation failure - proof that
    `ui-isolation` can fail;
- mechanical checks of what each system grants (`systems/*.grants`), of
  where `unsafe` code may live, and of the standard-library crates built
  into the protection domains;
- a pinned dev container holding the Microkit SDK, QEMU and both Rust
  toolchains.

On the Raspberry Pi 5 (`BOARD=rpi5b_2gb`), the UI runs on the HDMI screen
with a USB keyboard and mouse, and so do the timers, the watchdog and the
supervisor's reset; `make pi-run` boots a system on the board through a
Debug Probe (`tools/README.md`). The systems built on QEMU's own devices -
`ramfb`, virtio - run in QEMU only. Every public API is **provisional** and
may change.

## Layout

| Path | What |
| --- | --- |
| `crates/nowayos-bounded` | Fixed-capacity `BoundedVec`, `BoundedQueue` and UTF-8 `BoundedStr`, and `Wipe` for clearing data in place: no allocation, no panics |
| `crates/nowayos-ipc` | Typed message records with an explicit wire layout and a panic-free codec; a single-producer / single-consumer ring over shared memory that copies before it validates, with a separate memory view for each side |
| `crates/nowayos-audio` | The audio channel between the USB host PD and a client PD: 16-bit PCM records from a microphone and to a speaker, in the device's own rate and channels, the devices' formats every second, and the assembler that makes records from frames; no record shows its samples. The microphone's ring is a lossy ring over `nowayos-ipc`'s memory views: its producer never waits, and a consumer that falls behind loses the oldest records, each copy kept only if the producer had not begun to write over it. Property tests, a two-thread race test, a fuzz target and Kani proofs |
| `crates/nowayos-usb` | USB host logic for the USB host PD, with no hardware, tested on the host and fuzzed, from the xHCI 1.2, USB 2.0, HID 1.11, USB Audio 1.0 and UVC 1.5 specifications alone: capability and port registers, TRBs, contexts, requests, descriptors; one device's enumeration as a state machine; boot keyboards and mice as canonical events, with a held key's repeat; report descriptors and the absolute pointer they lay out; USB audio streams and isochronous transfers; a webcam's Motion JPEG, its frames assembled into the ring of frames a client reads (Kani-proven checks); and the capture gate that streams a microphone or camera only while its client asks. Nothing a controller or device writes is trusted |
| `crates/nowayos-gem` | A Cadence GEM Ethernet MAC's rules, with no hardware: its registers and values, clause 22 MDIO frames, the PHY's standard registers and the link they describe, the one Broadcom setting the Pi 5's `rgmii-id` link needs, and the receive and transmit descriptor rings, every word the device writes checked - so `nowayos-hw`'s `gem` holds only the register accesses |
| `crates/nowayos-heartbeat` | The watchdog PD's heartbeat rounds, with no seL4: answers counted once a round, and at the tick, service if every watched PD answered, else every PD that missed, highest priority first |
| `crates/nowayos-signing` | The security PD's signing records: the one input it signs - a TLS 1.3 client `CertificateVerify` input, by its fixed form - anything else refused with which part differs; the sign request and answer, and the key's approval code |
| `crates/nowayos-tls-crypto` | rustls's crypto provider for a PD, on RustCrypto crates: TLS 1.3 client only, its suites, groups and signature checks, the caller's random source; derived from rustls's `provider-example` (Apache-2.0 OR ISC OR MIT). No C, assembly or `unsafe` of its own |
| `crates/nowayos-tls-loopback` | Test instrument: a rustls client and server on `nowayos-tls-crypto`, joined in memory, so a TLS 1.3 handshake runs inside one PD and on the host under a counting allocator; the server's raw public key is made at start. Never in a product image |
| `crates/nowayos-icons` | A few of Lucide's icons (ISC), drawn in advance into 8-bit alpha masks at each device scale, so a device tints and draws an icon without parsing SVG; made only by `make atlases` |
| `crates/nowayos-input` | Canonical `InputEvent` (keys, pointer, touch) with its wire encoding, validation against the screen, pointer tracking and rate limiting; and a terminal's bytes - printable characters, Enter, Backspace, Tab, Shift-Tab, arrows, Escape - as key presses, for the debug UART's development route, everything else dropped and counted |
| `crates/nowayos-display` | Overflow-safe geometry, framebuffer pixel formats and layouts, and a bounded dirty-rectangle tracker |
| `crates/nowayos-serial` | The byte stream between a serial driver PD and its one client: chunks of up to 118 bytes on a `nowayos-ipc` ring each way, one ring per direction |
| `crates/nowayos-fault-policy` | The fault policy's rule at each start: from the counts read at boot, how the start came about and how long the system had run, the counts to store and whether to restart, counted or not, or enter the fail-safe state, and which classes of PD may keep running there; the default limits unless a product tightens them, which a product may only tighten; and `stand_in`, the counts a test system gives its supervisor until A store still to come keeps them. Property tests and Kani proofs (`make kani`) |
| `crates/nowayos-net` | The network path's lowest boundary: sDDF 0.7.0's network queues byte for byte, each side keeping its own index and checking the other's and every descriptor; an ARP request and its parser, for the network test client. Property tests, fuzz targets for both ends against a hostile peer and for the ARP parser, and Kani proofs of the index and descriptor checks (`make kani`) |
| `crates/nowayos-ip` | The network stack PD's logic: smoltcp 0.14.0 (0BSD, signed off) - built with only `medium-ethernet`, `proto-ipv4`, `proto-dhcpv4`, `socket-dhcpv4` and `socket-tcp`, which `deny.toml` enforces - over the NIC driver's four queues. Every descriptor checked and every frame copied into private memory before smoltcp parses it; oversize and malformed frames dropped and counted; a DHCP client that takes the stack's address, run only with a seed that is not 0 and on monotonic time; and up to four TCP connections for one socket client, each record checked before use, opened only to a destination the system's allow list holds. Unit and property tests, host tests against a DHCP server's frames built by hand and against a second smoltcp interface, and fuzz targets through smoltcp's parsing and through hostile TCP segments and records at once |
| `crates/nowayos-seed` | The seed a PD's own generator is keyed with: the 32-byte record on a consumer's seed ring, never all zeros, and taking one off the ring - copied out and the index moved past it before it is decoded. Property tests, a fuzz target and Kani proofs (`make kani`) |
| `crates/nowayos-rng-health` | A hardware random source's bytes health-tested as NIST SP 800-90B section 4.4 sets out - the repetition count and adaptive proportion tests at alpha = 2^-40, for 8, 4, 2 or 1 bits per byte, a failure latched - the first 1,024 discarded, then conditioned into 256-bit seeds with SHA-256 over ceil(320 / H) bytes each, for the entropy PD |
| `crates/nowayos-socket` | The socket records between the network stack PD and its socket clients: connect to an IPv4 address and port, connected, send, received, close, closed and failed, each naming a connection, on a pair of `nowayos-ipc` rings per client; stream bytes inside the records, up to one bound; and each side's connection table, which refuses what no honest peer sends; and the allow list, the destinations a system lets its stack connect to |
| `crates/nowayos-time` | Monotonic time and wall-clock time as two types that never mix: `Instant`, which `nowayos-timer-api` re-exports; UTC held as an interval from the image's build time, widened at the drift bound, with the checks on a sample from a source, the date check of a certificate and the clock's two records, a reading also as the five message registers the clock PD answers in; Kani proofs and a fuzz target |
| `crates/nowayos-signature` | Checks ECDSA P-256 signatures over SHA-256 in a protection domain - verification only, no private key and no randomness - with RustCrypto's `p256` and `sha2`, default features off; keys and signatures of the wrong form refused with typed errors; one known answer made with OpenSSL, its private key discarded; property tests and a fuzz target |
| `crates/nowayos-update` | The update records the update PD decodes before it trusts anything about an update: the signed trust list, the update manifest - product, version and security version, the image's length and SHA-256, the trust-list version it needs, a development mark, opaque reference values and the image key's signature - and the fixed-size chunks a bundle arrives in, each naming its part and offset; fixed-layout, versioned records whose decoders cannot panic and refuse every encoding but one. Property tests, fuzz targets and Kani proofs (`make kani`) |
| `crates/nowayos-ui` | First cut of the declarative UI core: bounded node tree, layout, hit-testing, focus and actions, plus the children-first `builder` API that Screen Compiler-generated screens call. Layout measures text with a glyph atlas's advances, pair kerning and line height (`TextMetrics::from_atlas`) or, the special case, a built-in monospace font's; every text a node shows is refused, with a typed error, unless each character is one the atlases draw and a person can see |
| `crates/nowayos-render-eg` | Draws a laid-out UI tree to any `embedded-graphics` draw target, each node clipped to its own rectangle, repainting only dirty areas; text from the glyph atlases, anti-aliased - each glyph's coverage blended over the colour its node is known to draw beneath it, never read back - in any RGB colour type; `RawFramebuffer`, a draw target over raw framebuffer memory in any `nowayos-display` pixel format |
| `crates/nowayos-glyphs` | The glyph atlases: every character of the UI's set drawn in advance from the signed-off font at each size the themes use - 8-bit coverage, metrics and pair kerning in 1/64 pixel - as generated Rust tables (`src/atlas/`, written by `make atlases`, never by hand), with lookups that never parse anything. Licensed `OFL-1.1`, the font's licence (its `OFL.txt`), not NoWayOS's; `glyphs.toml` pins the font file it is made from |
| `crates/nowayos-demo` | A small synthetic form demo and its scripted session of taps, clicks and keys: `make demo` checks every step's frame against a golden image (`session/`); the same screens and session run in a UI PD |
| `crates/nowayos-testkit` | Host-only test harness: in-memory framebuffer and golden-image snapshots (PNG) |
| `pds/` | Microkit protection domains and the libraries they share: a separate Cargo workspace on the nightly toolchain rust-sel4 needs (see `pds/README.md`) |
| `fuzz/` | The fuzz harness: libFuzzer targets for the decoders of untrusted input (`fuzz_targets/`), a Cargo workspace of its own that `make fuzz` builds and runs; libFuzzer is for development and CI fuzzing only and never ships |
| `tools/glyph-atlas` | The glyph atlas tool: a host program in a Cargo workspace of its own, so the rasteriser (`swash`) and its font parsers are in no device crate's dependency graph. It checks every font file against its pin, then writes `crates/nowayos-glyphs/src/atlas/` (`make atlases`); `make atlases-check` writes it again and compares |
| `platforms/` | One board file per Microkit board (`qemu_virt_aarch64.toml`, `rpi5b_2gb.toml`): the devices a PD can be granted, each with its register window, interrupts and source (read by `tools/check-system`, and by `tools/board-system`, which fills them into each board's system description: the descriptions in `systems/` name devices, never addresses); the Pi 5's U-Boot settings (`rpi5b_2gb-u-boot.config`, read by `make uboot`) and `config.txt` (`rpi5b_2gb-config.txt`, copied by `make sdcard`) |
| `systems/` | Microkit system descriptions (`*.system`), the output each must print in QEMU or on a board (`*.expect`) and everything each grants (`*.grants`); see `systems/README.md` |
| `tools/container/` | The dev container: Microkit SDK, QEMU, Rust toolchains, cargo-deny (see its README) |
| `tools/make/` | Make fragments: `container.mk` (shared by the NoWayOS repositories) and `system.mk` (NoWayOS's container, QEMU and check targets) |
| `tools/qemu-test` | Boots a system image headless in QEMU and checks its serial output |
| `tools/check-serial` | Checks a serial log captured from a board against the same expect file, by the same rules (`tools/serial_expect.py`, shared with `qemu-test`) |
| `tools/check-system`, `tools/check-unsafe`, `tools/check-std-crates`, `tools/check-crates-absent` | The grants, `unsafe` and standard-library checks, and the check that no device workspace's cargo tree holds the rasteriser (`make atlases-check`; a tree it cannot read fails too) - `tools/test_tools.py` tests them |
| `tools/baseline.toml`, `tools/check-baseline` | The lint, profile and cargo-deny settings every NoWayOS workspace must have, and their check (each repo's `make deny`) |
| `tools/unsafe-allowlist.toml` | The approved-`unsafe` list |

## Prerequisites

- **Podman** (or Docker with `ENGINE=docker`) and GNU Make. Every target runs
  in the dev container by default, so no host Rust is needed. Build
  the dev image once with `make container`, and again whenever
  `tools/container/Containerfile` changes (runs refuse a stale image).
- **Optional, for the host loop** (`IN=host`): Rust 1.93.1, the version
  `rust-toolchain.toml` pins. If a stable 1.93.1 toolchain is already
  installed, use it with `IN=host CARGO="cargo +stable"`. The Makefile sets
  `RUSTUP_AUTO_INSTALL=0`, so rustup reports a missing toolchain and never
  downloads one behind your back.

## Commands

Everyday targets - in the dev container, which they start for you; add
`IN=host` to run them on the host instead:

```sh
make test          # unit, property, doc and golden-image tests
make demo          # the demo's scripted session, every frame checked against its approved image
make atlases       # the glyph atlases made again from the pinned font (review the change, then commit it)
make lint          # rustfmt --check, then clippy with warnings as errors
make fmt           # format everything
make doc           # API docs, warnings as errors
make clean         # remove the target directory (the container's; the host's with IN=host)
```

In the dev container only (except `make container`, which builds it):

```sh
make container                     # build the dev image (once; host only)
make nostd                         # every crate but the host-only ones, for aarch64-unknown-none
make deny                          # the baseline check, then cargo-deny: licences, sources, bans (smoltcp's features
                                   # among them), advisories
make sbom                          # a CycloneDX SBOM per workspace: build/sbom/nowayos.cdx.json, nowayos-pds.cdx.json,
                                   # nowayos-fuzz.cdx.json, nowayos-glyph-atlas.cdx.json
make secrets                       # the tree and every commit scanned for keys and credentials, offline (secrets-allow.toml)
make fuzz                          # every fuzz target built with libFuzzer and run for FUZZ_SECONDS (10) s; corpus and
                                   # crashes in build/fuzz/ (FUZZ_TARGETS=ipc_decode for one; FUZZ_INPUT=FILE to rerun one input)
make atlases-check                 # the atlas tool's checks and tests, the atlases made again and compared with the
                                   # checked-in ones, and no rasteriser crate in any other workspace's cargo tree
make qemu-test SYSTEM=ipc-demo     # build, check the grants, boot headless, check the output
                                   # (each image's seL4 configuration is recorded beside it: sel4-config.txt,
                                   # and what it is made of: sbom.cdx.json)
make image-sbom SYSTEM=hello       # the image and its CycloneDX SBOM: kernel, loader, monitor, capDL initialiser (with the
                                   # crates its audit found in it), every PD and the crates in it, each with its sha256
                                   # (BOARD=rpi5b_2gb: the SD card's files too)
make qemu-test-all                 # every system
make qemu SYSTEM=hello             # boot interactively; quit with Ctrl-A, then x
make check-serial SYSTEM=hello LOG=build/rpi5b_2gb/hello/serial.log   # check a board's captured serial log
make uboot                         # U-Boot 2026.07 for the Pi 5 (build/rpi5b_2gb/u-boot/u-boot.bin), reproducibly
make sdcard BOARD=rpi5b_2gb SYSTEM=hello   # the Pi 5 SD card's boot partition, with SHA256SUMS (build/rpi5b_2gb/hello/sdcard/)
make pds-test pds-miri             # the pds libraries' tests; Miri over the approved-unsafe ones
make unsafe-audit                  # unsafe code only where tools/unsafe-allowlist.toml allows it
make sel4-config-check             # the seL4 verification position is for the pinned Microkit SDK
make screen-compiler-check               # Screen Compiler's generated screens against this nowayos-ui, drawn as approved (needs ../screen-compiler; else a notice)
make ci                            # the everyday gate: all of the above, Kani and fuzzing only where a change
                                   # reaches (CI_SCOPE=full: every proof and target; Kani needs ~10 GB per KANI_JOBS)
make release-evidence              # the release evidence pack for the commit checked out (refuses uncommitted changes):
                                   # build/release-evidence/COMMIT/INDEX.md, the ci summary, SBOMs, versions, image hashes
make shell                         # a shell with the workspace at /work
```

`make` on its own lists every target, and `make system-help` describes the
container and system targets in more detail.

The maintainers run one more before publishing: `make publication-scan
TERMS=FILE` reads the open repositories' trees and histories for the terms in
FILE, a private list kept outside every repository, and for private links and
assistant files. It is not part of `make ci`. When a rendering change is
intended, regenerate the golden images with `NOWAYOS_UPDATE_SNAPSHOTS=1 make
test` and review the new files in `crates/nowayos-render-eg/tests/snapshots/`.
With a screen-compiler clone beside this one, `make ci` also matches Screen Compiler's
generated screens with their approved images (`make screen-compiler-check`): redraw
those with `make bless-images` in screen-compiler, where a person approves them
before they are committed, and push both repos' commits together.

**A product repo's own systems and PDs** - kept in that repo, never here -
are built and checked with these same targets and checks: a repo beside this
one sets `SYSTEM_REPO := $(CURDIR)` (and its default `SYSTEM`) and includes
`../nowayos/tools/make/container.mk` and `../nowayos/tools/make/system.mk`.
`make image`, `qemu-test`, `qemu-test-all`, `check-serial`, `pds-lint`,
`pds-deny`, `pds-test`, `unsafe-audit` and `std-crates-check` then work on its
`systems/` and `pds/` with this repo's dev container, pinned nightly, Microkit
SDK, board facts, grant rules and checks; its PDs must pin this repo's
nightly, and their git sources the commits `pds/Cargo.toml` pins
(`tools/check-git-pins`). The variables are documented at the top of
`tools/make/system.mk` ("Product repos"). Its PDs can be NoWayOS's own,
run from programs of its own that call NoWayOS's PD libraries: the timer,
the virtio console driver, and the supervisor, whose program names the
system's children (`pds/supervisor`'s documentation shows how).

## Ground rules

- Every crate forbids `unsafe`, panicking shortcuts, unchecked indexing and
  arithmetic, and `#[allow]` without a reason: the NoWayOS baseline,
  `tools/baseline.toml`, which `make deny` checks in every repo
  (`tools/check-baseline`). Deployed crates are `no_std` and allocate
  nothing. The only `unsafe` code is in the PD crates
  `tools/unsafe-allowlist.toml` lists, and `make unsafe-audit` checks that.
- Everything crossing a protection-domain boundary is a bounded, versioned,
  validated message, copied out of shared memory before it is checked.
- Every parser of untrusted input has property tests and a fuzz target in
  `fuzz/fuzz_targets/`, which `make fuzz` runs in `make ci`.
- A change to a system description that adds or widens a capability also
  changes its `.grants` file, and is called out as a security change.
- The glyph atlases are the signed-off font's, under its licence (OFL-1.1):
  their crate is the one with a licence of its own, carries the font's
  notice, and is named for its job, never for the font. `nowayos-ui`
  depends on it, to measure text. Every image has the notice beside it and
  names the font in its SBOM.
- Dependencies must be permissively licensed (MIT, Apache-2.0, BSD, ISC),
  or signed off by the project owner, each with an exception of its own in
  `deny.toml` - smoltcp and `managed` (0BSD), the IP stack the
  network stack PD runs, the only ones in a device crate today;
  `make deny` enforces it for every crate any build can use, dev-dependencies
  included (the few `Cargo.lock` entries that only a never-true `cfg` reaches
  are never built, and `deny.toml` notes them), and `make std-crates-check`
  for the standard-library crates built into the protection domains.
- Test data is synthetic.

## Licence

NoWayOS is licensed under **MIT OR Apache-2.0**, at your option
(`LICENSE-MIT`, `LICENSE-APACHE`). Every crate declares it in its `license`
field and every source file carries an `SPDX-License-Identifier` line, but
for three:

- `crates/nowayos-glyphs`, the glyph atlases, is a Modified Version of a font
  under the SIL Open Font License 1.1 and is licensed under it alone (its
  `OFL.txt`);
- `crates/nowayos-icons`, the icons' masks, is ISC, the licence of the Lucide
  icons it is made from;
- `pds/hello` keeps the BSD-2-Clause notice of the rust-sel4 example whose
  shape it follows.

`crates/nowayos-tls-crypto` is derived in part from rustls's provider
example, used under its MIT licence; rustls's notice is in the crate's
`LICENSE-rustls-MIT`. `make deny` checks every crate's licence, ours
included, against the allowed list in each `deny.toml`.

## Documentation

- The guides - getting started, a first system, how a system fits together,
  the Raspberry Pi 5 - are on [www.nowayos.com](https://www.nowayos.com/docs).
- The API documentation: `make doc`. Each crate's and each PD's own
  documentation says what it does and the rules it keeps; `pds/README.md`,
  `systems/README.md` and `tools/README.md` cover the protection domains, the
  system descriptions and their grants, and every make target.

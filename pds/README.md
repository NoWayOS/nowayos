# pds: Microkit protection domains

The Rust programs that run as seL4 Microkit protection domains (PDs), and
eight small libraries they share. This is a Cargo workspace of its own,
separate from the library crates in `../crates/`, because PDs need:

- the nightly toolchain rust-sel4 is built with (`rust-toolchain.toml`,
  `nightly-2026-04-04`, the rust-sel4 v5.0.0 pin);
- the custom target `aarch64-sel4` (`support/targets/`, copied from rust-sel4
  v5.0.0; licence notice there) with `core`/`alloc` rebuilt from source
  (`.cargo/config.toml`);
- rust-sel4's `sel4-microkit`, a git dependency pinned by full commit hash
  (`Cargo.toml`), built against the Microkit SDK's headers for one board and
  configuration (`SEL4_INCLUDE_DIRS`).

All of that lives in the dev container, so build from the `nowayos` root:

```sh
make pds SYSTEM=hello              # build the PDs that systems/hello.system names
make qemu-test SYSTEM=hello        # build, make the Microkit image, boot, check output
make qemu-test SYSTEM=ipc-demo # two PDs exchange typed messages
make qemu-test SYSTEM=isolation-test # ungranted accesses fault
make qemu-test SYSTEM=pl011-demo # the first driver PD writes to the UART's registers
make qemu-test SYSTEM=dma-demo # a device writes into DMA memory by its physical address
make qemu-test SYSTEM=display-test # In QEMU: the ramfb display PD's test pattern, the screen compared
make qemu-test SYSTEM=virtio-input-test # In QEMU: the virtio-input driver PD
make qemu-test SYSTEM=input-broker-test # In QEMU: the input broker PD, a flood and the drivers
make qemu-test SYSTEM=input-isolation # the UI's forbidden writes beside the broker fault
make qemu-test SYSTEM=net-test # In QEMU: the virtio-net driver PD and an ARP exchange
make qemu-test SYSTEM=net-isolation # the network client's forbidden writes fault
make qemu-test SYSTEM=net-stack-test # In QEMU: the network stack PD on smoltcp takes a DHCP lease
make qemu-test SYSTEM=net-stack-no-seed # In QEMU: with no seed ring, no interface
make qemu-test SYSTEM=net-stack-bad-frames # In QEMU: bad frames from a hostile driver dropped and counted
make qemu-test SYSTEM=tcp-test # In QEMU: a TCP connection for a socket client; unlisted destinations refused
make qemu-test SYSTEM=virtio-console-test # In QEMU: the virtio console driver PD, both ways
make qemu-test SYSTEM=signature-test # In QEMU: an ECDSA P-256 signature checked in a PD
make qemu-test SYSTEM=update-test # In QEMU: the update PD stages a signed release, refuses bad bundles
make qemu-test SYSTEM=budget-test   # time isolation: a looping PD held to its budget leaves the PD below its time
make qemu-test SYSTEM=getrandom-test # In QEMU: every way into getrandom reaches the PD's one generator
make qemu-test SYSTEM=timer-demo # the timer PD: time and timeouts for two clients
make qemu-test SYSTEM=clock-test # In QEMU: the clock PD: wall-clock time from the build-time floor
make qemu-test SYSTEM=ui-demo # the clickable demo in QEMU, every frame's hash checked
make qemu-test SYSTEM=ui-interactive   # the same UI on QEMU's screen, keyboard and tablet (make qemu ... VNC=1 to click it)
make qemu-test SYSTEM=supervisor-test # a faulted child stopped and reported, the board reset by PSCI
make qemu-test SYSTEM=supervisor-panic MICROKIT_CONFIG=release   # the same for a panic, in the release kernel
make qemu-test SYSTEM=supervisor-counts   # stand-in counts below their limits; a fault reports the next boot's
make qemu-test SYSTEM=failsafe-restart-limit   # counts at a limit: the fail-safe state at start-up (five more failsafe-*)
make qemu-test SYSTEM=supervisor-empty-list   # no children named: the counts still read, every child stopped
make qemu-test SYSTEM=supervisor-bad-list   # a list valid() refuses: each wrong entry reported, no child kept
make qemu-test SYSTEM=supervisor-failsafe-release MICROKIT_CONFIG=release   # the fail-safe state, release kernel
make qemu-test SYSTEM=ui-isolation # four forbidden accesses with the UI's and the display's maps fault
make pds-lint pds-deny             # rustfmt + clippy, and cargo-deny, for this workspace
make pds-test                      # tests + clippy for the libraries, on the container's own target
make pds-miri                      # Miri over the approved-unsafe crates marked miri = true
make unsafe-audit                  # unsafe code only where ../tools/unsafe-allowlist.toml allows it
make std-crates-check              # the standard-library crates built into the PDs (std-crates.txt)
```

Never run cargo in this directory on the host: the nightly it needs lives in
the dev container, and rustup would otherwise try to install it.

## Protection domains

One crate per program image; the package name is the image name in the
system descriptions (`<program_image path="NAME.elf"/>`).

| PD crate | System | What it does |
| --- | --- | --- |
| `hello` (`nowayos-hello`) | `hello` | Prints `NOWAYOS: rust pd starting` and `NOWAYOS: hello from rust` on the debug console, then waits (see "Provenance" below) |
| `glyph-bench` (`nowayos-glyph-bench`) | `glyph-bench` | Times `nowayos-raster` drawing the themes' type scale from the glyph outlines (`nowayos-glyphs`) at device scales 1, 1.25, 1.5, 2, 3 and 4 by the CPU's counter, printing each scale's glyphs, coverage bytes and time, then `NOWAYOS: glyph-bench: done`. Its rasteriser and buffer live in `init`'s frame: a `stack_size` of 0x400000. No `unsafe` code |
| `ipc-producer` (`nowayos-ipc-producer`) | `ipc-demo` | Sends a fixed synthetic sequence of `InputEvent`s (one deliberately malformed) through a shared-memory ring, notifying the consumer; continues when the consumer has drained the ring |
| `ipc-consumer` (`nowayos-ipc-consumer`) | `ipc-demo` | Maps the ring's messages read-only; copies each record out, validates, decodes and reports it; rejects the malformed one and carries on |
| `isolation-parent` (`nowayos-isolation-parent`) | `isolation-test`, `ui-isolation`, `ui-isolation-widened`, `input-isolation`, `net-isolation` | Parent of the probes, picking its test by PD name: receives their faults, reads each one's label before parsing it, stops them, reports the decoded fault, checks the bystander still answers |
| `isolation-probe` (`nowayos-isolation-probe`) | `isolation-test`, `ui-isolation`, `ui-isolation-widened`, `input-isolation`, `supervisor-test`, `supervisor-panic`, `supervisor-counts`, the six `failsafe-*`, `supervisor-empty-list`, `supervisor-bad-list`, `supervisor-failsafe-release`, `net-isolation` | Test instrument, never in a product image, picking its probe by PD name: as `probe_unmapped` reads an address not mapped in its PD; as `probe_readonly` reads, then writes, its read-only page. With the display's or the UI's maps: writes the read-only framebuffer (`display_writes_framebuffer`) or the display's index on the Present ring (`ui_writes_display_index`); reads where a stand-in for the display keeps its private page (`ui_reads_display_private`, after drawing a pixel) or `0x0900_0000`, the UART's physical address, where nothing is mapped in it (`ui_reads_device`). With the UI's maps beside the input broker: writes where the broker maps a driver's ring (`ui_writes_driver_ring`) or its own read-only ring from the broker (`ui_writes_broker_ring`). For the supervisor's tests: `faulter` checks and writes a marker page, then writes its read-only page when `trigger` notifies it; `panicker` panics. For the fail-safe tests: `kept_child` prints its start line, then writes its read-only page; `stopped_child` and `unnamed_child` - a child the supervisor's program does not name - print their start lines, which they must never get to, and so does `kept_app`, kept against its class (`supervisor-bad-list`); `unnamed_faulter` reads where nothing is mapped in it, a fault the supervisor would report even in the release configuration (`supervisor-failsafe-release`). With the network client's maps: writes where the virtio-net driver maps its DMA memory (`net_writes_driver_dma`) or its read-only receive buffers (`net_writes_rx_buffers`) |
| `isolation-bystander` (`nowayos-isolation-bystander`) | `isolation-test`, `ui-isolation`, `ui-isolation-widened`, `input-isolation`, `net-isolation` | Keeps a marker in a private page and answers the parent after each probe fault (as `display_side` in the UI tests, standing in for the display's private state; as `broker_side` in `input-isolation`, for the broker's view of a driver's ring; as `driver_side` in `net-isolation`, for the network driver's DMA memory) |
| `dma-demo` (`nowayos-dma-demo`) | `dma-demo` | Binds a DMA region and its physical address through `nowayos-hw`, and has QEMU's fw_cfg device write its signature into the region by DMA - which only works if that address is right; then a transfer past the end of the region is refused before the device sees it. No `unsafe` code. QEMU only |
| `ramfb` (`nowayos-ramfb`) | `display-test` | The display PD for QEMU's `ramfb` device: finds `etc/ramfb` in fw_cfg's file directory, draws a test pattern into its framebuffer - one 2 MiB DMA page, so physically contiguous - and gives QEMU the framebuffer's physical address, format (XRGB8888) and size by a fw_cfg DMA write. No `unsafe` code. QEMU only (fw_cfg is QEMU's test device) |
| `virtio-input` (`nowayos-virtio-input`) | `virtio-input-test` | The driver PD for QEMU's virtio input devices: finds its keyboard and tablet among the virtio-mmio transports it is granted and negotiates their features (`nowayos-hw`'s `virtio`); sets up each device's event queue, a `virtqueue::RxQueue`, in its own DMA region; on each device's interrupt copies the events out, translates them (`nowayos_input::evdev`; the tablet scaled to the screen its configuration region gives, `nowayos_region::screen`) and sends the canonical events through that device's own ring - the pointer ring and the keyboard ring - dropping and counting unknown, out-of-range and excess events. A library and a program, so a product runs it from a program of its own. No `unsafe` code. QEMU only |
| `virtio-net` (`nowayos-virtio-net`) | `net-test` | The NIC driver PD for QEMU's virtio-net card: finds the card among the virtio-mmio transports it is granted, negotiates `VIRTIO_NET_F_MAC` and reads the card's MAC address, and sets up its receive and transmit queues - `virtio_net::NetRx` and `NetTx`, a header in its own DMA memory and a packet buffer per chain - in its own DMA regions. It passes frames between the card and one client through sDDF's four network queues (`nowayos_net::queue`, viewed through `nowayos_region::net_queue`), handing the card the client's buffers by physical address (`nowayos_hw::io_region`) and never mapping them: each client descriptor is copied out and checked (inside its data region, on a buffer boundary, a length from an Ethernet header to 1514 bytes, a buffer it does not already hold), each frame from the card too; what fails is dropped and counted. A library and a program, so a product runs it from a program of its own. No `unsafe` code. QEMU only |
| `gem-net` (`nowayos-gem-net`) | `gem-net-test` (the Pi 5's) | The NIC driver PD for a Cadence GEM MAC - RP1's on the Pi 5 - polled on the timer, as no RP1 interrupt reaches the CPU. It releases the PHY's reset, which the firmware holds (`nowayos-hw`'s `rp1`), identifies and resets the MAC and sets up its rings (`nowayos-hw`'s `gem`), loops one frame back inside the MAC through its DMA and rings as a self-test, finds the PHY on MDIO, turns on the PHY's RGMII receive skew and transmit delay - the board's link is `rgmii-id`, and the PHY starts with its receive skew off, which loses every frame (`nowayos-gem`'s `brcm`, the one vendor setting) - loops a second frame back through the PHY, over the RGMII bus both ways, and restarts auto-negotiation through clause 22's standard registers; on a link it sets the MAC's speed and duplex and turns receive and transmit on, and when the link goes it turns them off and gives the client back what it had not sent. Every descriptor from the client is checked as in `virtio-net`, every descriptor word the MAC writes by `nowayos-gem`'s rings; what fails is dropped and counted; every 5 s it reports what it received and sent, and the MAC's statistics, when they change. Its MAC address is the stack's, fixed. A library and a program, so a product runs it from a program of its own. The Pi 5 only |
| `net-client` (`nowayos-net-client`) | `net-test` | Test instrument standing in for the network stack (role `network`): gives the driver every receive buffer, sends five descriptors the driver must drop (outside the region, off a boundary, too long, too short, a buffer already being sent) and an ARP request for QEMU's gateway (`nowayos_net::arp`), and prints the reply, each frame checked and copied out of the read-only receive buffers before it is parsed. Never in a demo or product system (`../tools/grant-rules.toml`) |
| `net-stack` (`nowayos-net-stack`) | `net-stack-test`, `net-stack-no-seed`, `net-stack-bad-frames`, `tcp-test` | The network stack PD (role `network`): runs smoltcp over the virtio-net driver's four queues as its one client (`nowayos-ip`), holding no device memory, interrupt or physical address and mapping the receive buffers read-only: every descriptor checked and every frame copied into private memory before smoltcp parses it, oversize and malformed frames dropped and counted, the counts reported when they change. Its seed comes on its seed ring (`nowayos-seed`, read-write region 5 and read-only region 1): with no ring it says it is not ready and never brings its interface up; with an empty one it waits, not ready, for a seed on channel 2 - smoltcp never runs on its default seed of 0. Monotonic time from the timer PD (channel 1), which wakes it when smoltcp is next due; then a DHCP lease, printed. Its MAC address is fixed, the one each system's `.qemu` file gives the card. Its socket client, when the system gives one: `nowayos-socket`'s records on two `nowayos-ipc` rings - the client's requests (messages read-only region 4, its index read-write region 7) and the stack's reports (messages read-write region 6, the client's index read-only region 3) - and channel 3, and the system's allow list, read-only region 2, prefilled from `tcp-allow-A.B.C.D-PORT.bin`: it opens TCP connections to the destinations the list holds, refuses any other and says why; with no reports ring it serves no client. Its socket storage and the connections' buffers (48 KiB) live in `init`'s frame, which runs rust-sel4's event loop itself (`Handler::run`) and never returns, as a PD has no heap: every system gives it a `stack_size` of 0x40000. A library and a program, so a product runs it from a program of its own - with `init_withholding_destinations`, whose console names no destination, only how many are allowed, for a system whose destination must not reach a log. No `unsafe` code |
| `net-stack-test` (`nowayos-net-stack-test`) | `net-stack-test`, `net-stack-bad-frames` | Test instrument, picking its script by PD name: `seed_source` stands in for the entropy PD, writing one fixed test seed - never random - on the stack's seed ring; `driver_stand_in` stands in for a hostile NIC driver: it checks the stack's first frame is a DHCP discover, takes every receive buffer and passes back two oversize frames, three malformed descriptors and two frames smoltcp cannot use. Never in a demo or product system (`../tools/grant-rules.toml`) |
| `socket-test` (`nowayos-socket-test`) | `tcp-test` | Test instrument: the network stack's socket client, holding no network device. Over its two rings it asks for a TCP connection to 10.0.2.100 port 7, QEMU's echo service; sends a line and checks it comes back byte for byte; closes and waits for the stack's end; then asks for a port and an address the allow list does not hold and checks each is refused as not allowed. Every report is checked by its own `ClientTable` before use. Never in a demo or product system |
| `input-print` (`nowayos-input-print`) | `virtio-input-test`, `input-broker-test` | Test instrument standing in for the UI: receives canonical input events through read-only rings and prints each, so the expect file can check them - run as `ui`, the UI's input ring from the broker (64 slots); otherwise a driver's rings (4 slots each). Never in a product image |
| `input-broker` (`nowayos-input-broker`) | `input-broker-test` | The input broker PD: drains the input PDs' rings in turn, one record from each; refuses and counts malformed records and events the screen (its configuration region) cannot have, clamps positions onto the screen, holds each source to its own rate (`nowayos_input::broker`: 64 events at once, then 250 a second) and sends what passes through the UI's input ring (`nowayos_input::broker::UI_RING_SLOTS`, the ring `nowayos-ui-loop` reads). Holds no device, interrupt or DMA memory (role `broker`); reads only the counter. A library and a program, so a product runs it from a program of its own. No `unsafe` code |
| `input-flood` (`nowayos-input-flood`) | `input-broker-test` | Test instrument standing in for a faulty or hostile input driver: sends the broker a malformed record, a pointer off the screen, a button the screen does not have and 200 key events. Never in a demo or product system (`tools/grant-rules.toml`) |
| `timer` (`nowayos-timer`) | `timer-demo` | The timer PD: answers protected procedure calls for the time and for timeouts (`nowayos-timer-api`) - the time from the counter, as nanoseconds that never go back; arms a hardware timer for the earliest deadline, and alone holds its interrupt; notifies each client whose timeouts expired. The hardware timer is the one the system grants: the CPU's EL1 physical timer when no device is bound (QEMU), BCM2712's system timer, channel 1, when its registers are bound as device 0 (the Pi 5, `systems/rpi5b_2gb/timer-demo.system`) - reported on serial at start-up, with the counter's frequency and, for the system timer, the rate it counts at. A library and a program: a product system runs it from a program of its own that returns `nowayos_timer::init()`. No `unsafe` code. Built for the Pi 5, not yet run there |
| `clock` (`nowayos-clock`) | `clock-test` | The clock PD: holds wall-clock time as an interval from the image's build time - the floor, read from its read-only configuration (`nowayos_region::build_time`) - with no latest until a source answers, carried on in monotonic time from the timer PD at the drift bound (`nowayos_time::Clock`). Takes samples only on the rings its system grants - source `k`'s on channel `k`, its messages read-only region `k - 1` - each copied out, checked and kept only if newest; answers any PD that calls with a reading (`nowayos-clock-api`), and refuses a sample offered by call and a notification on a channel with no ring. Holds no device, interrupt, physical address or key (role `clock`). A library and a program: a product system runs it from a program of its own that returns `nowayos_clock::init()`. No `unsafe` code. QEMU only so far |
| `clock-test` (`nowayos-clock-test`) | `clock-test` | Test instrument, picking its script by PD name: `tls_stand_in` stands in for the TLS PD as the clock's one source - sees the time not known and a date check refused, sends a sample a day after the floor and sees the check pass, then three bad samples and sees the time unchanged after each, and 300 ms on checks the interval moved on and widened exactly as the drift bound says; `reader` holds only a channel it may call the clock on, and cannot move the time by offering a sample. Never in a demo or product system (`../tools/grant-rules.toml`, `[test_only]`) |
| `timer-client` (`nowayos-timer-client`) | `timer-demo` | Run by two PDs, each picking its script by name: `client_alpha` reads the time and checks it never goes back, checks that malformed requests and bad timeouts are refused with their typed errors, and sets timeouts, cancelling one; `client_beta` sets timeouts and checks a fifth is refused. Each checks every expired timeout was not early |
| `ui-demo` (`nowayos-ui-demo`) | `ui-demo` | The UI PD: the clickable demo's screens (`../crates/nowayos-demo`) run through `nowayos-ui-loop`, built once the display announces its layout and device scale, laid out in its viewport at the theme's metrics for that scale, drawn into the framebuffer it shares with the display PD - only what changed, after the first frame - and the time each frame took printed. All a product UI PD adds to the loop is its screens. No `unsafe`, no device |
| `supervisor` (`nowayos-supervisor`) | `supervisor-test`, `supervisor-panic`, `supervisor-counts`, the six `failsafe-*`, `supervisor-empty-list`, `supervisor-bad-list`, `supervisor-failsafe-release` | The fault policy's supervisor: the parent of every other PD. For each fault it reads the label first and decodes only a memory fault (`nowayos-fault`), stops the child, reports it - through a console PD when the system gives it one, as a test system run in the release configuration must - and resets the board with PSCI `SYSTEM_RESET` through its SMC capability (Microkit's slot 7), once the report is printed. A library and a program: the library knows no children, so every system runs it from a program that names its children - each one's `id` in the system description and its name - and returns `nowayos_supervisor::init`; NoWayOS's program picks its test system's children by its own PD name - the grants check runs its image, `nowayos-supervisor.elf`, only in a test system (`tools/grant-rules.toml`, `[test_only]`) - and a product writes its own (the library's documentation shows how). Each child also has its class - its role, as the program declares it: nothing checks it against the grants file - and may be marked to keep running in the fail-safe state if its class may (the watchdog or the display). Given counts - in a test system, a read-only stand-in (region 1) - it reads them at start-up, before any child runs, and decides with `nowayos-fault-policy`: in the fail-safe state it stops every child it does not keep - every one of the 64 slots a child can have, so a child its program does not name too, and every child if it names none; a debug kernel is first asked which slots hold a child, so it prints no error for the empty ones - and a fault then resets nothing; otherwise, at a fault, it reports the counts the next boot would hold. It reads the counter (`nowayos-hw`) for how long the system has run. No `unsafe` code; role `supervisor` |
| `console` (`nowayos-console`) | `supervisor-panic`, `supervisor-failsafe-release` | Test instrument: prints other PDs' lines through the debug UART's own registers (`nowayos-hw`'s PL011 view, with the board's facts about the UART), refusing anything but printable ASCII, and tells each sender when its lines are out. For test systems in the release configuration, whose kernel prints nothing. Never in a product image |
| `display-ramfb` (`nowayos-display-ramfb`) | `ui-interactive` | The interactive demo's display PD: reads the screen's size from its configuration region, gives QEMU a scan-out buffer of that size through ramfb (`nowayos-hw`'s `ramfb`), announces the screen to the UI PD at its own pixels in Xrgb8888, the Pi 5's format, at its device scale (1920x1080 at 2), and for each checked `Present` copies the rectangles it names from the framebuffer it shares read-only with the UI into the scan-out buffer, converting them to ramfb's XRGB8888, prints the whole frame's hash and acknowledges it. A library and a program, so a product runs it from a program of its own - with `init_quiet`, printing and computing no frame's hash, for a screen whose frames are not golden. No `unsafe` code. QEMU only |
| `input-script` (`nowayos-input-script`) | `ui-demo` | An input PD that plays the host demo's scripted session to the UI PD as canonical `InputEvent` records, one step per frame, with one deliberately malformed record. Development only (`[development_only]` in `../tools/grant-rules.toml`: never in a product system) |
| `uart-input` (`nowayos-uart-input`) | `uart-input-test`, its Pi 5 variant | A development route only: keys typed on a terminal attached to the board's debug UART - the Pi 5's Debug Probe, QEMU's serial port - as canonical key events for the input broker. Woken by the timer PD every 10 ms (the UART's receive interrupt does not reach a PD on the Pi 5), it drains the receive FIFO, decodes the bytes (`nowayos_input::terminal`: printable characters, Enter, Backspace, Tab, Shift-Tab, the arrows and Escape; anything else dropped and counted) and sends each press whole - Shift with it - through its ring, metered below the broker's rate so the broker's rate limit drops none of it (the broker still drops events when the UI's ring is full); a lone ESC becomes Escape once a poll finds the line quiet, and a poll spins at most 2 ms in all for a sequence's bytes. Who owns the UART: the kernel transmits (its debug console) and everything the boot firmware set stays as it is; this PD reads UARTCR once (and a PrimeCell's identification registers), then UARTFR and UARTDR, the receive side, and writes nothing - its system maps the registers read-only, which the grants check holds an input PD to. Below the broker, which drains its ring at once when notified. Development only (`[development_only]`: never in a product system, and `make image` names it). A library and a program. No `unsafe` code |
| `display-stub` (`nowayos-display-stub`) | `ui-demo`, its Pi 5 variant | `ui-demo`'s display PD: announces the screen at its own pixels and its device scale - the board's framebuffer window and format where it is given one (device 0 and its facts, region 5: the Pi 5's `firmware_fb`, 1920x1080 x8r8g8b8, at scale 2), else the same stand-in screen - checks each `Present`, shows the rectangles it names on the board's framebuffer at the monitor's own pixels (`nowayos-fb`'s `show_rect`), and acknowledges it; unless quiet it hashes the whole frame (read-only), reads back and hashes what the window shows, and prints both and how long showing took; the expect files hold the golden images' hashes. Given none (QEMU), it only hashes. A system whose framebuffer region is too small for the screen at its own pixels gets the kiosk's 800x480 `Xrgb8888` frames at scale 1, shown at the largest whole-number scale that fits below the window's first row, centred, as an older system's. A library and a program, so a product runs it from a program of its own - with `init_quiet`, printing no frame's hash or timing, for screens that show what a person enters. No `unsafe` code |
| `restart-probe` (`nowayos-restart-probe`) | `restart-test` | Test instruments, by PD name: `restarter` restarts its child `worker` at its restart entry when it faults; `worker` serves `client` over two rings and panics on a crash request; `client` sends again, on `worker`'s new restart epoch, the work that was not answered. Its program starts at the restart entry (`build.rs`) |
| `virtio-console` (`nowayos-virtio-console`) | `virtio-console-test` | The serial driver PD for QEMU's virtio console: finds the console on transport 31 of the virtio-mmio page it is granted, negotiates no optional feature (one port), and passes a byte stream between the device and one client through `nowayos-serial`'s rings - the transmit ring to the device's transmit queue (`virtqueue::TxQueue`), the receive queue (`RxQueue`) to the receive ring - in only the directions it is given (`Directions`): a transmit-only driver sets up no receive queue and binds no receive ring, and the other way round. Received bytes wait in the device until the ring has room; malformed chunks and used entries are dropped and counted. A library and a program: a product system runs it, in the directions it needs, from a program of its own. No `unsafe` code. QEMU only |
| `serial-test` (`nowayos-serial-test`) | `virtio-console-test` | Test instrument standing in for a serial driver's client: sends one malformed record, which the driver must refuse, and two lines, which QEMU loops back, and prints each line it receives, control characters as dots. Never in a demo or product system (`../tools/grant-rules.toml`) |
| `signature-check` (`nowayos-signature-check`) | `signature-test` | Test instrument: checks with `nowayos-signature` the ECDSA P-256 signature OpenSSL made once (`nowayos_signature::known_answer`), then one-bit changes of the message, the signature and the public key, and an r of zero, and prints each result and how long each check took on the counter (`nowayos-hw`'s `cpu`). No heap - nothing in its tree uses `alloc`, or it would not link - and no randomness: `deny.toml` bans `getrandom` in this workspace. Never in a demo or product system |
| `update` (`nowayos-update-pd`) | `update-test` | The update PD, a library and a program: thin over `nowayos-update`'s `Updater`, with `nowayos-signature`'s ECDSA P-256 and SHA-256 as its checks. Takes each bundle - chunk records on a ring, one bundle a notification - in rule 2's order, and prints what became of it; reaches storage only through the storage PD's protected calls and one shared page (`nowayos_update_pd::storage`: read or write a copy of the update state, write image bytes to a slot); checks the held trust list against the image's anchors, a read-only prefilled region. No device, interrupt or physical address |
| `update-bundles` (`nowayos-update-bundles`) | `update-test` | Test instrument, in the TLS PD's place: reads the run's bundles from QEMU's fw_cfg file `opt/nowayos/bundles` by DMA, and offers the update PD one at a time on a ring. fw_cfg is test only. Never in a demo or product system |
| `update-storage` (`nowayos-update-storage`) | `update-test` | Test instrument, the storage PD's stand-in: two copies of the update state in memory, copy A provisioned from a prefilled region, and two 4 KiB slots, each a region; answers the update PD's calls through the shared page, refusing any out of bounds, and prints each copy written. Never in a demo or product system |
| `budget-probe` (`nowayos-budget-probe`) | `budget-test`, `budget-test-widened` | Test instrument, by PD name: `heartbeat` beats every 100 ms on the timer and says whether each beat came on time, and tells `looper` to start after its third; `looper` then loops for ever. Never in a demo or product system |
| `getrandom-probe` (`nowayos-getrandom-probe`) | `getrandom-test` | Test instrument: every way into getrandom - 0.2, 0.3 and 0.4, `rand_core` 0.9's `OsRng`, getrandom 0.4's `SysRng` - refused before `nowayos-entropy` is seeded, writing nothing, drawing from its one generator in turn once seeded with a fixed, public test seed, and refused again once the generator is dropped; one line per check, `ok` or `FAILED`. Never in a demo or product system |
| `tls-test` (`nowayos-tls-test`) | `tls-test` | Test instrument: a rustls client and server on `nowayos-tls-crypto`, joined in memory (`../crates/nowayos-tls-loopback`), complete a TLS 1.3 handshake in this one PD - the server's Ed25519 raw public key made at start from `nowayos-entropy`'s generator, seeded with a fixed, public test value, and pinned by the client - with the plain Certificate message and the largest rustls accepts, printing the suite, the group and the arena's peak; then the client refuses another pinned key. The first PD with a heap: `nowayos-heap`'s, `nowayos_tls_loopback::ARENA_BYTES`. Never in a demo or product system |
| `tls-test-small-arena` (`nowayos-tls-test-small-arena`) | `tls-test-small-arena` | Test instrument: the largest handshake on a 256 KiB arena, below its peak: the arena refuses a request part way through, the PD reports it and the allocation error stops it. Never in a demo or product system |
| `usb-host` (`nowayos-usb-host`) | `usb-test` | The USB host PD: drives one xHCI controller through `nowayos-hw`'s `xhci` view - QEMU's `qemu-xhci`, RP1's on the Pi 5 - with the board's facts about it (read-only region 5). Where the facts give a BAR address (QEMU, which has no firmware to enumerate PCI) it places the controller's window first, through its PCI configuration page (device 1, `nowayos-hw`'s `pci`); on the Pi 5, U-Boot has. It identifies the controller from its capability registers, halts and resets it - the Pi's firmware leaves RP1's running - sets it up in its own DMA region at the bus addresses the facts' DMA offset gives (RP1 sees memory at physical + `0x10_0000_0000`), runs it, has a No Op command come back through DMA and reports each root port. Then, on the timer PD's tick every 8 ms (channel 0; polled: no interrupt reaches it on the Pi), it drains the controller's events and enumerates each device on its ports, one at a time (`nowayos_usb::enumerate`): vendor, product, class, USB release and speed on serial; each HID interface that speaks the boot protocol - a keyboard or a mouse - set up and its reports started; a hub or any other device left alone, its slot given back; a device unplugged forgotten and enumerated afresh when it returns. Each keyboard's and mouse's boot reports become canonical `InputEvent`s (`nowayos_usb::hid`: keys down and up, releases first; the pointer clamped to the screen of read-only region 7; buttons) sent to the input broker through one ring (regions 0, channel 1), a small queue holding what does not fit until the broker - above it - drains the ring, pointer moves folded together; a device unplugged has every key and button it held released; reports of a wrong length, rollovers and undefined keys dropped and counted; a held key pressed again after 500 ms and then about 30 times a second, never a modifier or a lock key (`nowayos_usb::hid::KeyRepeat`). A HID interface outside the boot subclass has its report descriptor read and walked (`nowayos_usb::report`): one that lays out an absolute pointer - a touchscreen, whose touches start, move and end, or a tablet, whose pointer moves and clicks - is set up, its layout printed, and every other left alone. On a DMA region of `DMA_SIZE_STREAMING` with a client's audio rings and channel (regions 1 and 2, channel 2; `nowayos-audio`) it streams a USB speaker and microphone for that client: of each audio streaming interface's alternate settings the best it supports (Type I PCM, one or two channels, 8 to 192 kHz), at the rate it takes nearest 48 kHz - or the product's, `init_with_rate` - set with the audio class's SET_CUR where the endpoint has the control or a choice; the microphone's sound to the client as 16-bit PCM on a lossy ring - only while the client asks, its interface at alternate setting 0 otherwise (a `Capture` record, `nowayos_usb::capture`) - the client's 16-bit PCM to the speaker and silence when it has sent none, an asynchronous speaker paced by its feedback endpoint; a stream it does not support refused, with why. Started with `init_test_tone` (`usb-tone`) it streams audio interfaces' isochronous endpoints with no client, IN and OUT, at full and high speed; started with `init_camera` (`usb-camera`), a USB camera's Motion JPEG frames, whole, into a ring for a client PD - only while the client wants frames, its interface at alternate setting 0 otherwise, its size and rate offered on the ring from when it is set up (`nowayos_usb::video::ring`'s frames-wanted word and offer). Started with `init_calls(rate)` it does both for a call: the audio client on regions 1 and 2 and channel 2, a camera's frames beside it on read-write regions 3 and 4 and read-only region 3, the same client notified on channel 2. A library and a program, so a product runs it from a program of its own. No `unsafe` code. QEMU and the Pi 5 |
| `audio-loop` (`nowayos-audio-loop`) | `usb-audio-loop-test`, `usb-audio-loop-pi-test` | Test instrument (asks the microphone to capture while it records and to stop while it plays back): the audio client of one or two USB host PDs' rings (`nowayos-audio`; side `k` on regions `2k` and `2k+1`, channel `k`), learning from their devices records which has a speaker and which a microphone. With a microphone it records 5 s into its recording region (read-write region 4) while the speaker plays a 440 Hz tone for 1 s, then plays the recording back, converted linearly to the speaker's rate and channels, and again; with none, the tone without end. 16 records kept queued for the speaker. On serial each side's devices, every 10 s sent, and each recording's format, loudest sample, records lost and gaps - never the sound. QEMU and the Pi 5 |
| `usb-tone` (`nowayos-usb-tone`) | `usb-audio-test`, `usb-stream-test` | Test instrument: the USB host PD started with `init_test_tone`, so it also streams every USB audio interface it finds - the isochronous endpoint of each audio streaming interface's first alternate setting that has one (`nowayos_usb::audio`), selected with SET_INTERFACE, 48 ms of transfers kept queued ahead of the controller and topped up each tick (`nowayos_usb::isoch`, `nowayos-hw`'s `xhci` streams): a speaker's OUT endpoint plays a 440 Hz tone at the one rate it takes, a microphone's IN endpoint is counted and never kept. Every 10 s each stream's time and intervals missed ("no interval missed", or "N intervals missed so far": counted, never fatal), and its intervals served and bytes. Needs a DMA region of `DMA_SIZE_STREAMING`; smaller, it streams nothing. QEMU and the Pi 5 |
| `usb-camera` (`nowayos-usb-camera`) | `usb-camera-test` | Test instrument: the USB host PD started with `init_camera`, so it streams audio interfaces as `usb-tone` does and also the first USB camera it finds (`nowayos_usb::video`): its Motion JPEG sizes and rates listed, a camera with none refused, 1280 x 720 at 30 a second chosen where offered and set by the probe and commit, the alternate setting with the most bandwidth selected, its isochronous video endpoint fed from a pool of 254 buffer pages (`nowayos-hw`'s `stream_pool`), each payload's header checked and removed and whole frames put into a ring of four slots for a client (frames region and its page read-write, regions 1 and 2; the client's page read-only, region 1; channel 2); a frame with no free slot dropped, never queued, a damaged one dropped. Every 10 s the frames, their sizes, the drops and why, and the busiest tick. Needs a DMA region of `DMA_SIZE_CAMERA`. The Pi 5 |
| `camera-test` (`nowayos-camera-test`) | `usb-camera-test` | Test instrument: the client of both USB host PDs' rings of frames - each slot's record copied out and checked, each frame's SOI and EOI checked, counted and released; it wants frames from the start and checks the host's offer is its first frame's size and interval; after 605 s of frames it dumps one, base64 with a CRC-32 a line, twice, for `tools/serial-frame`, then wants no more. The Pi 5 |
| `entropy-source` (`nowayos-entropy-source`) | `entropy-test`, `entropy-test-no-rng`, `entropy-rate-test`, `entropy-stuck-test` | The entropy PD: alone owns the device's random source - QEMU's virtio-rng on transport 8, the board file's `rng` - passes every byte through SP 800-90B's two continuous health tests after discarding the first 1,024 (`nowayos-rng-health`), and makes each seed SHA-256 over the tested bytes; seeds go one way on each consumer's own seed ring (`nowayos-seed`, up to `MAX_CONSUMERS`), refilled at no more than `SEEDS_PER_SECOND`, and a taken seed's slot is overwritten. A failed health test latches: no seed again, the test named (never the bytes), and the PD faults on purpose so the supervisor resets the device. Polls at start until every ring is full, then works on the device's interrupt; with no device its consumers stay not ready. Keeps no seed it has handed out, holds no key, calls no PD (role `entropy`). Written from OASIS VIRTIO 1.2 section 5.4. A library and a program. No `unsafe` code. QEMU only: the Pi 5's RNG200 needs its own driver |
| `seed-consumer` (`nowayos-seed-consumer`) | the four `entropy-*` systems | Test instrument standing in for a PD that makes keys: refused bytes before its first seed (not ready), takes a seed off its ring (`nowayos-seed`'s `take`), checks both copies it held are wiped, prints the SHA-256 of 32 bytes drawn - never the bytes - and checks the slot it took no longer holds the seed. Never in a demo or product system |
| `watchdog` (`nowayos-watchdog`) | `watchdog-test`, `pm-watchdog-test`, `run-length-test` and the Pi 5's test systems | The fault policy's watchdog PD: pings every PD it watches on its own channel, and on each tick - which ends a round, its only clock: it makes no call - services the hardware if every PD answered, and otherwise names every PD that missed, highest priority first, and never services again, so the hardware resets the board. In QEMU a stand-in sends the tick and plays the hardware (`init`); on the Pi 5 it arms the PM watchdog itself (`init_pm`), or keeps the board up only while ticks come (`init_pm_ticks_only`, a test system's run length). Runs above every PD it watches but the supervisor (role `watchdog`). A library and a program. No `unsafe` code |
| `watchdog-stand-in` (`nowayos-watchdog-stand-in`) | the watchdog's systems | Test instrument, by PD name: `stand_in`, in QEMU, sends the watchdog its tick every `PERIOD_MS` and reports when the hardware would reset (it resets nothing); `ticker`, on the Pi 5, sends the tick every `TICKER_PERIOD_MS` until the system's run length (read-only region 0, `test-run-length-N.bin`), so the PM watchdog then resets the board into U-Boot's prompt for `tools/pi-run`. Never in a demo or product system |
| `heartbeat-probe` (`nowayos-heartbeat-probe`) | `watchdog-test`, `pm-watchdog-test` | Test instrument: answers the watchdog's heartbeat on channel 0; the PD called `looper` answers `LOOP_AFTER` rounds, then loops for ever at full budget, starving every PD below it. Never in a demo or product system |
| `fan` (`nowayos-fan`) | `fan-test` | The fan PD: runs the Pi 5's active cooler, which the firmware leaves off - full speed at start (`nowayos-hw`'s `rp1_fan`), the SoC's temperature every second from the firmware (`vc_mailbox`), then the device tree's cooling curve (off below 50 °C, a level up at 50, 60, 67.5 and 75 °C, down 5 °C below each), and a line with the temperature, level and rpm at each change and every `REPORT_SECS`. With no temperature it runs at full speed; a late reading is asked for again the next second. A library and a program. No `unsafe` code. The Pi 5 only |
| `fb-test` (`nowayos-fb-test`) | `fb-test` (the Pi 5's) | Spike S2: draws the display test pattern on the framebuffer the Pi 5's firmware set up (`nowayos-fb`), and prints the window's hash before, the pattern's, every byte written's and what reads back, which must match. The Pi 5 only |
| `pl011` (`nowayos-pl011`) | `pl011-demo` | The first driver PD: binds the PL011 UART's registers through `nowayos-hw` with the board's facts about it (`device-debug_uart.bin`), identifies it - by its identification registers on QEMU, by those facts on the Pi 5, whose window holds none - and sends two lines through the UART's own data register, polling the transmit FIFO with a deadline on the counter. No `unsafe` code. QEMU and the Pi 5 |

## Libraries

They need no seL4, so `make pds-test` runs their tests on the container's
own target (Linux on AArch64), outside `.cargo/config.toml`.

| Library | What it provides |
| --- | --- |
| `region` (`nowayos-region`) | Safe, typed views of the memory regions the system description maps into a PD: `read_write(N)` / `read_only(N)` bind the symbols the Microkit tool patches (`nowayos_rw_region_N_*`, `nowayos_ro_region_N_*`), and `ring_producer` / `ring_consumer` put a `nowayos-ipc` ring over two regions so that each side writes only what it owns; `net_queue` views a read-write region as one of sDDF's network queues, for `nowayos-net`'s queue ends. Each end implements only its own side (`ProducerMemory` or `ConsumerMemory`), so the wrong end cannot be attached. `into_frame` views a framebuffer-sized region as atomic bytes: on a read-write map a `Frame` the UI draws into (`nowayos-display`'s `FrameMemory`), on a read-only map loads only. Configuration a system prefills: `screen()`, the screen's size (region 7), `build_time()`, the image's build time as a `nowayos_time::Utc` - the floor for wall-clock time (region 6) - and `device_facts()`, the board's facts about the device a driver holds: its compatible string and how many bytes of its window are registers (region 5). **Approved `unsafe`**, in one module (`src/mapping.rs`) |
| `fault` (`nowayos-fault`) | Decodes the VM fault reports seL4 sends a fault handler (AArch64 exception syndrome: access, cause), with a build-stable summary line |
| `ipc-demo` (`nowayos-ipc-demo`) | The IPC demonstration's shared definitions: ring geometry, the synthetic sequence, the end marker, the consumer's receive step and report lines. Its tests check `../systems/ipc-demo.expect` |
| `timer-api` (`nowayos-timer-api`) | The timer PD's interface, naming no board: requests (the time; set, cancel and collect timeouts) and replies as a label and message registers, `Instant` (nanoseconds since the counter started, never decreasing; defined in `../crates/nowayos-time` beside wall-clock time and re-exported here) and typed errors; `schedule::Timeouts`, the timer PD's bookkeeping (deadlines rounded up so none is early, at most 4 per client, up to 24 hours each); with the `client` feature, `Client`, which makes the calls from inside a PD. Host-tested without seL4 |
| `clock-api` (`nowayos-clock-api`) | The clock PD's interface: a reading by protected procedure call - label `READ`, answered with `nowayos-time`'s `Reading` record as five message registers, validated field by field when decoded - and typed refusals: an unknown request, the wrong number of words, a sample offered by call (`OFFER`: samples come only on a source's ring) and no monotonic time; `serve` and `take_samples`, the clock PD's side, which copy each record off a source's ring, at most its capacity per notification, and offer it to `nowayos_time::Clock`; with the `client` feature, `Client`, which makes the call from inside a PD. Host-tested without seL4 |
| `ui-loop` (`nowayos-ui-loop`) | The UI PD's reusable loop: the display contract's UI end, drawing a `nowayos-ui` tree into the shared framebuffer with `nowayos-render-eg` - whole (`draw_all`) or only what changed (`draw_dirty`) - presenting; the display's device scale, the viewport and the theme's metrics at that scale for the screens; `UiChannels` binds the rings to and from the display PD, the input ring (each record copied out before it is decoded) and the framebuffer. Its contract and drawing logic is host-tested |
| `entropy` (`nowayos-entropy`) | Randomness in a PD: the PD's own generator - ChaCha20, RustCrypto's `ChaCha20Rng`, re-keyed from its own next 32 bytes after every request, the old state wiped - seeded by `seed` (in time, from the entropy PD), dropped by `forget`; and the getrandom back end: one function behind getrandom 0.2's `register_custom_getrandom!` and behind the `__getrandom_v03_custom` symbol 0.3 and 0.4 share (`.cargo/config.toml` sets `--cfg getrandom_backend="custom"`). Refuses before the first seed, a request over 65,536 bytes, and every request after the 65,536th since the seed; a refused request writes nothing. For single-threaded PDs only |
| `heap` (`nowayos-heap`) | A PD's heap: sel4-dlmalloc over a fixed static arena, as sel4-microkit's `heap_size` declares it, wrapped by `CountingHeap`, which counts the bytes in use, the peak and the refusals and calls the PD's hook on a refusal; `declare_heap!` declares it as the PD's global allocator. A PD with large working memory declares one over a memory region instead (`declare_region_heap!`, `give_region`): the region - bound with `nowayos_heap_region_vaddr` and `_size`, its PD's alone (check-system's rule 16) - is seL4's memory, zeroed, never in the program image the loader copies at boot. Its tests run under Miri (`make pds-miri`), not yet `make pds-test` |
| `restart` (`nowayos-restart`) | Restarting one PD cleanly: the restart image's format, parsed and checked, and its restoring; with the `entry` feature, the restart entry - where a restartable PD's program starts (`--entry=nowayos_restart_entry`), which checks the PD's writable memory against its read-only restart image at a cold start, and puts the image back at a restart, before rust-sel4's runtime; with the `parent` feature, `restart(child, header)` for its parent. The image is made by `make image` (`../tools/restart-image`) |
| `hw` (`nowayos-hw`) | The hardware a driver PD uses, through safe, typed views: `device(N)` binds a device's register window from the symbols the Microkit tool patches (`nowayos_device_N_*`); a device module views it through `safe-mmio`, which never makes a Rust reference to device memory - `pl011::Pl011` for the Arm PL011 UART, with every register's offset asserted at compile time, viewed as the board's facts name it: a whole PrimeCell (`arm,pl011`), its identification registers checked, or BCM2712's `arm,pl011-axi`, whose 0x200 bytes of registers hold none, read no further than UARTCR, and `fw_cfg::FwCfg` for QEMU's test-only firmware configuration device; `system_timer::SystemTimer` for the system timer of Broadcom's BCM2835 family (the Pi 5's), which reaches only the ARM's compare channels, 1 and 3, never the VideoCore's; `dma_region(N)` binds DMA memory with its physical address (`nowayos_dma_N_*`), read and written by offset, and gives a device only addresses checked to lie inside it; `io_region(N, size)` binds memory another PD maps by its physical address alone (`nowayos_io_N_paddr`: a network client's packet buffers, never mapped here), and gives a device only addresses checked to lie inside it; `virtqueue` lays out a device's queues in DMA memory and checks every used entry - `RxQueue`, `TxQueue` (buffers the device reads), and `ChainQueue`, a header and an I/O-region buffer per chain - and `virtio_net` drives the network card's receive and transmit queues with them, dropping and counting oversize and malformed frames; `xhci` drives an xHCI USB host controller - its registers viewed as plain words, every offset from its capability registers checked against its window, its rings and contexts in its own DMA region at bus addresses, every event read cycle bit first, copied and checked against what was queued - and `pci` places a PCI function's BAR 0 through its configuration page, where no firmware has (QEMU); `gem` drives a Cadence GEM Ethernet MAC - its registers as plain words, at the offsets `nowayos-gem` names, its two descriptor rings in its DMA region and the client's buffers at bus addresses, every descriptor word copied and checked by `nowayos-gem` - and `rp1` releases the Pi 5's Ethernet PHY reset through RP1's GPIO, touching that one line; `cpu` reads the counter and its frequency and issues barriers. **Approved `unsafe`**, in twelve modules |
| `fb` (`nowayos-fb`) | Showing frames on a framebuffer the firmware set up and the display scans out - the Pi 5's HDMI framebuffer: binds the window the board's facts give (`firmware_fb`, read-only region 5) and places a frame on it, at the window's own pixels or scaled by the largest whole number that fits, centred; `show_rect` copies a frame's rectangles to the screen |

## `unsafe` in this workspace

`unsafe_code` is forbidden workspace-wide except in six crates, each of
which allows it only in the modules listed below and states every invariant
beside the code.
They are the entries of the approved-`unsafe` list,
`../tools/unsafe-allowlist.toml`, and `make unsafe-audit` fails if `unsafe`
code appears anywhere else:

| Crate | Module | Why |
| --- | --- | --- |
| `nowayos-region` | `src/mapping.rs` | Declaring the symbols the Microkit tool patches (`export_name`, `link_section`), and turning a patched region address into a `&'static` view of atomics - a typed layout, or the whole region as atomic bytes for a framebuffer. Tested under Miri (`make pds-miri`) |
| `nowayos-entropy` | `src/cell.rs`, `src/backend.rs` | The PD's one generator in a static - `unsafe impl Sync`, sound because a PD has one thread, its cell refusing re-entry - and `__getrandom_v03_custom`, the `#[no_mangle]` back end getrandom 0.3 and 0.4 call with a raw pointer and a length. Tested under Miri (`make pds-miri`), but for the check that an error made as getrandom 0.3's arrives intact as 0.4's, which `make pds-test` runs
| `nowayos-heap` | `src/counting.rs`, `src/region.rs` | A PD's global allocator is an `unsafe impl GlobalAlloc`: `CountingHeap` forwards each call, with the arguments it was given, to sel4-dlmalloc over the PD's static arena, and counts around it. A heap in a memory region takes its address and size from two symbols the Microkit tool patches, defined as `nowayos-region` defines its own (`unsafe(export_name)`, `unsafe(link_section)`). Tested under Miri (`make pds-miri`) over the system allocator, one that refuses everything, and a page-aligned test arena |
| `nowayos-restart` | `src/entry.rs` | The restart entry, which runs before rust-sel4's runtime: its assembly, the restart image's patched symbols (`export_name`, `link_section`), and views of the image and of the PD's own writable segments - checked against its program headers - made from addresses |
| `nowayos-isolation-probe` | `src/forbidden.rs` | The deliberately forbidden memory accesses the isolation tests need, each one inline-assembly instruction: a read of an address from a fixed list, unmapped in the probe that reads it (`ui-isolation-widened` maps one on purpose), and a write to a read-only region. A test instrument, never linked into a product image |
| `nowayos-hw` | `src/device.rs`, `src/dma.rs`, `src/pl011.rs`, `src/fw_cfg.rs`, `src/cpu.rs`, `src/system_timer.rs`, `src/virtio.rs`, `src/scanout.rs`, `src/pci.rs`, `src/xhci.rs`, `src/gem.rs`, `src/rp1.rs` | Declaring the device, DMA-region and I/O-region symbols the Microkit tool patches, and turning a patched address into a `safe-mmio` view of a register block, a slice of plain words with no safe access (`view_words`: a PCI configuration page, an xHCI controller's window) or a slice of atomic bytes; each register block's layout promise (`unsafe impl RegisterBlock`) and `safe-mmio`'s `field!` projections, which expand to `unsafe` blocks; each plain word's read and write, stated safe where it is made; the writes that start DMA, with addresses inside the PD's DMA region; the counter and barriers, which are inline assembly. Its binding and DMA-memory logic runs under Miri; register accesses run in `make pds-test` (real load and store instructions on ordinary memory) and in QEMU |

These five crates carry their own `[lints]` tables (the workspace ones with
`unsafe_code = "deny"`); `make unsafe-audit` checks they stay in step with
`Cargo.toml`, and that every other crate takes the workspace lints.

## Adding a PD

1. Create `NAME/` with a binary crate whose package name is the program image
   name used in the system description (`<program_image path="NAME.elf"/>`).
2. Add it to `members` in `Cargo.toml`; depend on
   `sel4-microkit.workspace = true` and set `[lints] workspace = true`
   (`make unsafe-audit` checks this).
3. `#![no_std]`, `#![no_main]`, no global allocator (so any heap use fails to
   link) - unless it needs a heap, as a TLS PD does: then `nowayos-heap`'s
   `declare_heap!`, its arena sized from measurement - or, for a heap of
   megabytes, `declare_region_heap!` and a memory region its PD alone maps,
   which `give_region` hands the heap first in `init` - and the crate in
   `PDS_LINT_ALONE` (`../tools/make/system.mk`), so that `make pds-lint`
   never unifies its dependencies' features into the heap-less PDs' - no
   `unsafe` of its own (`unsafe_code` is forbidden workspace-wide):
   bind shared memory through `nowayos-region`. Library code that needs no
   seL4 belongs in `../crates/`, or in a library here that `make pds-test`
   covers (add it to `PDS_LIB_CRATES` in `../tools/make/system.mk`).
4. Reference it from a system description in `../systems/`, list everything
   that description grants in its `.grants` file (see `../systems/README.md`),
   and run `make qemu-test SYSTEM=...` and `make pds-lint pds-deny pds-test`.
5. If product systems are to run it too, make it a library and a program
   (`timer`, `virtio-console`, `supervisor`, `display-stub`, `input-broker`,
   `uart-input`, `usb-host`): the library exports the PD's
   state and an `init` that returns it, and the program only wraps `init` in
   `#[protection_domain]`, with whatever the system must tell it - the
   directions a serial driver carries, the children a supervisor has. A
   product's own program then does the same, with the library as a path
   dependency (its program images come from its own `pds/` workspace).

`debug_println!` only exists in the Microkit `debug` configuration.

### When several PDs print

The kernel's debug output is a system call a character, not a line, and a
real UART takes about 87 µs a character at 115200 baud - 8 ms for a long
line, where QEMU's serial takes none. A PD held mid-line lets another print
into its line (`ipc-demo` on the Pi 5). On one core, with distinct
priorities, nothing lower runs while a PD prints, so a line is split only
when the PD printing stops being the highest that can run:

- **its budget runs out** - budget below period, as `ipc-demo`'s producer
  (5 ms of every 10 ms): a lower PD runs meanwhile, and must have nothing to
  print then;
- **an interrupt wakes a higher PD** - a timer's: `timer-demo`'s clients
  print when their timeouts fire, and stay whole only because no deadline
  falls within another client's line (each at least 160 ms from the next at
  115200 baud);
- two PDs at one priority, or on two cores (no NoWayOS system has either).

So a PD that may be held mid-line hands over the turn: it prints, then
notifies, and the PD it notifies prints nothing - not even its start-up
line - until that notification comes (`ipc-producer` and `ipc-consumer`).
Never print after notifying a PD that prints. A system's expect file checks
its lines whole on the Pi as in QEMU; to see a change hold at a real line
rate, boot it with QEMU's serial drained at 115200 baud.

## Adding a driver PD

A driver PD is a PD (above) that owns a device. It still has no `unsafe` code
of its own: everything raw goes through `nowayos-hw`, and the
`pl011` PD is the worked example.

1. **Sources first.** Work from the device's datasheet or specification,
   and list every source read - an AI assistant's included - in the commit
   message and pull request. Never port or translate a GPL driver: work
   clean-room, from datasheets and specifications.
2. **The board file.** The device must be in `../platforms/BOARD.toml`, with
   its register window, interrupts, trigger and the source of those facts -
   and its compatible string, and `registers` when fewer than the window's
   bytes are registers, if the driver is given its facts (step 4).
3. **The register block, in `nowayos-hw`.** If the device has no module yet,
   add one (`hw/src/DEVICE.rs`, listed in `../tools/unsafe-allowlist.toml` -
   a security change): a `repr(C)` layout of `safe-mmio` field wrappers with
   every register's offset asserted at compile time, `ReadPure` only where a
   read has no side effect, no register that can start DMA reachable through
   a safe wrapper, an identification check before a view is handed out, and
   safe methods for what drivers need. Host tests run it against ordinary
   memory (`make pds-test`); Miri cannot run register accesses. Where a
   board's device holds no identification registers, or fewer registers
   than its page, the view takes the board's facts instead and reads
   nothing outside them: the PL011's `arm,pl011-axi` on the Pi 5.
4. **The system description.** Map the device's exact window uncached, binding
   `nowayos_device_N_vaddr` / `nowayos_device_N_size`; take its interrupt with
   `<irq>` if the driver uses it. List both in the `.grants` file by device
   name (`device PD NAME rw`, `irq PD NAME INTID`); `tools/check-system`
   refuses a device symbol anywhere else. A driver that needs the board's
   facts about its device - its compatible string and how many bytes of its
   window are registers - maps them read-only as `nowayos_ro_region_5`, a
   memory region prefilled from `device-NAME.bin`, which `make image` writes
   from the board file (`nowayos_region::device_facts`; `map PD REGION r` in
   the `.grants` file); the grants check lets only the PD that maps NAME's
   window map them (rule 12).
5. **DMA memory, if the device does DMA.** A memory region without
   `phys_addr`, mapped with `nowayos_dma_N_vaddr` / `nowayos_dma_N_size`, and
   `<setvar symbol="nowayos_dma_N_paddr" region_paddr="REGION"/>` naming the
   same region (`tools/check-system` refuses a mismatch); list it as `map` and
   `paddr` in the `.grants` file - a physical-address grant, because the PD
   can point the device at that memory. Any register that starts DMA stays
   out of the safe view: the device module starts transfers itself, with
   addresses from `DmaRegion::device_address`, which checks they lie inside
   the region. The `dma-demo` PD is the worked example.
   Memory the device reads or writes that belongs to another PD - a
   network client's packet buffers - is an I/O region instead: the driver
   learns only its physical address, with `<setvar
   symbol="nowayos_io_N_paddr" region_paddr="REGION"/>`, never maps it
   (`tools/check-system` refuses a map of it in the same PD), and binds it
   with `nowayos_hw::io_region(N, size)`, stating the size it assumes -
   Microkit gives no size with a `<setvar>`, so `../tools/test_tools.py`
   checks each description against it. The `virtio-net` PD is the worked
   example.

6. **The driver.** Bind with `nowayos_hw::device(N)`, then the device module's
   constructor (and `nowayos_hw::dma_region(N)`). Bound every wait (the
   `pl011` PD gives each byte 10 ms on the counter), and report failures
   rather than panicking.
7. **Test it in QEMU** with an expect file, before any board.

## Dependencies and licences

`make pds-deny` checks every crate in `Cargo.lock` against the house licence
list, dev-dependencies included, refuses registry dependencies written as
`"*"`, and bans browser-engine, WebView, window-system and C GUI crates by
name. The standard-library crates that build-std compiles into every PD are
not in `Cargo.lock`, so cargo-deny cannot see them: `std-crates.txt` lists
them with their licences, and `make std-crates-check` fails if the set or a
licence changes (for example with a nightly bump). rust-sel4's crates in this tree are BSD-2-Clause or MIT (checked file by
file at v5.0.0: no GPL files; rust-sel4's GPL-2.0-only files belong to
`sel4-kernel-loader`, which is not used). The PD crates depend only on each
other, on `../crates/nowayos-ipc`, `../crates/nowayos-input`,
`../crates/nowayos-net`, `../crates/nowayos-time` and
`../crates/nowayos-ip`, `../crates/nowayos-seed` and `../crates/nowayos-bounded`, on rust-sel4's
`sel4` (MIT), which was already in the tree, and - `nowayos-hw` only - on `safe-mmio` 0.3.2 (MIT OR Apache-2.0) and its run-time
dependency `zerocopy` with `zerocopy-derive` (BSD-2-Clause OR Apache-2.0 OR
MIT); `zerocopy-derive`'s build-time dependencies were already in the tree. The
signature check's test PD brings, through `../crates/nowayos-signature`,
RustCrypto's `p256` 0.14.0 and `sha2` 0.11.0 with their default features off,
and their trees: each crate MIT or Apache-2.0 at the user's choice but
`subtle` (BSD-3-Clause); no `getrandom` (admitted here, in `deny.toml`, only under `nowayos-entropy`) and no
`alloc` (a PD has no global allocator, so a crate that used it would not
link). The network stack PD brings, through `../crates/nowayos-ip`,
smoltcp 0.14.0 and its dependency `managed` 0.8.0, both 0BSD - outside the
house list, signed off, excepted at those
versions in `deny.toml`, whose `[[bans.features]]` holds smoltcp to
`medium-ethernet`, `proto-ipv4`, `proto-dhcpv4`, `socket-dhcpv4` and
`socket-tcp` (which adds no crate) - and
smoltcp's `heapless` 0.9.3 with `hash32` and `stable_deref_trait`,
`bitflags` 1.3.2 and `cfg-if` (MIT OR Apache-2.0) and `byteorder`
(Unlicense OR MIT: MIT taken). Outside the house list: `unicode-ident` (adds Unicode-3.0;
build-time only; signed off 2026-10-01) and the build-std crate
`compiler_builtins` (includes Apache-2.0 WITH LLVM-exception; linked into
every PD; signed off 2026-10-02). The TLS test PDs bring,
through `../crates/nowayos-tls-loopback` and `../crates/nowayos-tls-crypto`,
rustls 0.23.46 and RustCrypto's provider crates - the root workspace's list
, with `rsa`'s timing advisory ignored as no RSA private key is
used - and `nowayos-heap` brings rust-sel4's `sel4-dlmalloc` (BSD-2-Clause)
and `one-shot-mutex`, already in the tree. Randomness: `nowayos-entropy`
brings RustCrypto's `chacha20` 0.10.2 (features `rng` and `zeroize`, with
`cipher`, `cpufeatures` and `rand_core` 0.10.1) and `zeroize` 1.9.0, and
getrandom 0.2.17 and 0.3.4; the getrandom-test instrument adds getrandom
0.4.3 and `rand_core` 0.9.5; each MIT or Apache-2.0 at the user's choice,
as are their platform trees (`libc`, `r-efi`, `wasi`, `wasip2`), which no
PD compiles.

## Provenance

`hello` was written for NoWayOS. It follows the minimal protection-domain
shape that `sel4-microkit`'s API dictates — the `use` line, a
`#[protection_domain]` init function returning a handler, and an empty
`Handler` impl — which is also the shape of rust-sel4 v5.0.0's
`crates/examples/microkit/hello` example (BSD-2-Clause, Copyright 2023,
Colias Group, LLC); only that API boilerplate is shared. To be safe, the
project owner decided (2026-10-02) that the file keeps upstream's
copyright line and SPDX identifier - the one NoWayOS file that carries a
notice while NoWayOS's own licence is undecided.

`hw`'s `fw_cfg` module (`src/fw_cfg.rs`) was written from QEMU's fw_cfg
specification, a GPL-2.0 document, using only the interface it describes:
none of its text, and no QEMU source was read. The project owner accepted
this for test-only code (2026-10-03): only QEMU systems use the
module - the `dma-demo` test system and `display-test`'s `ramfb` display
PD, QEMU's own display device - and it never goes into a product image.
`ramfb`'s configuration layout comes from edk2's `QemuRamfbDxe`
(BSD-2-Clause-Patent).

# systems: Microkit system descriptions

Each `NAME.system` is a Microkit system description (SDF): the protection
domains, their memory regions, channels and priorities. It names devices and
states no board fact, so one source serves every board: `make image
SYSTEM=NAME BOARD=BOARD` first writes the board's description,
`build/BOARD/NAME/NAME.system`, filling in each device's window and
interrupt from `../platforms/BOARD.toml` (`../tools/board-system`), then
turns it into `build/BOARD/NAME/loader.img` (and `report.txt`, and
`capdl.json`, the Microkit tool's description of the kernel objects).
Each has two companions: `NAME.expect`, the serial output `make qemu-test`
and `make check-serial` check, and `NAME.grants`, everything the description
grants.

| System | Contents |
| --- | --- |
| `hello` | one PD, `nowayos-hello`; no channels, memory regions or devices |
| `ipc-demo` | `input_producer` and `input_consumer` exchange typed `InputEvent` records through a four-slot ring over two 4 KiB regions (messages: producer `rw`, consumer `r`; control: consumer `rw`, producer `r`), one channel; one malformed record must be rejected. The producer (the input side) runs above the consumer and has a CPU budget of half its period |
| `isolation-test` | `isolation_parent` with two child probes that each attempt one ungranted access (read of an unmapped address; write to a read-only page), and a `bystander` that must keep running; one channel parent–bystander |
| `dma-demo` | `dma_demo`: QEMU's fw_cfg register page (`fw_cfg`) uncached, and one 4 KiB DMA region with its physical address (`paddr`), bound through `nowayos-hw`; fw_cfg writes into the region by DMA. QEMU only |
| `timer-demo` | `timer` holds the EL1 physical timer's interrupt (`phys_timer`) and answers `client_alpha`'s and `client_beta`'s protected procedure calls for the time and for timeouts; each client has one channel, on which it calls the timer and the timer notifies it. Priorities and budgets are justified in its `.grants` file. The Pi 5 builds its own variant, `rpi5b_2gb/timer-demo.system`: the timer maps BCM2712's system timer page (`system_timer`) uncached and holds channel 1's interrupt instead; the same PDs, channels and expect file. Built for the Pi 5, not yet run there |
| `clock-test` | `clock` holds wall-clock time: it maps the image's build time read-only (region 6) and its ends of `tls_stand_in`'s ring - the control read-write, the samples read-only - calls the timer and is notified only by `tls_stand_in`, the one source; `reader` holds one channel, on which it may only call the clock. The clock has the role `clock`: no device memory, interrupt or physical address, and only `tls_stand_in`, a test PD, can write what it reads (`clock-samples-only-from-network`). Every printed line is fixed text relative to the floor, so its expect file holds whenever the image was built. QEMU only (`phys_timer`) |
| `ui-demo` | The clickable demo in QEMU: `input` (role `input`) plays the host demo's session as `InputEvent` records; `ui` (role `ui`) handles them and draws each frame into `framebuffer` (read-write; read-only in `display`); `display` (role `display`) checks each `Present` and hashes the frame. Three rings, each over two regions; two channels. No device, interrupt or child. The screen is the Pi 5's window, 1920x1080 r5g6b5 at device scale 2, drawn at its own pixels; `framebuffer` is 4 MiB. Its Pi 5 variant, `rpi5b_2gb/ui-demo.system`, also gives `display` the board's framebuffer, `firmware_fb`, cached (it cleans each line it writes to memory), and its facts, read-only: the same program then shows each frame on the HDMI monitor, on every row with no border, and `rpi5b_2gb/ui-demo.expect` adds the scan-out hash of each frame to `ui-demo.expect`'s lines |
| `ui-isolation` | Negative tests at the UI PD's boundaries: `ui_isolation_parent` with four child probes, each mapping exactly what `display` or `ui` maps in `ui-demo` (and with that role): `display_writes_framebuffer` writes the framebuffer it maps read-only; `ui_writes_display_index` writes `present_control`, the display's index on the Present ring, read-only to the UI; `ui_reads_display_private` reads where `display_side` keeps its private page; `ui_reads_device` reads the PL011 UART's physical address as a virtual address. `display_side`, a bystander standing in for the display's private state, must keep running; one channel parent–`display_side`. Its device probe reads QEMU `virt`'s UART address |
| `ui-isolation-widened` | `ui-isolation` with one grant widened on purpose: `ui_reads_display_private` also maps `display_side`'s private page. Its expect file requires the isolation failure `ui-isolation.expect` forbids, so it shows the test can fail; `make tools-test` checks the two systems differ by exactly that grant, and that each probe in `ui-isolation` maps what its `ui-demo` PD maps |
| `display-test` | The display PD for QEMU's `ramfb` device (chosen as the device closest to the Pi 5's framebuffer): `ramfb` maps fw_cfg's register page (`fw_cfg`) uncached, a 4 KiB DMA region for fw_cfg's requests (`requests`) and the framebuffer, one 2 MiB DMA page (`framebuffer`), and learns both regions' physical addresses; it draws a test pattern and gives QEMU the framebuffer. `display-test.qemu` adds `-device ramfb`, so `make qemu-test` compares QEMU's screen with `display-test.screen.png`. QEMU only |
| `virtio-input-test` | The virtio-input driver PD: `virtio_input` (role `input`) maps one page of virtio-mmio transports (`virtio_mmio_24_31`) uncached, finds QEMU's keyboard and tablet there, negotiates their features, sets up each device's event queue in a 4 KiB DMA region (`keyboard_events`, `tablet_events`, whose physical addresses it learns) and holds their two interrupts; it turns their events into canonical `InputEvent`s and sends them through each device's four-slot ring - the pointer ring and the keyboard ring - to `input_print` (role `test`), which prints each. `virtio-input-test.qemu` gives QEMU the devices; `virtio-input-test.qmp` types keys, moves and clicks the tablet, and sends an unknown key and a burst bigger than the ring, which are dropped and counted. QEMU only |
| `input-broker-test` | The input broker PD: `input_broker` (role `broker`: no device or interrupt) drains three rings - `virtio_input`'s pointer ring (source 1) and keyboard ring (source 2), and the ring from `input_flood` (source 3, role `input`), a test instrument standing in for a faulty driver - checks each event against the screen, holds each source to its own rate, and sends what passes through the UI's input ring (64 slots) to `ui` - `nowayos-input-print`, run in the UI's place (role `ui`). At start-up `input_flood` sends a malformed record, a pointer off the screen, a button the screen does not have and 200 key events: refused, clamped, refused, and dropped and counted beyond the rate. `input-broker-test.qmp` then types and clicks, which must reach `ui` unaffected. Priorities and budgets are justified in its `.grants` file. QEMU only |
| `ui-interactive` | The clickable demo with QEMU's own display and input: `ui` (role `ui`), `nowayos-ui-demo` with exactly its grants in `ui-demo`; `display` (role `display`), `nowayos-display-ramfb`, plays the display contract and shows each frame on QEMU's ramfb - fw_cfg's registers, a DMA region for its requests and an 8 MiB scan-out region, four 2 MiB pages placed contiguously; `virtio_input` (role `input`), the keyboard and tablet as in `virtio-input-test`; `input_broker` (role `broker`) merges their rings into the UI's input ring. The screen's size, 1920x1080 - the Pi 5's window, drawn at its own pixels at device scale 2 - is prefilled configuration, read-only in the display, the driver and the broker. `make qemu SYSTEM=ui-interactive VNC=1` shows it over VNC to click through; `ui-interactive.qmp` plays the host demo's session and compares QEMU's screen after each step with the step's golden image (`../crates/nowayos-demo/session/`). QEMU only |
| `restart-test` | Restarting one PD cleanly: `worker` serves `client` over two rings; a crash request makes it panic, and its parent `restarter` restarts it at its restart entry (`nowayos-restart`), which puts back its writable memory from its restart image, made when the system image is built (`tools/restart-image`) and mapped read-only into it and its parent (`tools/check-system` rules 12 and 13). `client` sees the new restart epoch and sends again the work not answered. Test systems only: no product system may have a restart image |
| `supervisor-test` | The fault policy's supervisor (built ahead of its acceptance): `fault_supervisor` (role `supervisor`, the SMC capability) is the parent of every other PD. `faulter` (role `driver`) checks and writes a marker page, then writes its read-only page when `trigger` notifies it; the supervisor stops it, reports the decoded fault and resets the board with PSCI `SYSTEM_RESET`. The test sees the system boot again and the marker gone |
| `supervisor-panic` | The same when a child panics, in the debug and the release configuration (`supervisor-panic.configs`): `panic_supervisor` reads the fault's label before parsing it, reports it undecoded and resets the board; everything checked is printed by `console`, a test instrument driving the debug UART's registers - identified by the board's facts about it, `device-debug_uart.bin` - since the release kernel prints nothing. A test system, so the supervisor may hold a ring and a channel to the console |
| `supervisor-counts` | The fault policy's counts below their limits: `counts_supervisor` reads its stand-in counts - `counts`, prefilled from `test-counts-1-0.bin` and mapped read-only, in a test system only - before any child runs: restart count 2 of 4, so every child runs. `faulter` (role `storage`, a sensitive class) faults on `trigger`'s notification, and the supervisor reports the counts the next boot would hold - restart count 3, sensitive count 1 - and resets the board, as `supervisor-test` does. The stand-in stores nothing, so the next boot reads the same counts |
| `failsafe-restart-limit`, `failsafe-sensitive-limit`, `failsafe-no-counts`, `failsafe-counts-cut-short`, `failsafe-counts-version`, `failsafe-counts-check` | 7 | The fail-safe state at start-up: `failsafe_supervisor`'s stand-in counts are at the restart limit, at the sensitive limit, missing, cut short, of an unknown version or failing their check - the six systems differ in nothing else, which `make tools-test` checks - so before any child runs it enters the fail-safe state: it stops `stopped_child` and `unnamed_child`, a child its program leaves out on purpose, which never print their start lines, and keeps `kept_child`, standing in for the watchdog PD a product keeps (all three role `test`; the supervisor's program classes `kept_child` as the watchdog). When `kept_child` writes its read-only page, the supervisor stops and reports it and resets nothing |
| `supervisor-empty-list` | A supervisor whose program names no children fails closed: NoWayOS's supervisor program has no list for `empty_list_supervisor`, so it runs with none, as a product's program would with the empty list `nowayos_supervisor::valid` refuses. It still reads its stand-in counts, at the restart limit (`test-counts-4-0.bin`), and before any child runs stops every child - `stopped_child` and `unnamed_child` (role `test`), each reported as an unknown child, which never print their start lines - and keeps none |
| `supervisor-bad-list` | A list of children the supervisor's library refuses: NoWayOS's program gives `bad_list_supervisor` a list `nowayos_supervisor::valid` refuses, which no program that asserts it builds with - `kept_app`, of class `app`, marked to keep running in the fail-safe state, which only the watchdog and display classes may; `stopped_child`; id 1 named again, marked to keep; and id 64, which no Microkit child has. The supervisor reports each wrong entry at start-up and honours none: with its stand-in counts at the restart limit (`test-counts-4-0.bin`), it keeps nothing and stops both children (role `test`), which never print their start lines |
| `supervisor-failsafe-release` | The fail-safe state at start-up in the debug and the release configuration (`supervisor-failsafe-release.configs`): `failsafe_release_supervisor`, with its stand-in counts at the restart limit, keeps `console` running - a test instrument driving the debug UART's registers, as in `supervisor-panic`, since the release kernel prints nothing; the supervisor's program classes it as the display, which a product keeps for its out-of-service image - and stops `unnamed_faulter` (role `test`), which its program leaves out on purpose and which would fault if it ran. A debug kernel is asked which slots hold a child; a release kernel answers `InvalidCapability` for an empty one, so this system shows both ways. A test system, so the supervisor may hold a ring and a channel to the console |
| `input-isolation` | Negative tests at the UI's side of the input broker: `input_isolation_parent` with two child probes, each mapping exactly what `ui` maps in `input-broker-test` (role `ui`): `ui_writes_driver_ring` writes where the broker maps the tablet's ring and `broker_side`, a bystander, keeps its private page; `ui_writes_broker_ring` writes its own read-only ring from the broker. `make tools-test` checks each probe maps what `ui` maps there |
| `net-test` | The NIC driver PD for virtio-net: `virtio_net` (role `driver`) maps one page of virtio-mmio transports (`virtio_mmio_24_31`) uncached, finds QEMU's network card there on transport 31, negotiates `VIRTIO_NET_F_MAC` and reads its MAC address, sets up its receive and transmit queues in two DMA regions of its own (`rx_ring`, `tx_ring`, whose physical addresses it learns), and holds its interrupt; it learns the physical addresses of the client's receive and transmit buffers (`rx_buffers`, `tx_buffers`, 128 buffers of 2048 bytes each) but never maps them. sDDF's four network queues (`rx_free`, `rx_active`, `tx_free`, `tx_active`), one page each, are read-write in both PDs. `net_client` (role `network`), a test instrument standing in for the network stack, maps the receive buffers read-only and its transmit buffers read-write; it sends five malformed descriptors, which the driver drops and counts, and an ARP request for QEMU's gateway, and prints the reply. `net-test.qemu` gives QEMU the card on a user-mode network cut off from the host (`restrict=on`). QEMU only |
| `net-stack-test` | The network stack PD on smoltcp: `virtio_net`, the NIC driver of `net-test` with the same grants; `net_stack` (role `network`) maps the four queues read-write, the receive buffers read-only and its transmit buffers read-write, and its end of a seed ring - the control read-write, the seed read-only - and holds no device, interrupt or physical address; `seed_source` (role `test`) stands in for the entropy PD and writes one fixed test seed there; `timer` gives the stack monotonic time and wakes it when smoltcp is due. The stack takes a DHCP lease from QEMU's user-mode network: 10.0.2.15/24, gateway 10.0.2.2. `net-stack-test.qemu` leaves that network unrestricted (`restrict=off`), as QEMU's DHCP server names a gateway only then; the stack has no socket but its DHCP client. QEMU only |
| `net-stack-no-seed` | `net-stack-test` with no seed: no `seed_source`, seed ring or seed channel - `make tools-test` checks it differs in nothing else - so the stack says it is not ready and never brings its interface up. QEMU only |
| `net-stack-bad-frames` | The stack against a hostile driver: `driver_stand_in` (role `test`) holds the driver's ends of the four queues, maps the receive buffers read-write and the transmit buffers read-only, and passes back two oversize frames, three malformed descriptors and two frames smoltcp cannot use; the stack, with exactly its grants in `net-stack-test` (`make tools-test` checks), drops and counts the first five and carries on. No network card. QEMU only (`phys_timer`) |
| `tcp-test` | The network stack's socket client: `net-stack-test`'s PDs with the same grants - `make tools-test` checks the stack's differ only by what follows - and `socket_test` (role `test`, `nowayos-socket-test`), the client. Two `nowayos-ipc` rings of `nowayos-socket`'s records, each a 32 KiB messages region (`RING_REGION_LEN`) and a control page: requests (the client writes the messages, the stack the control) and reports (the stack writes the messages, the client the control); a channel each way; and the allow list, `tcp_allow`, prefilled from `tcp-allow-10.0.2.100-7.bin` (written by `tools/board-system`) and mapped read-only by the stack alone. The client holds nothing else and runs below the stack. `tcp-test.qemu` has QEMU's user-mode network run `cat` for each connection to 10.0.2.100 port 7, the echo service; the client's line comes back byte for byte, the connection closes cleanly, and 10.0.2.100 port 8 and 10.0.2.2 port 7 are refused, the stack saying why. QEMU only |
| `virtio-console-test` | The serial driver PD for QEMU's virtio console, both ways: `console` (role `driver`) maps one page of virtio-mmio transports (`virtio_mmio_24_31`) uncached, finds the console there on transport 31, sets up its receive and transmit queues in two 4 KiB DMA regions (`receive_queue`, `transmit_queue`, whose physical addresses it learns) and holds its interrupt; it passes bytes to and from `serial_test` (role `test`) through `nowayos-serial`'s two rings, each over two 4 KiB regions; one channel, on which each notifies the other. `virtio-console-test.qemu` joins the console to a UDP socket that sends to itself, so the client's two lines come back to it; the driver refuses its malformed record. A product system can run the same driver one way only. QEMU only |
| `net-isolation` | Negative tests at the client's side of the virtio-net driver: `net_isolation_parent` with two child probes, each mapping exactly what `net_client` maps in `net-test` (role `network`): `net_writes_driver_dma` writes where the driver maps its DMA memory and `driver_side`, a bystander, keeps its private page; `net_writes_rx_buffers` writes the receive buffers it maps read-only. `make tools-test` checks each probe maps what `net_client` maps there |
| `signature-test` | ECDSA P-256 checked in a PD: `signature_check` (role `test`), `nowayos-signature-check`, checks with `nowayos-signature` a signature OpenSSL made once (its private key discarded), then the same with one bit changed in the message, the signature and the public key, and a signature whose r is zero, and prints each result and how long each check took on the counter; the expect file checks the results and that the slowest check took under a second. No channels, memory regions, devices or children; no heap and no randomness in its tree |
| `update-test` | The update PD stages a signed release and refuses each bad bundle, on the real kernel: `update` (role `update`, `nowayos-update-pd`) holds no device, interrupt or physical address - its end of the bundle ring, the page it shares with the storage stand-in, and the trust anchors read-only (`test-trust-anchors.bin`, the run's root key, which the image is built with) - and calls `update_storage` (role `test`, `nowayos-update-storage`: two copies of the update state, copy A provisioned from `test-update-state.bin`, and two 4 KiB slots, each a region), which also says which slot booted (A) and how long a slot is. The ring holds 8,192 records, a whole release image's bundle (6 MiB in 2 MiB pages, as `update-trial`'s). `update_bundles` (role `test`, `nowayos-update-bundles`), in the TLS PD's place, reads the run's bundles from QEMU's fw_cfg (`update-test.qemu`: so keys made for each run are in no build) by DMA, as `dma-demo`, and offers them one at a time. `tools/update-bundles` makes the keys - deleted when it ends - and the bundles before each image; each bad bundle is refused for its reason with no slot marked, and the good one is written to slot B, checked and marked for one trial. `update-test.reproducible`: `qemu-test-all` also builds its image twice from the same inputs and compares them (`make image-reproducible`). QEMU only |
| `update-trial-r1`, `update-trial-r2` | 7 | The update tests across reboots: two releases of one system, which differ only in their release marker (`test-release-N.bin`), booted run after run by `tools/update-trial` (`make update-trial-test`; their `.harness` files name it, so `qemu-test-all` runs it once instead of booting them alone), which plays the bootloader between runs and checks each against `update-trial/RUN.expect`. `update_supervisor` (role `supervisor`, `smc`) is the parent of every other PD, so a fault in the trial release resets the board; QEMU runs with `-no-reboot`, so the reset ends the run (`qemu-test --ends-in-reset`). `update` as in `update-test`, its end of the bundle channel `pp`: on a trial boot it asks for the health verdict. `update_bundles` (priority 4, above the two it serves) is the only PD that maps fw_cfg (rule 7): the bundles, the health script it stands in for - `pass`, `fail`, `late` or `fault` - and the disk, the update state as the last run left it, which it copies into `disk` for `update_storage` (read-only there) before that starts; it maps the release marker read-only. `update_storage`'s slots are 4 MiB, a release image whole; it prints each copy of the state it writes in hex, and the SHA-256 of what each slot was written, which the harness carries into the next run. QEMU only |
| `budget-test` | Time isolation shown, not assumed: `looper` (role `test`, `nowayos-budget-probe`), above `heartbeat`, with a budget of 2 ms in every 10 ms, loops for ever once told after the heartbeat's third beat; `heartbeat` (role `test`, the same program) beats every 100 ms on `timer`'s timeouts (role `driver`, `nowayos-timer`, which it calls) and says whether each came on time - every one does. QEMU only |
| `budget-test-widened` | `budget-test` with the looper's budget widened to its period, and nothing else (`make tools-test` checks): the looper starves the heartbeat, which stops - its expect file requires the third beat and forbids a fourth. QEMU only |
| `smp-test` | Four cores, each PD pinned to one: Microkit's `cpu` attribute, in an SMP configuration only (`smp-test.configs`: `smp-debug`; `smp-test.qemu`: `-smp 4`). `timer` (role `driver`) and `smp_heartbeat` (role `test`, `nowayos-budget-probe`) on core 0; `spinner_1`, `spinner_2` and `spinner_3` (role `test`, the same program) on cores 1 to 3, above the heartbeat, each with a full budget. Each core in turn has the kernel list its threads and their core (`seL4_DebugDumpScheduler`, a debug kernel's); then the spinners spin for ever and every beat of the heartbeat stays on time - on one core any of them would starve it, as `budget-test-widened` shows. Its Pi 5 variant, `rpi5b_2gb/smp-test.system`, differs only in the timer (BCM2712's system timer, as `timer-demo`'s; `make tools-test` checks). QEMU and the Pi 5 (`make pi-run PI_SYSTEMS=smp-test MICROKIT_CONFIG=smp-debug`) |
| `getrandom-test` | Randomness in a PD: `getrandom_probe` (role `test`, `nowayos-getrandom-probe`) checks that every way into getrandom is refused before `nowayos-entropy`'s generator is seeded, writing nothing, draws from that one generator in turn once seeded with a fixed, public test seed, and is refused again once it is dropped. No device: the entropy PD that will seed real PDs is `entropy-test`'s. QEMU only |
| `entropy-test` | The entropy PD seeds one consumer: `entropy` (role `entropy`, `nowayos-entropy-source`) alone holds `rng` - QEMU's virtio-rng on transport 8, fed from the host's `/dev/urandom` (`entropy-test.qemu`) - its interrupt and its request queue's DMA region; it health-tests and conditions every byte (`nowayos-rng-health`), discards the first 1,024, fills `seed_consumer`'s seed ring before that PD runs, and overwrites each seed once taken. `seed_consumer` (role `test`, `nowayos-seed-consumer`) is refused before its first seed, keys its generator from one and wipes its copies, prints the SHA-256 of a draw, never the bytes, and checks the slot it took was overwritten. The seed ring is read-only in the consumer, its index read-only in the entropy PD; neither calls. QEMU only |
| `entropy-test-no-rng` | `entropy-test` with no random source: the same PDs and grants, but no virtio-rng on transport 8, so the entropy PD hands out nothing and the consumer stays not ready. QEMU only |
| `entropy-rate-test` | Each consumer held to its rate: the entropy PD with two seed rings, each refilled at no more than `nowayos-seed`'s `SEEDS_PER_SECOND` after its first fill, measured on the counter. `seed_flood` (role `test`) takes seeds as fast as it can for 2 s and gets no more than a ring's worth plus that rate, and at least half of it; `polite_consumer` (role `test`), a seed every 200 ms timed by the timer PD, finds one each time. QEMU only |
| `entropy-stuck-test` | A stuck source stops all seeding: virtio-rng fed from `/dev/zero`, so the entropy PD's repetition count test fails on its first bytes; it hands out no seed, reports the test (never the bytes) and faults on purpose; `entropy_supervisor`, every PD's parent, stops it, reports it and resets the board, and the second boot fails the same way. The stored counts, and `entropy` among the sensitive classes, are still to come. QEMU only |
| `tls-test` | TLS 1.3 inside one PD on a fixed heap arena: `tls_test` (role `test`, `nowayos-tls-test`) runs a rustls client and server on `nowayos-tls-crypto`, joined in memory (`nowayos-tls-loopback`); the server's Ed25519 raw public key is made at start from the PD's generator, seeded with a fixed, public test value, and the client pins it. With the plain Certificate message, then the largest rustls accepts, it prints the suite, the group and the arena's peak, and the expect file checks them; then the client refuses another pinned key. Its heap is `nowayos-heap`'s, `nowayos_tls_loopback::ARENA_BYTES` in its own image. No channels, memory regions, devices or children. QEMU only |
| `tls-test-small-arena` | The same handshake, the largest, on a 256 KiB arena, below its peak: `tls_test_small_arena` (role `test`, `nowayos-tls-test-small-arena`); the arena refuses a request part way through, the PD reports it, and the allocation error stops it - the Microkit monitor reports the fault; no handshake ever completes or returns. QEMU only |
| `usb-test` | The USB host PD: `usb0` and `usb1`, two `nowayos-usb-host`, each mapping one xHCI controller's window (`xhci0`, `xhci1`) uncached, a 76 KiB DMA region whose physical address it learns, and the board's facts about its controller read-only (`device-xhciN.bin`: on QEMU, where to place its BAR; on the Pi, the DMA offset); no interrupt. Each (role `input`) calls `timer` (role `driver`, the timer PD) for a tick every 8 ms, and sends its devices' canonical events through a ring of its own to `input_broker` (role `broker`; source 1 is `usb0`, 2 `usb1`), which sends what passes to `ui`, `nowayos-input-print` in the UI's place (role `ui`); the screen's size, 800x480, is read-only configuration in the USB host PDs and the broker. On QEMU each also maps its PCI function's configuration page (`xhci0_config`, `xhci1_config`) to place the controller, two `qemu-xhci` functions on bus 0 (`usb-test.qemu`), nothing attached. The Pi 5 builds its own variant, `rpi5b_2gb/usb-test.system`: RP1's two controllers' first pages, no configuration page, the timer on BCM2712's system timer. Each PD identifies, halts, resets and runs its controller, has a No Op command come back through DMA, then enumerates what is attached; the expect file checks only the controllers, as what is attached differs by board. QEMU and the Pi 5 |
| `pm-watchdog-test` | The Pi 5 only (`rpi5b_2gb/pm-watchdog-test.system`; QEMU has no PM watchdog): `pm_watchdog` (role `watchdog`, `nowayos-watchdog`) holds the PM block's page (`pm_watchdog`), uncached, and nothing else but its channels; at each round's end it arms BCM2712's PM watchdog for 6 s while `upper`, `looper` and `lower` (role `test`, heartbeat probes) answer their heartbeats; `ticker` (role `test`, the watchdog stand-in's ticker) ends each round every 2 s, timed by `timer` on the system timer. Ten rounds - 20 s, past the 15 s the PM watchdog can be armed for at once - then looper loops; at round 11 the watchdog names looper and lower and services no more, and the PM watchdog resets the board, which `make pi-run` sees boot again |
| `run-length-test` | The Pi 5 only (`rpi5b_2gb/run-length-test.system`): a test system that asks for a 60 s run - `test-run-length-60.bin`, read-only in `ticker` - and ends it by a reset into U-Boot's prompt. `run_watchdog` (role `watchdog`) holds the PM block's page and watches no PD: it arms the PM watchdog for 6 s at each of `ticker`'s 2 s ticks (timed by `timer`), for as long as they come; after 60 s the ticker stops, and the PM watchdog resets the board. `make pi-run` reads the run length from the description and waits for that reset. Another Pi test system asks for its run the same way |
| `gem-net-test` | The Pi 5 only (`rpi5b_2gb/gem-net-test.system`; QEMU has no GEM): `net-stack-test`'s stack and seed source, unchanged, over `gem_net` (role `driver`, `nowayos-gem-net`), the NIC driver PD for RP1's Cadence GEM MAC, polled on the timer (`nowayos-timer` on the system timer, as in `timer-demo`'s Pi variant). The driver holds RP1's GEM window (`rp1_eth`) and two pages of RP1's GPIO (`rp1_gpio_bank1`, `rp1_pads_bank1`) for the PHY's reset line - each uncached and read-write, and in no other PD - its rings' 4 KiB DMA region and a page for its start-up self-tests (one frame looped back inside the MAC, one through the PHY), each with its physical address, the stack's buffers by physical address only, the four queues and the board's facts about the MAC. The stack maps its packet buffers uncached here: RP1's DMA, across PCIe, does not see the CPU's caches. On the desk's LAN the stack takes a lease and answers ping; the link and lease are printed, not checked, as they differ from desk to desk. The run lasts 180 s (`test-run-length-180.bin`): `run_watchdog` (role `watchdog`) holds the PM block's page and keeps the PM watchdog armed while `ticker` (role `test`) ticks, then the board resets |
| `usb-audio-test` | Isochronous transfers: `usb0` (role `test`) runs `nowayos-usb-tone`, the USB host PD's test program, on `xhci0` with QEMU's USB audio device on its first root port (`usb-audio-test.qemu`; QEMU writes what it plays to `build/BOARD/usb-audio-test/speaker.wav`). It maps what `usb-test`'s `usb0` maps but a larger DMA region, 188 KiB (`DMA_SIZE_STREAMING`: four streams' rings and buffers, a large control buffer), and no ring to a broker and no screen; it calls `timer` for its tick. It sets up the speaker's isochronous OUT endpoint, selects its alternate setting and plays a 440 Hz tone, 48 ms of transfers queued ahead and topped up each tick; every 10 s it reports the stream's time and intervals missed, and any missed interval fails the test. `QEMU_SETTLE=600` runs it ten minutes more. On the Pi 5, `usb-stream-test`. QEMU only |
| `usb-stream-test` | The Pi 5 only (`rpi5b_2gb/usb-stream-test.system`): `nowayos-usb-tone` (role `test`) on both of RP1's controllers, each with a 188 KiB DMA region, streaming whatever audio interfaces are plugged in - a camera's microphone counted, a speaker's tone - for a 660 s run (`run_watchdog`, `ticker`, as `gem-net-test`); every 10 s each stream's intervals missed, and any missed interval fails the run |
| `usb-audio-loop-test` | USB audio: `usb0` (role `input`) runs the USB host PD itself, `nowayos-usb-host`, on `xhci0` with QEMU's USB audio device on its first root port (`usb-audio-loop-test.qemu`; QEMU writes what it plays to `build/BOARD/usb-audio-loop-test/speaker.wav`), with `usb-audio-test`'s DMA region and a client's two audio rings: from `usb0` (lossy; messages `usb0` `rw`, client `r`; control the other way) and to it (messages client `rw`, `usb0` `r`; control the other way), and a channel on which `usb0` notifies `client` (role `test`, `nowayos-audio-loop`, its 1 MiB recording region its own). `usb0` streams the speaker at the rate it takes nearest 48 kHz; the client sends it a 440 Hz tone; `make qemu-test` waits for 10 s sent and the speaker's first 20 s with no interval missed, and a missed interval, a stream not streamed or a speaker fed too little fails it |
| `usb-audio-loop-pi-test` | The Pi 5 only (`rpi5b_2gb/usb-audio-loop-pi-test.system`): `nowayos-usb-host` (role `input`) on both of RP1's controllers, each with `usb-audio-loop-test`'s DMA region, rings and channel to one client, `nowayos-audio-loop` (role `test`), for a 180 s run (`run_watchdog`, `ticker`): the first speaker and microphone plugged into any port - a tone, then 5 s recorded and played back, in turns - for a person to hear, the microphone capturing only while the client records; a missed interval, a stream not streamed or a speaker fed too little fails the run |
| `usb-camera-test` | The Pi 5 only (`rpi5b_2gb/usb-camera-test.system`; QEMU has no webcam): `usb0` and `usb1` (role `test`) run `nowayos-usb-camera` on RP1's two controllers, each with a 1204 KiB DMA region (`DMA_SIZE_CAMERA`: `usb-stream-test`'s and a pool of buffers for a camera's transfers) and a ring of frames to `camera_test` (role `test`, `nowayos-camera-test`): its 2 MiB frames region and its page read-write in the host PD and read-only in the client, the client's page the other way, and a channel the host notifies on. Whichever finds a webcam offers its size and rate on the ring and streams its Motion JPEG frames, whole, into it while the client wants them; the client checks the offer against its first frame, checks and counts the frames, after ten minutes dumps one on serial (`tools/serial-frame` saves it) and then wants no more, so the host's log shows the camera stopping. A 720 s run (`run_watchdog`, `ticker`, as `usb-stream-test`); a record or count the client refuses, a frame that is not a JPEG, or a camera not streamed fails it |
| `fan-test` | The Pi 5 only (`rpi5b_2gb/fan-test.system`): `fan` (`nowayos-fan`) runs the board's active cooler - full speed for 3 s, then the device tree's cooling curve on the SoC's temperature, asked of the firmware through the VideoCore mailbox from its own uncached buffer, a report every 10 s - timed by `timer` on the system timer. A run lasts `PI_WATCHDOG` seconds |
| `usb-input-test` | `usb-test` with QEMU's USB keyboard, mouse and tablet on the first controller's root ports: the same description and grants (`make tools-test` checks). `usb0` enumerates them - vendor, product, class, USB release and speed; the tablet's layout read from its report descriptor - and starts their reports; `usb-input-test.qmp` types, moves and clicks, and the UI's stand-in prints the canonical events that come through the broker; then it unplugs the keyboard with a key held - its release comes with it - and plugs it back, enumerated afresh, its keys arriving again; then unplugs it and plugs it straight back, with no wait between, and it is enumerated again. QEMU only |
| `uart-input-test` | A development route only: `uart_input` (role `input`, `nowayos-uart-input`) holds the board's debug UART - its window, uncached and read-only, and its facts, read-only; no interrupt - and, woken every 10 ms by `timer` (role `driver`, `nowayos-timer`, which it calls), sends each key typed on a terminal attached to the UART, decoded, to `input_broker` (role `broker`), which checks each event and sends it on to `ui` - `nowayos-input-print`, run in the UI's place (role `ui`). `uart-input-test.qmp`'s `serial` steps type printable characters, Enter, Backspace, Tab, Shift-Tab, the arrows, Escape and malformed sequences - into QEMU's serial port (`tools/qemu-test`), or through the Pi 5's Debug Probe (`tools/pi-run`) - and the expect file checks each event and each count. Its Pi 5 variant, `rpi5b_2gb/uart-input-test.system`, gives the timer BCM2712's system timer, as `timer-demo`'s does. `nowayos-uart-input.elf` is development-only: a product system may not run it |
| `pl011-demo` | the first driver PD, `pl011`: the board's debug UART's register window (`debug_uart`, a PL011 on both boards), uncached and read-write, bound through `nowayos-hw`, and the board's facts about it, `device-debug_uart.bin`, read-only (QEMU's `arm,pl011`, identified by its identification registers; the Pi 5's `arm,pl011-axi`, 0x200 bytes of registers and none to identify it by); no interrupt, channel or other memory. QEMU and the Pi 5 |

Conventions:

- `<program_image path="CRATE.elf"/>` names a binary crate in `../pds/`; the
  make targets build exactly the crates a system names. Two PDs may share an
  image (the isolation probes do, and pick their behaviour by PD name; the
  isolation parent picks its test the same way).
- `NAME.qemu`, optional, adds the QEMU devices the system's PDs drive
  (`-device ramfb`), one or more per line, `#` starting a comment. A system
  given a display device also has its screen checked: as soon as every
  expected line has appeared, `make qemu-test` takes a screendump over QMP
  and compares it, pixel for pixel, with `NAME.screen.png`; a screen that
  differs is written beside it as `NAME.screen.actual.png` (never
  committed), and `NOWAYOS_UPDATE_SNAPSHOTS=1 make qemu-test SYSTEM=NAME`
  rewrites the approved image - review it by eye before committing it.
- `NAME.qmp`, optional, drives QEMU over QMP while the system runs - to type
  keys, move and click the pointer (`input-send-event`): one step per line,
  `#` starting a comment (`../tools/qemu-test --qmp-script`): `wait TEXT`
  (until a serial line equal to TEXT appears), `send JSON` (a QMP command),
  `pause MS`, `tablet W H` then `click X Y` (a tablet click at pixel (X,
  Y)), `key QCODE...`, `hold QCODE` and `release QCODE` (QEMU's key names;
  each event paced at a person's speed), `screen PATH` (compare QEMU's
  screen with an approved image, relative to the script; never rewritten),
  and `serial TEXT` (TEXT's bytes written to the serial port as one
  burst, as a terminal sends a key - `\e` ESC, `\r` CR, `\xNN` any byte).
  A script of `wait`, `pause` and `serial` steps alone also plays on the Pi
  5: `make pi-run` writes its serial steps through the Debug Probe.
- `NAME.configs`, optional, lists the Microkit configurations `make
  qemu-test-all` boots the system in (`debug release`), `#` starting a
  comment; without it, `MICROKIT_CONFIG` (debug). A release kernel prints
  nothing, so such a system prints what its expect file checks through a
  console PD of its own.
- `NAME.baud`, optional, lists line rates `make qemu-test-all` boots the
  system at again, in each configuration, its serial taken no faster than a
  UART at that many baud (`make qemu-test QEMU_BAUD=115200`, `#` starting a
  comment). QEMU's own serial takes no time, which hides two PDs
  printing into each other's lines; at 115200 baud, the Pi 5's, a line
  takes up to 8 ms. `ipc-demo`, whose producer's budget can hold it
  mid-line, and `timer-demo`, whose clients are woken by the timer's
  interrupt, have one (`pds/README.md`, "When several PDs print").
- Configuration a PD reads, such as the screen's size, is a memory region
  prefilled from a file the system description names:
  `<memory_region name="screen" prefill_path="screen-WxH.bin" />`, mapped
  read-only as `nowayos_ro_region_7` (`nowayos_region::screen`). `make image`
  writes the file from its name (`../tools/board-system`), and the grants
  check keeps every map of such a region read-only (rule 12). A prefill whose
  name starts with `test-` is a test-only stand-in, refused outside a test
  system (rule 12): today a supervisor's counts until a store is built - `test-counts-R-S.bin` (restart count R, sensitive count S), or
  `-missing`, `-cut-short`, `-unknown-version` or `-bad-check.bin` -
  mapped read-only into the supervisor as `nowayos_ro_region_1`.
  A device's facts from the board file - its compatible string and how many
  bytes of its window are registers - are
  `<memory_region name="uart_facts" prefill_path="device-debug_uart.bin" />`,
  mapped read-only as `nowayos_ro_region_5` (`nowayos_region::device_facts`)
  into the driver that holds the device, and only into it (rule 12):
  the PL011 and console PDs identify the debug UART by them.
  The image's build time, the floor for wall-clock time, is
  `<memory_region name="build_time" prefill_path="build-time.bin" />`,
  mapped read-only as `nowayos_ro_region_6` (`nowayos_region::build_time`):
  `make image` writes it from `SOURCE_DATE_EPOCH` when that is set, and
  otherwise from the later of the commit times of the repo built and of
  NoWayOS - the same commit, the same bytes - and refuses a time later than
  its own clock, naming both, or 0, which is what an unwritten region holds.
  `clock-test` is the first system to map it; the grants check
  requires every `clock` PD to bind `nowayos_ro_region_6` to exactly one
  region, read-only and prefilled from `build-time.bin`
  (`clock-floor-from-build-time`).
  A Pi test system's run length is
  `<memory_region name="run_length" prefill_path="test-run-length-N.bin" />`,
  N seconds (1 to 86400), mapped read-only into its ticker as
  `nowayos_ro_region_0`: the ticker stops after N s, the watchdog PD then
  services the PM watchdog no more, and the board resets; `make pi-run`
  reads N from the description and waits for that reset.
  The destinations a network stack may connect its socket client to
   are
  `<memory_region name="tcp_allow" prefill_path="tcp-allow-10.0.2.100-7.bin" />`,
  more joined by `+` (`tcp-allow-10.0.2.100-7+10.0.2.101-22.bin`, up to 8),
  mapped read-only as `nowayos_ro_region_2` (`nowayos_socket::AllowList`):
  `make image` refuses a list the stack would refuse - an address in 0/8,
  127/8 or 224/3, port 0, a destination twice. `tcp-test` is the first
  system to map it.
- `NAME.expect` lists the serial lines `make qemu-test SYSTEM=NAME` must see,
  in order, as whole lines (`#` starts a comment). A line starting with `!` is
  forbidden output instead: if that text appears anywhere in a line, up to
  `QEMU_SETTLE` seconds (default 1) after the last expected line, the test
  fails at once. Every system needs an expect file: `make qemu-test-all`
  (part of `make container-ci`) boots every system. On a board, `make
  check-serial SYSTEM=NAME LOG=FILE` applies the same rules to a captured
  serial log, ignoring the boot output before the Microkit loader's first
  `LDR|` line and checking the whole rest of the log for forbidden output.
- `NAME.grants` names the kind of system (`system test|demo|product`) and
  each PD's role (`role PD ROLE`: ui, input, broker, display, driver,
  network, security, storage, update, app, supervisor, watchdog, clock or test), and lists every protection domain with its priority and CPU
  budget (and `passive`, for a PD that runs only on its callers' time), every
  map with its rights, every channel (with `notify=PD` when only one end can
  notify) and every parent-child relationship, every protected procedure call
  (`call CALLER CALLEE`), every PD that learns a region's physical address
  for DMA (`paddr PD REGION`), every PD given the SMC capability (`smc PD`),
  every device mapping (`device PD NAME r|rw`)
  and every interrupt (`irq PD NAME INTID`, or `irq PD NAME` when the device
  has one interrupt on the board, so the file serves every board) (format:
  `../tools/check-system`). Devices are named, never addressed: their register
  windows (none for a device reached through system registers, such as the
  CPU's timer), interrupts and triggers, each with its source, live in
  `../platforms/BOARD.toml`. A description maps a device through a
  `memory_region` with `phys_addr="@NAME" size="@NAME"`, mapped
  `cached="false"`, and takes its interrupt with `<irq irq="@NAME"
  trigger="@NAME" .../>` (`@NAME[N]` for the Nth of several); `make image`
  fills these in from the board file, and refuses a literal address or
  interrupt number, or a device the board does not have. A device with the
  same name on every board (`debug_uart`) keeps a system board-independent;
  one only QEMU has (`fw_cfg`, `phys_timer`, the virtio-mmio pages) keeps it
  QEMU only - unless a board has its own variant: `BOARD/NAME.system`, with
  its own `BOARD/NAME.grants`, which `make image BOARD=BOARD` builds and
  checks instead (by the same rules, devices still named). A variant keeps
  its system's protection domains and channels and differs only in the
  devices, their facts and the interrupts it grants (`../tools/test_tools.py`
  checks it); the Pi 5's `timer-demo`, `ui-demo` and
  `uart-input-test` are three.
  It is checked against `NAME.expect`, as QEMU is - or, where the board
  prints lines of its own, against `BOARD/NAME.expect`, which must hold
  every line of `NAME.expect` in its order, and every forbidden text, and
  adds the board's (`make check-serial BOARD=BOARD` and `make pi-run` take
  it; the Pi 5's `ui-demo`, its scan-out hashes). A system only one board
  can run has no `NAME.system` to vary: `BOARD/NAME.system` and its grants
  alone, its expect file `NAME.expect` (the Pi 5's `fb-test`). Before the PDs are
  built, `tools/check-system` fails if the description grants anything not
  listed, lists anything not granted, maps a region twice into one PD, maps
  anything executable, maps a device cached (but a framebuffer granted
  `cached`), into two PDs or as anything but
  its window, gives an interrupt to two PDs, lets a PD call one of equal or
  lower priority, gives a device's registers' physical address to a PD,
  maps an I/O region into the PD that learns its physical address
  (`nowayos_io_N_paddr`: memory a driver hands its device but never maps), or
  uses an element it does not model (virtual machines, SMC calls, extra CSpace
  capabilities...); after the image is made, it checks the page rights,
  device and DMA pages, interrupt handlers, notification and endpoint
  capabilities, passive PDs and every other capability in `capdl.json`
  against the same list. It also refuses the grants that must never exist,
  whatever a description says: the rules in `../tools/grant-rules.toml`, each
  with its reason - among them a `ui` PD with device memory, an interrupt, a
  child, a physical address or a channel to a `security` PD; an input
  `broker` with device memory, an interrupt or a physical address; a
  `network` PD with device memory, an interrupt, a physical address or a
  channel to a `ui` PD; an `app` PD with device memory, an interrupt or a
  physical address; a `watchdog` PD with a child, a call or a physical
  address, or memory in a demo or product; an `update` PD with device
  memory, an interrupt, a physical address or a channel to a `ui`, `input`,
  `broker` or `display` PD; in a product, a `ui`, `input`, `broker`,
  `display`, `network` or `app` PD with a channel to a `storage` PD; a
  remote-display `session` or `decoder` PD with device memory, an interrupt, a
  physical address or a child, a `decoder` with a channel to any PD but a
  `session`, `display` or `watchdog` PD or a call to any but a `session` or
  `display` PD (a rule's `unless_peer_role` may list several roles), and in
  a demo or product a `session`, `security` or `network` PD with a channel
  to a `display` PD; or a PD with the role `test`, or a test-only
  program image, outside a `test` system. An `entropy` PD has no channel to a `ui` PD and makes no call, and what it maps
  read-write and gives no device - a seed ring's messages - is mapped by one other PD, read-only
  (`seed-ring-one-reader`). A `clock` PD holds no device memory, interrupt or physical
  address, reads its floor from read-only region 6 bound to the region
  prefilled from `build-time.bin` (`clock-floor-from-build-time`), and no
  PD but a `network` PD - or a `test` PD in a test system -
  may write a region it maps, by a read-write map or by learning its
  physical address (`clock-samples-only-from-network`). Some rules
  bind a system's shape: in a product, exactly one
  `supervisor`, every other PD its child; in a demo or product, the
  supervisor above every other PD in priority, and a `watchdog` above every
  PD but the supervisor. So widening a grant takes an edit to `NAME.grants`
  in the same change — a security change to call out in the pull request.
- Shared memory is bound through `nowayos-region` (`../pds/region`): a map's
  `setvar_vaddr` / `setvar_size` name a symbol pair, and **the symbol name
  states the rights the map must grant** — `nowayos_rw_region_N_*` only on
  `perms="rw"`, `nowayos_ro_region_N_*` only on `perms="r"`. The PD cannot
  check this itself; `tools/check-system` does.
- Descriptions are board-independent: the board is chosen at build time
  (`BOARD=qemu_virt_aarch64` by default, `rpi5b_2gb` for the Pi 5), and no
  driver or application code tests for a board (`make tools-test` checks).
  A new board needs a board file, not a new description.
- A change here that adds or widens a capability (device, memory region,
  map rights, channel, call, physical address, child, priority) is a
  security change: call it out in
  the pull request.
- Output order in each system with more than one PD follows from its
  priorities on one core; changing a priority can reorder the expected lines.

```sh
make qemu SYSTEM=ipc-demo             # boot interactively; quit with Ctrl-A, then x
make qemu-test SYSTEM=isolation-test  # headless boot, checks isolation-test.expect
make qemu-test-all                    # every system
make check-serial SYSTEM=hello LOG=build/rpi5b_2gb/hello/serial.log  # a board's captured log
make sdk-boards                       # boards and configurations in the pinned SDK
```

To see the kernel-level rights a description grants, read
`build/BOARD/NAME/capdl.json` after `make image SYSTEM=NAME`: each mapped
frame shows `"rights": {"read": ..., "write": ...}` and `"executable"`.

## The grants rules in brief

What the grants check enforces, in one place; `tools/grant-rules.toml` holds each rule with its reason.

- System descriptions (`systems/*.system`) grant the narrowest rights, and every
  grant is listed in the matching `systems/*.grants`, which `make image` /
  `make qemu-test` check against the description and the Microkit tool's capDL
  output. Devices are granted by name (`device PD NAME r|rw`, `irq PD NAME
  [INTID]`); the facts - each device's register window, interrupts and
  trigger - come from `platforms/BOARD.toml`, each with its source (add a
  device there only with one). A description names devices, never addresses
  (`phys_addr="@NAME"`, `irq="@NAME"`): `make image` writes each board's
  description with that board's facts (`tools/board-system`), so one
  source serves QEMU and the Pi 5, and no code may test for a board. A
  driver that needs to know which device it holds, or how many bytes of
  its page are registers (the Pi 5's PL011: 0x200, no identification
  registers), maps the board's facts about it - a region prefilled from
  `device-NAME.bin`, read-only region 5 (`nowayos_region::device_facts`) -
  which only the PD holding NAME's window may map (rule 12). A
  system that needs a device only some boards have may keep a board's own
  variant, `systems/BOARD/NAME.system` with its `.grants` (the Pi 5's
  `timer-demo`): it differs only in devices and their facts and shares
  `NAME.expect`, to which the board may add lines of its own in
  `systems/BOARD/NAME.expect`, holding every line of it (the Pi 5's `ui-demo`, which
  shows each frame on the HDMI monitor and prints its scan-out hash); a
  system only one board can run has no `NAME.system` to vary (`fb-test`). Calls (`call CALLER CALLEE`,
  only to a higher priority), one-way channels (`notify=PD`), passive PDs and
  every PD that learns a DMA region's physical address (`paddr PD REGION`)
  are listed too, and every PD given the SMC capability (`smc PD`: only a
  `supervisor`, and in no product system until it can be badged to PSCI
  `SYSTEM_RESET`), and every parent that can restart a child on its own
  (`restart PARENT CHILD`: it maps the child's restart image, made by `make
  image` from a `prefill_path="restart-CHILD.bin"` region; no product system
  may have one yet); a
  region prefilled from a `test-` file - a supervisor's stand-in counts,
  `test-counts-*.bin` - is refused outside a test system (rule 12).
  An image's build time - the floor for wall-clock time -
  is a region prefilled from `build-time.bin`, mapped read-only
  (`nowayos_region::build_time`): `make image` writes it from
  `SOURCE_DATE_EPOCH`, or else the later of the repo's and NoWayOS's commit
  times, and refuses one later than its own clock, or 0. The clock PD
  (role `clock`; `clock-test`) maps it and holds no device memory,
  interrupt or physical address; the grants check requires a `clock` PD to
  bind read-only region 6 to exactly one region, read-only and prefilled
  from `build-time.bin` (`clock-floor-from-build-time`, a `reads_prefill`
  rule), and `nowayos_region::build_time` refuses 0. It takes samples only
  on the rings its system grants, and answers readings by protected procedure call. No PD but
  a `network` PD - or a `test` PD in a test system - may write a region a
  clock maps, by a read-write map or by learning its physical address
  (`clock-samples-only-from-network`, an `only_writers` rule).
  Every `.grants` file names its system's kind (`system
  test|demo|product`) and each PD's role (`role PD ROLE`); development-only
  programs - the debug UART's keys (`nowayos-uart-input`), the
  scripted input PD, and a program with a stand-in for a service built in
  (`*-stand-in.elf`) - are listed in `[development_only]` as name
  patterns (`*-uart-input.elf`: a product's own program wrapping one keeps
  the ending), refused in a product system, and `make image` names each PD that
  runs one; and, whatever its program, an input PD maps the debug UART
  read-only (`input-reads-the-debug-uart-only`) and holds it in no product
  (`debug-uart-no-input-in-product`) - forbid rules may name one device and
  its rights; the grants that
  must never exist for a role are in `tools/grant-rules.toml`, each with its
  reason, and so are the rules on a system's shape: a product has
  exactly one `supervisor`, every other PD its child, and in a demo or
  product the supervisor runs above every PD and a `watchdog` above every PD
  but the supervisor; a DMA region binds `nowayos_dma_N_vaddr`/`_size`/`_paddr`,
  all naming one region, and devices get only addresses inside it; an I/O
  region - memory a driver hands its device but never maps, such as a
  network client's packet buffers - binds `nowayos_io_N_paddr` alone, and
  the PD that learns it may not map it. A board's firmware framebuffer (the Pi 5's
  `firmware_fb`, which the monitor shows) is mapped only by a `display` PD
  (`firmware-framebuffer-only-display`), which alone may map it cached
  (`cached PD DEVICE`, `cached-framebuffer-only-display`) and cleans
  every line it writes to memory before it acknowledges the frame, and `make image` refuses an image whose
  Microkit report places any kernel object in it but its window's frames, or whose
  Microkit SDK gives seL4 RAM over it (its `platform_gen.json`; rule 14). The Pi 5's PM block (`pm_watchdog`: its watchdog, and the SoC's power
  domain and reset controls) is mapped only by a `watchdog` PD
  (`pm-watchdog-only-watchdog`), and the device a board names `rng` - QEMU's
  virtio-rng page - only by an `entropy` PD (`rng-only-entropy`); and a system's QEMU device file puts virtio-rng on transport 8
  alone, the first of that page (`bus=virtio-mmio-bus.8`; rule 15). A PD's heap in a memory region (nowayos-heap's `nowayos_heap_region_vaddr` and `_size`) is mapped read-write into that PD alone, not prefilled, one to a PD (rule 16). Widening a grant is a security change: call it out.

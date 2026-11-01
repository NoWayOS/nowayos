# SPDX-License-Identifier: MIT OR Apache-2.0
# system.mk: NoWayOS's dev-container, Microkit system-image and QEMU targets.
#
# Included by nowayos's root Makefile (-include tools/make/system.mk). It also
# works on its own: make -f tools/make/system.mk qemu-test. A product repo
# includes it to build its own systems and PDs (see "Product repos" below).
#
# On the host, these targets run themselves inside the dev container (see
# container.mk); inside the container (NOWAYOS_CONTAINER=1, set by the
# image) the recipes run directly. `container` runs on the host only.
#
#   make container        build the dev container image (IMAGE, nowayos-dev:0.1),
#                         labelled with the Containerfile's sha256 and checked
#                         against the reviewed Debian package list
#                         (UPDATE_DEBIAN_LOCK=1 rewrites that list instead;
#                         NO_CACHE=1 rebuilds every layer, so the Debian
#                         packages come from today's archive, not the cache)
#   make shell            interactive shell in the container, workspace at /work
#   make qemu             build SYSTEM and boot it in QEMU (quit: Ctrl-A, then x);
#                         VNC=1 also shows QEMU's screen over VNC on the host's
#                         localhost:VNC_PORT (5900), with a password made for
#                         that run only, and takes the keyboard and pointer
#                         from the VNC client
#   make qemu-test        build SYSTEM, boot it headless, check systems/SYSTEM.expect
#   make qemu-test-all    qemu-test for every systems/*.system, in each
#                         configuration its SYSTEM.configs lists (default: one),
#                         and again at each line rate its SYSTEM.baud lists
#                         (QEMU_BAUD); a system with a SYSTEM.harness
#                         file is booted by the make target that file names
#                         instead, once (update-trial-test)
#   make update-trial-test the update tests across reboots (tools/update-trial):
#                         two releases built with the run's keys, a
#                         trial boot marked good, or failed and fallen back
#   make check-serial     check a serial log captured from a board, LOG=FILE,
#                         against systems/SYSTEM.expect by qemu-test's rules
#                         (tools/check-serial); LOG must lie in the workspace.
#                         With BOARD=rpi5b_2gb, systems/rpi5b_2gb/SYSTEM.expect
#                         where there is one: the board's own lines added
#   make pi-run           on the host, with the Pi 5's Debug Probe plugged in:
#                         build PI_SYSTEMS' images for rpi5b_2gb in
#                         the container, then run each on the board over its
#                         debug UART, unattended (tools/pi-run): loady, unzip
#                         and crc32 at U-Boot's prompt, the PM watchdog armed
#                         for PI_WATCHDOG seconds, go; each log followed live
#                         and checked by check-serial, the board left at
#                         U-Boot's prompt; a system's systems/SYSTEM.qmp of
#                         wait, pause and serial steps is played through the
#                         probe, as typed on a terminal. Logs in PI_LOGS
#   make pi-uboot         on the host: run PI_COMMANDS - U-Boot commands that
#                         read, separated by ';' - at the parked board's
#                         prompt (tools/pi-run uboot), logged to
#                         build/rpi5b_2gb/pi-run/uboot.log
#   make pi-show          on the host: build SYSTEM for rpi5b_2gb and run it
#                         with no watchdog, for a person to watch (tools/pi-run
#                         show): it runs until someone power-cycles the board;
#                         if that is within PI_HOLD seconds the board is parked
#                         at U-Boot's prompt and the log checked, and if not the
#                         image is left running and the run says how to park.
#                         PI_TYPE=1: for a person to drive - what is typed on
#                         this terminal goes to the board's debug UART, and the
#                         image's lines are printed (Ctrl-] or Ctrl-C stops)
#                         it. Logs in PI_LOGS
#   make container-ci     the full CI in the container: make secrets lint test doc demo
#                         nostd deny atlases-check sbom fuzz kani, then tools-test, sel4-config-check, unsafe-audit,
#                         pds-lint, pds-deny, pds-test, pds-miri, qemu-test-all,
#                         std-crates-check and screen-compiler-check
#   make release-evidence the release evidence pack for the commit checked out
#                         (tools/release-evidence): refuses uncommitted
#                         changes, then runs container-ci step by step and builds
#                         every image again - for QEMU and for EVIDENCE_PI_BOARD
#                         (rpi5b_2gb) where its facts serve - into
#                         build/release-evidence/COMMIT/ with INDEX.md: the ci
#                         summary, pinned versions, SBOMs, image hashes and
#                         configuration records, a grants inventory, the
#                         cryptographic crates and the Per Release checklist
#   make screen-compiler-check  Screen Compiler's generated screens built against this
#                         nowayos-ui and drawn as approved: when
#                         a screen-compiler clone sits beside this nowayos clone;
#                         skipped with a notice when not
#   make pds              build SYSTEM's protection domains only
#   make image            build build/BOARD/SYSTEM/loader.img (+ report.txt,
#                         capdl.json) only; writes the description's prefill
#                         files - the screen's size, and the image's build time
#                         (build-time.bin: SOURCE_DATE_EPOCH, or else the later
#                         of the repo's and NoWayOS's commit times, refused if
#                         later than this machine's clock); checks
#                         SYSTEM's grants, and records
#                         the kernel's seL4 configuration beside the image
#                         (sel4-config.txt: the SDK, board, configuration and
#                         proof-relevant kernel options, labelled) and
#                         what the image is made of (sbom.cdx.json, below).
#                         Puts the glyph atlases' notice, OFL.txt, beside every
#                         image.
#                         Writes the restart image of each PD the system can
#                         restart before the Microkit tool runs, and checks
#                         them against its capDL output after
#   make image-sbom       make image, then name its SBOM: a CycloneDX 1.5 list of
#                         the kernel, loader, monitor and capDL initialiser from
#                         the SDK, every program image and each crate compiled
#                         into it (cargo's unit graph for the same build), the
#                         system description - each with its sha256 - and, for
#                         rpi5b_2gb, U-Boot, the firmware and config.txt; GPL
#                         parts marked as needing a source offer, and the
#                         crates the capDL initialiser's audit found inside it
#                         (tools/image-sbom, tools/image-components.toml,
#                         tools/initialiser-audit.toml). make
#                         sdcard adds u-boot.bin's sha256
#   make sel4-config-check  the recorded verification position
#                         (tools/sel4-verification.toml) is for the Microkit
#                         SDK the Containerfile pins: fails after an SDK change
#                         until the position is re-checked
#   make pds-lint         rustfmt --check and clippy for the pds workspace,
#                         each of PDS_LINT_ALONE linted on its own
#   make pds-deny         cargo-deny (licences, sources, advisories) for pds,
#                         then its git sources only at the commits NoWayOS's
#                         pds/Cargo.toml pins (tools/check-git-pins)
#   make pds-test         tests and clippy (all targets) for the pds library
#                         crates, on the container's own target
#   make pds-miri         Miri over the approved-unsafe pds crates
#                         (tools/unsafe-allowlist.toml, miri = true)
#   make kani a product repo's Kani proofs: src/proofs.rs in
#                         each crate of its KANI_CRATES, then the mutants of its
#                         KANI_MUTATIONS_FILE (tools/kani-mutation), each of which
#                         must fail; with no crate named, nothing to prove. NoWayOS's
#                         own make kani is its root Makefile's
#   make fuzz a product repo's fuzz targets: each in its
#                         fuzz/fuzz_targets/ run for FUZZ_SECONDS, as NoWayOS's
#                         are (fuzz-run below); with no fuzz/, nothing to fuzz.
#                         FUZZ_TARGETS=NAME for one; FUZZ_INPUT=FILE runs one on
#                         one input. NoWayOS's own make fuzz is its root
#                         Makefile's, which runs the same fuzz-run
#   make unsafe-audit     unsafe code only in the approved crates (tools/check-unsafe)
#   make forget-verdicts  delete the cached verdicts of Kani, Miri and fuzzing
#                         (CI_VERDICTS), so make ci runs them all again
#   make std-crates-check the standard-library crates built into the PDs
#                         match pds/std-crates.txt (after qemu-test-all), and
#                         getrandom's back end is defined in no library they
#                         link but nowayos-entropy's (tools/check-backends)
#   make tools-test       tests for the tools in tools/
#   make uboot            build U-Boot for the Pi 5 from the release pinned in
#                         the dev image: build/rpi5b_2gb/u-boot/u-boot.bin,
#                         reproducibly (the same image gives the same bytes)
#   make sdcard           BOARD=rpi5b_2gb: the Pi 5 SD card's boot-partition
#                         files for SYSTEM - the seL4 Pi 5 guide's list, U-Boot,
#                         config.txt, loader.img, the glyph atlases' notice
#                         (OFL.txt) and SHA256SUMS - in
#                         build/rpi5b_2gb/SYSTEM/sdcard/, reproducibly
#   make sdk-boards       list the boards and configs in the pinned Microkit SDK
#   make system-clean     remove build/, the PD target directory and U-Boot's tree
#
# Every system description, systems/SYSTEM.system, has two companions:
# SYSTEM.expect (the serial output qemu-test checks) and SYSTEM.grants
# (everything it grants; tools/check-system compares it with the system
# description before the PDs are built and with the Microkit tool's capDL
# output after the image is made). A board that needs its own variant of a
# system - a device only it has - builds systems/BOARD/SYSTEM.system and
# checks it against systems/BOARD/SYSTEM.grants instead. Its log is
# checked against SYSTEM.expect, or against systems/BOARD/SYSTEM.expect
# where the board adds lines of its own, which must hold every line of
# SYSTEM.expect. A system only one board runs has
# systems/BOARD/SYSTEM.system alone (the Pi 5's fb-test).
#
# Product repos. A repo beside nowayos/, in the same workspace folder, that
# keeps its own systems/ and pds/ builds and checks them with these targets -
# pds, image, qemu, qemu-test, qemu-test-all, check-serial, pds-lint,
# pds-deny, pds-test, pds-miri, unsafe-audit, std-crates-check, sdcard - on
# NoWayOS's dev container, pinned nightly and Microkit SDK, with NoWayOS's
# board facts (platforms/), grant rules and checks, exactly as NoWayOS's own
# systems are. Its Makefile sets, before it includes this file:
#
#   SYSTEM_REPO := $(CURDIR)     the repo whose systems/ and pds/ are built
#   SYSTEM ?= NAME               its default system
#   PDS_LIB_CRATES := CRATES     its pds crates that need no seL4 (pds-test);
#                                empty: none
#   PDS_LINT_ALONE := CRATES     its PDs with a heap, which pds-lint lints on
#                                their own; empty: none
#   UNSAFE_ALLOWLIST := FILE     its approved-unsafe list, in tools/check-unsafe's
#                                format; empty (the default for a product): no
#                                unsafe code anywhere in the repo
#   SYSTEM_FORWARD = VAR=value   its own settings, passed into the container
#                                with each target (optional)
#
#   -include ../nowayos/tools/make/container.mk
#   -include ../nowayos/tools/make/system.mk
#
# SYSTEMS_DIR, PDS_DIR and BUILD_ROOT then follow SYSTEM_REPO (systems/, pds/,
# build/), and the std-crates check uses NoWayOS's pds/std-crates.txt
# (PDS_STD_CRATES). PDS_TOOLCHAIN names NoWayOS's pinned PD nightly (read it;
# never set it). Its pds/ is a Cargo workspace laid out like NoWayOS's: a
# rust-toolchain.toml pinning NoWayOS's nightly (the targets refuse another),
# a .cargo/config.toml building for aarch64-sel4 with build-std, with the
# target spec from nowayos/pds/support/targets, and git sources only at the
# commits NoWayOS's pds/Cargo.toml pins (pds-deny). From the host, each target
# runs the product's own Makefile in the container, in the product's folder.
# container-ci, tools-test, sel4-config-check, screen-compiler-check and uboot are
# NoWayOS's own.
#
# Variables:
#   BOARD ?= qemu_virt_aarch64   Microkit board (QEMU targets need a QEMU board)
#   MICROKIT_CONFIG ?= debug     debug | release | benchmark (debug has the console)
#   SYSTEM ?= hello              systems/$(SYSTEM).system (+ .expect, .grants)
#   QEMU_TIMEOUT ?= 60           seconds qemu-test waits for the expected lines
#   QEMU_SETTLE ?= 1             seconds qemu-test keeps watching for forbidden
#                                output after the last expected line
#   QEMU_BAUD                    qemu-test takes QEMU's serial no faster than a
#                                UART at this many baud (115200: the Pi 5's), so
#                                a line takes as long as on a board;
#                                empty: QEMU's own serial, which takes no time
#   LOG                          check-serial's captured log (relative to the
#                                repo, or absolute, inside the workspace)
#   PI_DEVICE                    pi-run's and pi-uboot's serial device, the
#                                Debug Probe (/dev/cu.usbmodemXXXX on a Mac)
#   PI_SYSTEMS ?= $(SYSTEM)      the systems pi-run runs, in order
#   PI_WATCHDOG ?= 12            seconds each boot runs before the PM
#                                watchdog resets the board (1 to 15)
#   PI_LOGS                      pi-run's new log folder (default:
#                                build/rpi5b_2gb/pi-run/DATE-TIME)
#   PI_COMMANDS ?= version       pi-uboot's commands
#   PI_HOLD ?= 14400             the longest pi-show waits for the power cycle
#   PI_TYPE                      1: pi-show passes what is typed to the board
#   PI_BOARD_LOCK (environment)  the lock file every run on the board takes
#                                (default: one in the temporary directory)
#   SYSTEM_REPO, SYSTEMS_DIR, PDS_DIR, BUILD_ROOT, PDS_TARGET_DIR,
#   PDS_LIB_CRATES, PDS_STD_CRATES, PDS_TOOLCHAIN, UNSAFE_ALLOWLIST,
#   SYSTEM_FORWARD: see "Product repos"
#   EVIDENCE_PI_BOARD ?= rpi5b_2gb  release-evidence's second board (empty: none)
#   SOURCE_DATE_EPOCH            the build time an image records, in seconds
#                                since 1970 (unset: the commit time)
#
# Written for GNU Make 3.81 (the version macOS ships) and later.

ifndef NOWAYOS_SYSTEM_MK
NOWAYOS_SYSTEM_MK := 1

_ts_system_mk := $(abspath $(lastword $(MAKEFILE_LIST)))
_ts_make_dir := $(dir $(_ts_system_mk))
_ts_nowayos := $(abspath $(_ts_make_dir)../..)
# The makefile make was started with (this file, nowayos's Makefile or a
# product's): qemu-test-all runs each system through it.
_ts_main_mk := $(abspath $(firstword $(MAKEFILE_LIST)))

# Do not take over the including Makefile's default goal (its `help`).
_ts_saved_goal := $(.DEFAULT_GOAL)

ifndef NOWAYOS_CONTAINER_MK
-include $(_ts_make_dir)container.mk
endif

BOARD ?= qemu_virt_aarch64
MICROKIT_CONFIG ?= debug
SYSTEM ?= hello
QEMU_TIMEOUT ?= 60
# Each update-trial run's limit: the staging run takes a whole
# release image through the update PD.
UPDATE_TRIAL_TIMEOUT ?= 120
QEMU_SETTLE ?= 1
QEMU_BAUD ?=

MICROKIT_SDK ?= /opt/microkit/sdk
PYTHON ?= python3
QEMU ?= qemu-system-aarch64
PDS_CARGO ?= cargo
# The repo whose systems/ and pds/ are built: NoWayOS, or a product repo
# beside it ("Product repos" above).
SYSTEM_REPO ?= $(_ts_nowayos)
_ts_repo_dir = $(abspath $(SYSTEM_REPO))
_ts_is_nowayos = $(filter $(_ts_nowayos),$(_ts_repo_dir))
SYSTEMS_DIR ?= $(_ts_repo_dir)/systems
PDS_DIR ?= $(_ts_repo_dir)/pds
BUILD_ROOT ?= $(_ts_repo_dir)/build
# Inside the container CARGO_TARGET_DIR points into a named volume.
PDS_TARGET_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR)-pds,$(PDS_DIR)/target)
# Every repo's PDs are built with NoWayOS's toolchain, so with the
# standard-library crates NoWayOS's reviewed list names.
PDS_STD_CRATES ?= $(_ts_nowayos)/pds/std-crates.txt
# The approved-unsafe list: NoWayOS's own; none for a product unless it sets one.
UNSAFE_ALLOWLIST ?= $(if $(_ts_is_nowayos),$(_ts_nowayos)/tools/unsafe-allowlist.toml)
_ts_unsafe_args = --root $(_ts_repo_dir) $(if $(UNSAFE_ALLOWLIST),--allowlist $(abspath $(UNSAFE_ALLOWLIST)),--no-allowlist)

# QEMU command for qemu_virt_aarch64, as given in the Microkit 2.3.1 manual,
# plus `-nic none`: QEMU's default network card is unused (no networking
# here) and would need an iPXE option ROM that the container leaves out.
QEMU_MACHINE ?= virt,virtualization=on
QEMU_CPU ?= cortex-a53
QEMU_MEMORY ?= 2G
QEMU_NET ?= -nic none
QEMU_EXTRA_ARGS ?=
# Where QEMU's loader puts the image, as the Microkit manual gives it (a
# product's own QEMU runs use it too).
QEMU_LOADER_ADDR ?= 0x70000000
# make qemu VNC=1: QEMU's screen over VNC, published on the host's
# localhost:VNC_PORT only.
VNC ?=
VNC_PORT ?= 5900

_ts_systems = $(sort $(basename $(notdir $(wildcard $(SYSTEMS_DIR)/*.system))))
# systems/SYSTEM.system names its devices; tools/board-system writes the
# board's description, with that board's facts, beside the image.
# A system whose PDs need a device only some boards have keeps a board's own
# variant, systems/BOARD/SYSTEM.system with its SYSTEM.grants, which that
# board builds instead (the Pi 5's timer-demo); it still names its
# devices, and a board's log is checked against QEMU's expect file - or
# against systems/BOARD/SYSTEM.expect, where the board adds lines of its own
# to every line of QEMU's (below).
_ts_variant = $(wildcard $(SYSTEMS_DIR)/$(BOARD)/$(SYSTEM).system)
_ts_system_dir = $(if $(_ts_variant),$(SYSTEMS_DIR)/$(BOARD),$(SYSTEMS_DIR))
_ts_src = $(_ts_system_dir)/$(SYSTEM).system
_ts_sdf = $(_ts_out)/$(SYSTEM).system
# A board may add lines of its own to a system's expect file - what only it
# prints, such as the Pi 5's scan-out hashes: systems/BOARD/SYSTEM.expect,
# which holds every line of SYSTEM.expect too (tools/test_tools.py checks).
_ts_expect = $(or $(wildcard $(SYSTEMS_DIR)/$(BOARD)/$(SYSTEM).expect),$(SYSTEMS_DIR)/$(SYSTEM).expect)
_ts_grants = $(_ts_system_dir)/$(SYSTEM).grants
_ts_out = $(BUILD_ROOT)/$(BOARD)/$(SYSTEM)
# A test system whose description prefills the update tests' trust anchors
# or state: tools/update-bundles makes fresh keys and bundles for it
# before each image is built - its bundles reach QEMU through the system's
# .qemu file - unless UPDATE_BUNDLES_KEEP=1 keeps the ones in
# UPDATE_BUNDLES_OUT, as two builds that must match do.
UPDATE_BUNDLES_OUT ?= $(BUILD_ROOT)/update-bundles
UPDATE_BUNDLES_KEEP ?=
_ts_update_prefills = $(shell grep -lE 'prefill_path="test-(trust-anchors|update-state)\.bin"' $(_ts_src) 2>/dev/null)
_ts_loader = $(_ts_out)/loader.img
_ts_sdk_board = $(MICROKIT_SDK)/board/$(BOARD)/$(MICROKIT_CONFIG)
# One target directory per board and config: sel4-sys is generated from the
# SDK's headers for that board/config.
_ts_pd_target = $(PDS_TARGET_DIR)/$(BOARD)-$(MICROKIT_CONFIG)
_ts_pd_elf_dir = $(_ts_pd_target)/aarch64-sel4/release

# Library crates in pds/ that need no seL4: pds-test runs their tests on the
# container's own target, and pds-miri runs the approved-unsafe ones under
# Miri. Both use the pds workspace's pinned nightly, from the repo's root
# rather than pds/, so that pds/.cargo/config.toml (the seL4 target,
# build-std) does not apply. Miri's sysroot is cached in the target volume.
PDS_LIB_CRATES ?= $(if $(_ts_is_nowayos),nowayos-region nowayos-fault nowayos-ipc-demo nowayos-hw nowayos-timer-api nowayos-clock-api nowayos-ui-loop nowayos-restart nowayos-fb nowayos-entropy)
# Empty: the pds crates marked `miri = true` in tools/unsafe-allowlist.toml.
PDS_MIRI_CRATES ?=
# PDs with a heap: pds-lint lints each on its own, never with the
# rest of the workspace, where cargo would unify their dependencies'
# features into every PD's - rustls's zeroize with `alloc` among them, which
# makes a PD with no global allocator, by design, fail to link. Each
# system's image is built from its own PDs alone, so only the lint meets it.
PDS_LINT_ALONE ?= $(if $(_ts_is_nowayos),nowayos-tls-test nowayos-tls-test-small-arena)

# A product repo's Kani proofs: the crates of its root workspace
# with proofs in src/proofs.rs (compiled only under cfg(kani)), and its own
# mutations - a JSON file tools/kani-mutation reads - whose proofs must fail.
# Logs in KANI_OUT. NoWayOS's own are its root Makefile's KANI_CRATES.
KANI_CRATES ?=
KANI_MUTATIONS_FILE ?=
KANI_OUT ?= $(BUILD_ROOT)/kani
# Harnesses proved at once: one CBMC per KANI_JOB_GIB of the dev
# container's memory, less one for the mutants that run beside them. Two at
# once ran out of memory in a 24 GiB VM; the VM has 60 GiB (Development
# Workflow). KANI_JOBS=1 proves one at a time.
KANI_JOB_GIB ?= 10
KANI_JOBS ?= $(shell awk -v g=$(KANI_JOB_GIB) '/^MemTotal:/ { n = int($$2 / (g * 1048576)) - 1; print (n < 1 ? 1 : n) }' /proc/meminfo 2>/dev/null || echo 1)
# Kani takes --jobs only with its terse output, whose lines are each
# harness's own, prefixed "Thread N: ", and its summary.
_ts_kani_jobs = $(if $(filter-out 1,$(KANI_JOBS)),--jobs $(KANI_JOBS) --output-format=terse)
# make fuzz (NoWayOS's, or a product repo's) runs each target in
# fuzz/fuzz_targets/ for FUZZ_SECONDS seconds - in make ci too - keeping its
# corpus and any crash in FUZZ_OUT. fuzz/ is a Cargo workspace of the repo's
# own, with its own lock: libFuzzer is signed off for development and CI
# fuzzing only, so nothing that ships may use it.
FUZZ_SECONDS ?= 10
FUZZ_TARGETS ?= $(sort $(basename $(notdir $(wildcard fuzz/fuzz_targets/*.rs))))
FUZZ_OUT ?= build/fuzz
FUZZ_INPUT ?=
# The instrumentation cargo-fuzz 0.13 asks rustc for when it runs with no
# sanitizer: libFuzzer's coverage counters, comparison tracing and stack
# depth, the cfg `fuzzing`, every function kept, debug assertions and one
# codegen unit. Our fuzzed crates forbid unsafe code; the third-party code
# some of NoWayOS's targets reach has some (heapless under smoltcp in
# net_stack_receive; p256, sha2 and their tree in signature_check) and runs
# here without a sanitizer, so a memory error there that does not panic goes
# unseen (Known Issues). Hence the pinned stable toolchain, not a
# nightly.
FUZZ_RUSTFLAGS := -Cpasses=sancov-module -Cllvm-args=-sanitizer-coverage-level=4 \
	-Cllvm-args=-sanitizer-coverage-inline-8bit-counters -Cllvm-args=-sanitizer-coverage-pc-table \
	-Cllvm-args=-sanitizer-coverage-trace-compares -Cllvm-args=-sanitizer-coverage-stack-depth \
	--cfg fuzzing -Clink-dead-code -Cdebug-assertions -Ccodegen-units=1
FUZZ_TARGET_DIR = $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR)-fuzz,$(CURDIR)/fuzz/target)
# How many fuzz targets run side by side, each on one core.
FUZZ_JOBS ?= $(shell nproc 2>/dev/null || echo 4)

# Cached verdicts (tools/ci-verdicts): with CI_SCOPE=changed, a Kani
# crate, a Miri crate or a fuzz target that passed before with exactly the
# inputs it has now - its sources and its local crates', a product's NoWayOS
# crates it reaches, their locked packages, the workspace's manifest
# and toolchain, the Containerfile's tool pins (FROM and ARG lines) and the
# check's own flags - is not run again, and says so (CACHED). Every pass
# records its verdict. They live beside the target volume's build output;
# delete CI_VERDICTS to forget them all. CI_SCOPE=full, make kani, make fuzz,
# make pds-miri and release-evidence never skip a check for a verdict.
CI_VERDICTS ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR)-verdicts,$(_ts_repo_dir)/build/verdicts)
_ts_verdicts = $(PYTHON) $(_ts_nowayos)/tools/ci-verdicts --root $(_ts_repo_dir) --dir $(CI_VERDICTS) \
	--pins $(_ts_nowayos)/tools/container/Containerfile \
	$(if $(wildcard $(_ts_repo_dir)/nowayos.rev),--upstream $(_ts_nowayos)) \
	--legacy-input $(_ts_nowayos)/tools/container/Containerfile \
	$(if $(wildcard $(_ts_repo_dir)/nowayos.rev),--legacy-input $(_ts_repo_dir)/nowayos.rev)
_ts_kani_salt = --salt "cargo kani -p"
_ts_miri_salt = --salt "MIRIFLAGS=$(PDS_MIRIFLAGS)"
_ts_fuzz_salt = --salt "FUZZ_SECONDS=$(FUZZ_SECONDS)" --salt "FUZZ_RUSTFLAGS=$(FUZZ_RUSTFLAGS)"
# In a recipe: the shell variable `items`, of KIND $(1) and salt $(2), cut to
# those with no verdict, each other one reported as cached ($(3) names the
# step).
_ts_keep = kept="$$($(_ts_verdicts) keep $(1) $$items $(2))" || exit 1; \
	for item in $$items; do case " $$kept " in *" $$item "*) ;; \
		*) echo "$(3): CACHED - $$item: passed before with exactly these inputs (CI_SCOPE=full runs it again)" ;; esac; done; \
	items="$$kept"
# nowayos-region reaches memory the kernel maps (outside any Rust
# allocation) through exposed provenance by design; this acknowledges that
# instead of a warning on every test. Miri's other checks stay on.
#
# Miri's aliasing model: Tree Borrows in the everyday gate, and
# Stacked Borrows - Miri's default, and the stricter about what unsafe code
# may do with references - at the checkpoint gate: make ci CI_SCOPE=full and
# release-evidence. Under Stacked Borrows every access through
# nowayos-region's 4 KiB words view retags the whole page from an exposed
# pointer, and its tests take about 11 minutes; under Tree Borrows, seconds.
# MIRI_MODEL=stacked or tree chooses either for any run.
MIRI_MODEL ?= $(if $(filter full,$(CI_SCOPE)),stacked,tree)
# Each Miri crate's log (make pds-miri runs them side by side).
PDS_MIRI_OUT ?= $(BUILD_ROOT)/miri
PDS_MIRIFLAGS ?= -Zmiri-permissive-provenance $(if $(filter tree,$(MIRI_MODEL)),-Zmiri-tree-borrows)
_ts_pds_toolchain = $(shell sed -n 's/^channel *= *"\(.*\)"/\1/p' $(_ts_nowayos)/pds/rust-toolchain.toml)
_ts_pds_host_cargo = $(PDS_CARGO) +$(_ts_pds_toolchain)
PDS_TOOLCHAIN = $(_ts_pds_toolchain)
# The toolchain the repo's pds/ pins: it must be NoWayOS's.
_ts_repo_pds_toolchain = $(shell sed -n 's/^channel *= *"\(.*\)"/\1/p' $(PDS_DIR)/rust-toolchain.toml 2>/dev/null)

# Each <program_image path="NAME.elf"/> in the system description is built
# from the pds workspace crate NAME (the same on every board).
_ts_pd_elves = $(shell $(PYTHON) -c 'import sys, xml.etree.ElementTree as t; print(" ".join(sorted({e.get("path", "") for e in t.parse(sys.argv[1]).iter("program_image")})))' $(_ts_src) 2>/dev/null)
_ts_pd_crates = $(patsubst %.elf,%,$(_ts_pd_elves))

# systems/SYSTEM.qemu, when there is one: the QEMU devices the system's PDs
# drive (for example `-device ramfb`), one or more per line, # for comments.
# A system given a display device has its screen checked: qemu-test compares
# a screendump with systems/SYSTEM.screen.png.
_ts_qemu_file = $(SYSTEMS_DIR)/$(SYSTEM).qemu
_ts_qemu_system_args = $(if $(wildcard $(_ts_qemu_file)),$(shell sed -e 's/\#.*//' $(_ts_qemu_file)))
_ts_screen_arg = $(if $(filter ramfb ramfb$(comma)%,$(_ts_qemu_system_args)),--screen $(SYSTEMS_DIR)/$(SYSTEM).screen.png)
# systems/SYSTEM.qmp, when there is one: steps that drive QEMU over QMP while
# the system runs - keys, pointer moves and clicks.
_ts_qmp_arg = $(if $(wildcard $(SYSTEMS_DIR)/$(SYSTEM).qmp),--qmp-script $(SYSTEMS_DIR)/$(SYSTEM).qmp)
comma := ,

_ts_qemu_cmd = $(QEMU) -machine $(QEMU_MACHINE) -cpu $(QEMU_CPU) -m size=$(QEMU_MEMORY) \
	-serial mon:stdio -nographic $(QEMU_NET) \
	-device loader,file=$(_ts_loader),addr=$(QEMU_LOADER_ADDR),cpu-num=0 $(_ts_qemu_system_args) $(QEMU_EXTRA_ARGS)

_ts_check_system = $(PYTHON) $(_ts_nowayos)/tools/check-system --sdf $(_ts_sdf) --grants $(_ts_grants) --board $(BOARD)
# The image's seL4 configuration, from the SDK, against the verification
# position for the pinned SDK (tools/sel4-config).
_ts_sel4_config = $(_ts_out)/sel4-config.txt
# The glyph atlases' notice: the font's copyright line and
# licence, OFL.txt in NoWayOS's atlas crate, beside every image (make image)
# and on every SD card (make sdcard), named in each image's SBOM with the
# font the atlases were made from (GLYPHS_DIR/src/atlas/manifest.toml).
GLYPHS_DIR ?= $(_ts_nowayos)/crates/nowayos-glyphs
# The icons' notice: Lucide's copyright line and ISC licence,
# LICENSE-LUCIDE.txt in NoWayOS's icon crate, beside every image and on every
# SD card, as the licence asks of every copy; the image's SBOM names the
# crate with its licence when a program links an icon.
ICONS_DIR ?= $(_ts_nowayos)/crates/nowayos-icons
# The image's SBOM (tools/image-sbom): its parts from the SDK, the
# description and cargo's unit graph for the PD build; for a board that boots
# from an SD card, what make sdcard puts beside it too.
_ts_image_sbom = $(_ts_out)/sbom.cdx.json
_ts_image_sbom_cmd = $(PYTHON) $(_ts_nowayos)/tools/image-sbom --system $(SYSTEM) --board $(BOARD) \
	--source $(patsubst $(_ts_repo_dir)/%,%,$(_ts_src)) --config $(MICROKIT_CONFIG) --dir $(_ts_out) --sdk $(MICROKIT_SDK) --pds $(PDS_DIR) \
	--glyphs $(GLYPHS_DIR) \
	--cargo $(PDS_CARGO) --toolchain $(_ts_pds_toolchain) --std-crates $(PDS_STD_CRATES) \
	$(if $(filter rpi5b_2gb,$(BOARD)),--sdcard-config $(SDCARD_CONFIG) --uboot-defconfig $(UBOOT_DEFCONFIG) \
		--uboot-fragment $(UBOOT_FRAGMENT) --firmware $(_ts_nowayos)/tools/container/rpi-firmware.sha256) \
	--out $(_ts_image_sbom)

# U-Boot for the Pi 5 (make uboot): rpi_arm64_defconfig plus our settings, from
# the source release pinned in the dev image (UBOOT_SOURCE). The release date
# fixes the build timestamp, so the same image builds the same u-boot.bin.
UBOOT_DEFCONFIG ?= rpi_arm64_defconfig
UBOOT_FRAGMENT ?= $(_ts_nowayos)/platforms/rpi5b_2gb-u-boot.config
UBOOT_SOURCE_DATE_EPOCH ?= 1783382400
_ts_uboot_out = $(BUILD_ROOT)/rpi5b_2gb/u-boot
# The source tree, in the target volume inside the container: tens of
# thousands of files are slow to unpack onto the bind mount.
UBOOT_BUILD_DIR ?= $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR)-uboot,$(_ts_uboot_out)/src)
# U-Boot's own make runs in a clean environment: our command-line variables
# (BOARD=qemu_virt_aarch64 among them) would otherwise reach it through
# MAKEFLAGS and override its own, and a fixed environment keeps the build
# reproducible.
_ts_uboot_make = cd $(UBOOT_BUILD_DIR) && env -i PATH="$$PATH" SOURCE_DATE_EPOCH=$(UBOOT_SOURCE_DATE_EPOCH) make

# The Pi 5 SD card (make sdcard): the firmware files from the release pinned in
# the dev image (RPI_FIRMWARE), U-Boot, our config.txt and the system image.
SDCARD_CONFIG ?= $(_ts_nowayos)/platforms/rpi5b_2gb-config.txt
SDCARD_FIRMWARE := start4.elf fixup4.dat bcm2712-rpi-5-b.dtb overlays LICENCE.broadcom COPYING.linux
_ts_sdcard = $(_ts_out)/sdcard

_ts_container_targets := pds image image-sbom image-reproducible qemu qemu-test qemu-test-all update-trial-test pds-lint pds-deny forget-verdicts \
	pds-test pds-miri unsafe-audit std-crates-check tools-test sel4-config-check screen-compiler-check \
	container-ci release-evidence sdk-boards system-clean uboot sdcard \
	$(if $(_ts_is_nowayos),,kani fuzz)

# make ci's steps, in order: the root Makefile's, then this file's.
# container-ci runs them, and so does release-evidence, one by one.
_ts_ci_root_steps := secrets lint test doc demo nostd deny atlases-check sbom fuzz kani
_ts_ci_system_steps := tools-test sel4-config-check unsafe-audit pds-lint pds-deny pds-test pds-miri \
	qemu-test-all std-crates-check screen-compiler-check
# release-evidence also builds each system for this board where its facts
# serve (empty: QEMU only).
EVIDENCE_PI_BOARD ?= rpi5b_2gb

# The Pi 5 on the bench: tools/pi-run, run on the host, where the
# Debug Probe is plugged in - Podman's VM cannot see USB serial devices.
PI_DEVICE ?=
PI_SYSTEMS ?= $(SYSTEM)
PI_WATCHDOG ?= 12
PI_COMMANDS ?= version
PI_HOLD ?= 14400
PI_TYPE ?=
_ts_pi_dir = $(BUILD_ROOT)/rpi5b_2gb/pi-run
ifneq ($(filter pi-run pi-show,$(MAKECMDGOALS)),)
PI_LOGS ?= $(_ts_pi_dir)/$(shell date +%Y%m%d-%H%M%S)
endif
_ts_pi_check = test -n "$(PI_DEVICE)" || { echo "error: name the Debug Probe's serial device: PI_DEVICE=/dev/cu.usbmodemXXXX (ls /dev/cu.usbmodem*)" >&2; exit 1; }
_ts_pi_host_only = echo "error: make $@ runs on the host, where the Debug Probe is plugged in: the dev container cannot see USB serial devices" >&2; exit 1

.PHONY: container shell system-help check-serial pi-run pi-uboot pi-show fuzz-run $(_ts_container_targets) \
	_ts-check-system _ts-check-pds-toolchain _ts-check-qemu-board _ts-check-sdcard

system-help:
	@sed -n '3,/^$$/p' $(_ts_system_mk) | sed 's/^# \{0,1\}//'

ifeq ($(NOWAYOS_CONTAINER),1)

# ---- Inside the dev container ----------------------------------------------

container:
	@echo "error: 'make container' builds the image; run it on the host" >&2; exit 1

shell:
	@echo "Already inside the dev container."

_ts-check-system:
	@test -f "$(_ts_src)" || { echo "error: no system description $(_ts_src) (have: $(_ts_systems))" >&2; exit 1; }
	@test -f "$(_ts_grants)" || { echo "error: no grants file $(_ts_grants): list everything $(SYSTEM).system grants (see tools/check-system)" >&2; exit 1; }
	@test -d "$(_ts_sdk_board)" || { echo "error: Microkit SDK $(MICROKIT_SDK) has no board/config $(BOARD)/$(MICROKIT_CONFIG) - see 'make sdk-boards'" >&2; exit 1; }
	@test -n "$(_ts_pd_crates)" || { echo "error: $(_ts_src) names no <program_image>" >&2; exit 1; }
	@mkdir -p $(_ts_out)
	@test -f $(BUILD_ROOT)/.gitignore || printf '*\n' > $(BUILD_ROOT)/.gitignore
	$(if $(_ts_update_prefills),$(if $(filter 1,$(UPDATE_BUNDLES_KEEP)),@test -f $(UPDATE_BUNDLES_OUT)/anchors.bin || )$(PYTHON) $(_ts_nowayos)/tools/update-bundles --out $(UPDATE_BUNDLES_OUT))
	@$(PYTHON) $(_ts_nowayos)/tools/board-system --board $(BOARD) \
		$(if $(SOURCE_DATE_EPOCH),--source-date-epoch '$(SOURCE_DATE_EPOCH)') \
		--update-bundles $(UPDATE_BUNDLES_OUT) \
		--commit-repo $(_ts_repo_dir) --commit-repo $(_ts_nowayos) $(_ts_src) $(_ts_sdf)
	@$(_ts_check_system)

# A product's PDs are built with NoWayOS's pinned nightly, and no other.
_ts-check-pds-toolchain:
	@test "$(_ts_repo_pds_toolchain)" = "$(_ts_pds_toolchain)" || { echo "error: $(PDS_DIR)/rust-toolchain.toml pins '$(_ts_repo_pds_toolchain)', but PDs are built with NoWayOS's pinned toolchain, $(_ts_pds_toolchain) ($(_ts_nowayos)/pds/rust-toolchain.toml): pin that" >&2; exit 1; }

_ts-check-qemu-board:
	@case "$(BOARD)" in qemu_virt_aarch64) ;; *) echo "error: BOARD=$(BOARD): the QEMU targets support BOARD=qemu_virt_aarch64 only" >&2; exit 1 ;; esac

pds: _ts-check-system _ts-check-pds-toolchain
	@echo "==> PDs for $(SYSTEM) ($(BOARD), $(MICROKIT_CONFIG)): $(_ts_pd_crates)"
	cd $(PDS_DIR) && SEL4_INCLUDE_DIRS=$(_ts_sdk_board)/include \
		$(PDS_CARGO) build --locked --release --target-dir $(_ts_pd_target) \
		$(addprefix -p ,$(_ts_pd_crates))
	@mkdir -p $(_ts_out)
	@test -f $(BUILD_ROOT)/.gitignore || printf '*\n' > $(BUILD_ROOT)/.gitignore
	cp $(addprefix $(_ts_pd_elf_dir)/,$(_ts_pd_elves)) $(_ts_out)/

# The Microkit tool also writes its capDL spec: the kernel objects, and the
# rights of every page each PD can reach, which check-system then compares
# with the grants file. A PD the system can restart has its restart image
# made from its program first (tools/restart-image), which the tool
# loads like any prefill; after, the image is checked against the threads
# the tool made. The glyph atlases' notice goes beside every image, whether
# its programs draw with the atlases yet or not.
image: pds
	@echo "==> Microkit image for $(SYSTEM): $(_ts_loader)"
	$(PYTHON) $(_ts_nowayos)/tools/restart-image --sdf $(_ts_sdf) --dir $(_ts_out)
	$(MICROKIT_SDK)/bin/microkit $(_ts_sdf) --search-path $(_ts_out) \
		--board $(BOARD) --config $(MICROKIT_CONFIG) \
		-o $(_ts_loader) -r $(_ts_out)/report.txt --capdl-json $(_ts_out)/capdl.json
	$(_ts_check_system) --capdl $(_ts_out)/capdl.json --report $(_ts_out)/report.txt \
		--sdk-memory $(_ts_sdk_board)/platform_gen.json \
		$(if $(wildcard $(_ts_qemu_file)),--qemu $(_ts_qemu_file))
	$(PYTHON) $(_ts_nowayos)/tools/restart-image --sdf $(_ts_sdf) --dir $(_ts_out) --capdl $(_ts_out)/capdl.json
	$(PYTHON) $(_ts_nowayos)/tools/sel4-config record --sdk $(MICROKIT_SDK) \
		--board $(BOARD) --config $(MICROKIT_CONFIG) --system $(SYSTEM) \
		--image $(_ts_loader) --out $(_ts_sel4_config)
	cp $(GLYPHS_DIR)/OFL.txt $(_ts_out)/OFL.txt
	cp $(ICONS_DIR)/LICENSE-LUCIDE.txt $(_ts_out)/LICENSE-LUCIDE.txt
	$(_ts_image_sbom_cmd)

image-sbom: image
	@echo "==> SBOM of $(SYSTEM)'s image ($(BOARD), $(MICROKIT_CONFIG)): $(_ts_image_sbom)"

qemu: _ts-check-qemu-board image
	@echo "==> Booting $(SYSTEM) in QEMU - quit with Ctrl-A, then x"
ifeq ($(VNC),1)
	@password=$$($(PYTHON) -c 'import secrets; print(secrets.token_hex(4))'); \
	echo "==> QEMU's screen over VNC: open vnc://localhost:$(VNC_PORT) (macOS: Finder, Go, Connect to Server)"; \
	echo "    with the password $$password - made for this run only, and never stored"; \
	$(_ts_qemu_cmd) -vnc 0.0.0.0:0,password-secret=vnc -object secret,id=vnc,data=$$password
else
	$(_ts_qemu_cmd)
endif

qemu-test: _ts-check-qemu-board image
	@test -f "$(_ts_expect)" || { echo "error: no expected-output file $(_ts_expect)" >&2; exit 1; }
	$(PYTHON) $(_ts_nowayos)/tools/qemu-test --expect $(_ts_expect) \
		--log $(_ts_out)/serial.log --timeout $(QEMU_TIMEOUT) --settle $(QEMU_SETTLE) \
		$(if $(QEMU_BAUD),--baud $(QEMU_BAUD)) \
		$(_ts_screen_arg) $(_ts_qmp_arg) -- $(_ts_qemu_cmd)

# A system boots in MICROKIT_CONFIG, or in each configuration its
# systems/SYSTEM.configs lists (the supervisor's panic test runs in the
# release configuration too, as a product would); and then again in each,
# its serial taken at each line rate its systems/SYSTEM.baud lists (a
# real UART's, at which two PDs printing at once would mix their lines).
qemu-test-all:
	@test -n "$(_ts_systems)" || { echo "error: no systems/*.system" >&2; exit 1; }
	@set -e; for s in $(_ts_systems); do \
		if [ -f $(SYSTEMS_DIR)/$$s.harness ]; then continue; fi; \
		configs=$$(sed -e 's/#.*//' $(SYSTEMS_DIR)/$$s.configs 2>/dev/null || true); \
		bauds=$$(sed -e 's/#.*//' $(SYSTEMS_DIR)/$$s.baud 2>/dev/null || true); \
		for c in $${configs:-$(MICROKIT_CONFIG)}; do \
			$(MAKE) -f $(_ts_main_mk) qemu-test SYSTEM=$$s MICROKIT_CONFIG=$$c; \
			for b in $$bauds; do \
				$(MAKE) -f $(_ts_main_mk) qemu-test SYSTEM=$$s MICROKIT_CONFIG=$$c QEMU_BAUD=$$b; \
			done; \
		done; \
		if [ -f $(SYSTEMS_DIR)/$$s.reproducible ]; then \
			$(MAKE) -f $(_ts_main_mk) image-reproducible SYSTEM=$$s; \
		fi; \
	done
	@set -e; for t in $$(sed -e 's/#.*//' $(wildcard $(SYSTEMS_DIR)/*.harness) /dev/null | sort -u); do \
		$(MAKE) -f $(_ts_main_mk) $$t; \
	done

# The update tests across reboots: tools/update-trial builds
# update-trial-r1 and -r2 with the run's keys, then boots them run after run,
# playing the bootloader between runs - reading the update state each run's
# storage stand-in prints and booting the slot it names, a trial once - and
# checks each run against systems/update-trial/RUN.expect. qemu-test-all
# runs it: those two systems name it in their .harness files, and are not
# booted alone.
update-trial-test: _ts-check-qemu-board
	$(PYTHON) $(_ts_nowayos)/tools/update-trial --make "$(MAKE) -f $(_ts_main_mk)" \
		--qemu "$(QEMU) -machine $(QEMU_MACHINE) -cpu $(QEMU_CPU) -m size=$(QEMU_MEMORY) -serial mon:stdio -nographic $(QEMU_NET)" \
		--loader-addr $(QEMU_LOADER_ADDR) --build-root $(BUILD_ROOT) --systems $(SYSTEMS_DIR) \
		--timeout $(UPDATE_TRIAL_TIMEOUT)

# The same inputs give the same image: SYSTEM's image built, set
# aside, its build removed and built again, and the two compared byte for
# byte - with the update tests' keys and bundles kept between the two
# (UPDATE_BUNDLES_KEEP=1), as they are inputs like the source. qemu-test-all
# runs it for each system with a systems/NAME.reproducible file.
image-reproducible:
	$(MAKE) -f $(_ts_main_mk) image SYSTEM=$(SYSTEM) UPDATE_BUNDLES_KEEP=1
	@mkdir -p $(BUILD_ROOT)/reproducible
	cp $(_ts_loader) $(BUILD_ROOT)/reproducible/$(SYSTEM)-first.img
	rm -rf $(_ts_out)
	$(MAKE) -f $(_ts_main_mk) image SYSTEM=$(SYSTEM) UPDATE_BUNDLES_KEEP=1
	@if cmp -s $(BUILD_ROOT)/reproducible/$(SYSTEM)-first.img $(_ts_loader); then \
		echo "image-reproducible: PASS - $(SYSTEM): two builds from the same inputs give the same image ($$(sha256sum $(_ts_loader) | cut -c1-16))"; \
	else \
		echo "image-reproducible: FAIL - $(SYSTEM): two builds from the same inputs differ: $(BUILD_ROOT)/reproducible/$(SYSTEM)-first.img and $(_ts_loader)" >&2; exit 1; \
	fi

check-serial:
	@test -n "$(LOG)" || { echo "error: name the captured log: make check-serial SYSTEM=$(SYSTEM) LOG=FILE" >&2; exit 1; }
	@test -f "$(_ts_expect)" || { echo "error: no expected-output file $(_ts_expect) (have: $(_ts_systems))" >&2; exit 1; }
	$(PYTHON) $(_ts_nowayos)/tools/check-serial --expect $(_ts_expect) --log $(LOG)

pds-lint: _ts-check-pds-toolchain
	cd $(PDS_DIR) && $(PDS_CARGO) fmt --all -- --check
	cd $(PDS_DIR) && SEL4_INCLUDE_DIRS=$(_ts_sdk_board)/include \
		$(PDS_CARGO) clippy --locked --release --target-dir $(_ts_pd_target) \
		--workspace $(addprefix --exclude ,$(PDS_LINT_ALONE)) -- -D warnings
	@set -e; for crate in $(PDS_LINT_ALONE); do \
		echo "==> $$crate, on its own"; \
		(cd $(PDS_DIR) && SEL4_INCLUDE_DIRS=$(_ts_sdk_board)/include \
			$(PDS_CARGO) clippy --locked --release --target-dir $(_ts_pd_target) \
			-p $$crate -- -D warnings); \
	done

# rust-sel4 is a git dependency: cargo-deny allows its URL, and
# check-git-pins allows it only at the commit NoWayOS's pds/Cargo.toml pins.
pds-deny:
	cd $(PDS_DIR) && $(PDS_CARGO) deny --locked check
	$(PYTHON) $(_ts_nowayos)/tools/check-git-pins --lock $(PDS_DIR)/Cargo.lock \
		--pins $(_ts_nowayos)/pds/Cargo.toml

ifneq ($(strip $(PDS_LIB_CRATES)),)
pds-test:
	cd $(_ts_repo_dir) && $(_ts_pds_host_cargo) test --locked \
		--manifest-path $(PDS_DIR)/Cargo.toml --target-dir $(PDS_TARGET_DIR)/host \
		$(addprefix -p ,$(PDS_LIB_CRATES))
	cd $(_ts_repo_dir) && $(_ts_pds_host_cargo) clippy --locked \
		--manifest-path $(PDS_DIR)/Cargo.toml --target-dir $(PDS_TARGET_DIR)/host \
		--all-targets $(addprefix -p ,$(PDS_LIB_CRATES)) -- -D warnings
else
pds-test:
	@echo "pds-test: PDS_LIB_CRATES names no library crate in $(PDS_DIR) to test without seL4"
endif

pds-miri:
	@crates="$(PDS_MIRI_CRATES)"; \
	if [ -z "$$crates" ]; then \
		crates="$$($(PYTHON) $(_ts_nowayos)/tools/check-unsafe $(_ts_unsafe_args) --miri-crates pds)" || exit 1; \
	fi; \
	if [ -z "$$crates" ]; then echo "pds-miri: no pds crate on the approved-unsafe list needs Miri"; exit 0; fi; \
	if [ "$(CI_SCOPE)" = changed ]; then \
		crates="$$($(PYTHON) $(_ts_nowayos)/tools/ci-scope --root $(_ts_repo_dir) --base $(or $(CI_BASE),origin/main) kani $$crates)" || exit 1; \
		if [ -z "$$crates" ]; then echo "pds-miri: SKIP - no approved-unsafe crate reaches a change (CI_SCOPE=changed; CI_SCOPE=full runs Miri)"; exit 0; fi; \
		items="$$crates"; $(call _ts_keep,miri,$(_ts_miri_salt),pds-miri); crates="$$items"; \
		if [ -z "$$crates" ]; then exit 0; fi; \
	fi; \
	echo "==> Miri over: $$crates ($(MIRI_MODEL) borrows, each crate its own run, side by side)"; \
	mkdir -p $(PDS_MIRI_OUT); \
	for crate in $$crates; do \
		{ start=$$(date +%s); \
		  cd $(_ts_repo_dir) && XDG_CACHE_HOME=$(PDS_TARGET_DIR)/cache MIRIFLAGS="$(PDS_MIRIFLAGS)" \
			$(_ts_pds_host_cargo) miri test --locked --manifest-path $(PDS_DIR)/Cargo.toml \
			--target-dir $(PDS_TARGET_DIR)/miri-$$crate -p $$crate > $(PDS_MIRI_OUT)/$$crate.log 2>&1; \
		  echo "$$? $$(( $$(date +%s) - start ))" > $(PDS_MIRI_OUT)/$$crate.status; } & \
	done; \
	wait; \
	failed=0; \
	for crate in $$crates; do \
		read -r status seconds < $(PDS_MIRI_OUT)/$$crate.status || status=1; \
		if [ "$$status" = 0 ]; then \
			echo "pds-miri: PASS - $$crate: $$(grep -h '^test result' $(PDS_MIRI_OUT)/$$crate.log | head -n 1 | sed 's/; finished.*//') in $$seconds s"; \
			$(_ts_verdicts) record miri $$crate $(_ts_miri_salt) || exit 1; \
		else \
			tail -n 40 $(PDS_MIRI_OUT)/$$crate.log >&2; \
			echo "pds-miri: FAIL - $$crate: the log is in $(PDS_MIRI_OUT)/$$crate.log" >&2; \
			failed=1; \
		fi; \
	done; \
	exit $$failed

unsafe-audit:
	$(PYTHON) $(_ts_nowayos)/tools/check-unsafe $(_ts_unsafe_args)

forget-verdicts:
	rm -rf $(CI_VERDICTS)
	@echo "forget-verdicts: the next make ci runs every Kani crate, Miri crate and fuzz target a change reaches"

ifeq ($(_ts_is_nowayos),)
# A product repo's make kani, as NoWayOS's: the crates proved in one
# pool of KANI_JOBS harnesses, each crate's verdict recorded only if
# the pool passed, a failure's last lines and its log named; each
# mutant, which must fail, runs beside them. CI_SCOPE=changed proves
# only the crates a change since CI_BASE reaches, and runs only their mutants,
# each skipped while it has a verdict under its crate's inputs, its name and
# the mutations file.
_ts_mutation_args = --root $(_ts_repo_dir) --mutations-file $(_ts_repo_dir)/$(KANI_MUTATIONS_FILE)
_ts_mutant_salt = $(_ts_kani_salt) --salt "kani-mutation $$name" --input $(_ts_repo_dir)/$(KANI_MUTATIONS_FILE)
kani:
	@crates="$(strip $(KANI_CRATES))"; \
	if [ -z "$$crates" ]; then echo "kani: nothing to prove - KANI_CRATES names no crate in $(notdir $(_ts_repo_dir))"; exit 0; fi; \
	pairs=""; \
	if [ -n "$(KANI_MUTATIONS_FILE)" ]; then \
		pairs="$$($(PYTHON) $(_ts_nowayos)/tools/kani-mutation $(_ts_mutation_args) --list | sed -n 's/^\([A-Za-z0-9_-]*\) ([^)]*): \([A-Za-z0-9_-]*\),.*/\1:\2/p')" || exit 1; \
	fi; \
	if [ "$(CI_SCOPE)" = changed ]; then \
		crates="$$($(PYTHON) $(_ts_nowayos)/tools/ci-scope --root $(_ts_repo_dir) --base $(or $(CI_BASE),origin/main) kani $$crates)" || exit 1; \
		if [ -z "$$crates" ]; then echo "kani: SKIP - no proven crate reaches a change (CI_SCOPE=changed; CI_SCOPE=full proves them all)"; exit 0; fi; \
		reached=""; \
		for pair in $$pairs; do \
			name=$${pair%%:*}; crate=$${pair##*:}; \
			case " $$crates " in *" $$crate "*) ;; *) continue ;; esac; \
			if [ -n "$$($(_ts_verdicts) keep kani $$crate $(_ts_mutant_salt))" ]; then reached="$$reached $$pair"; \
			else echo "kani-mutation: CACHED - $$name: passed before with exactly these inputs (CI_SCOPE=full runs it again)"; fi; \
		done; \
		pairs="$$reached"; \
		items="$$crates"; $(call _ts_keep,kani,$(_ts_kani_salt),kani); crates="$$items"; \
	fi; \
	mkdir -p $(KANI_OUT); \
	names=""; for pair in $$pairs; do names="$$names --mutation $${pair%%:*}"; done; \
	if [ -n "$$names" ]; then \
		( $(PYTHON) $(_ts_nowayos)/tools/kani-mutation $(_ts_mutation_args) --out $(KANI_OUT) $$names \
			> $(KANI_OUT)/mutants.out 2>&1; echo $$? > $(KANI_OUT)/mutants.rc ) & \
	fi; \
	failed=0; \
	if [ -n "$$crates" ]; then \
		log=$(KANI_OUT)/kani.log; \
		start=$$(date +%s); \
		packages=""; for crate in $$crates; do packages="$$packages -p $$crate"; done; \
		if (cd $(_ts_repo_dir) && cargo kani $$packages $(_ts_kani_jobs)) > $$log 2>&1; then \
			echo "kani: PASS - $$(echo $$crates): $$(sed -n 's/^Complete - //p' $$log) in $$(( $$(date +%s) - start )) s"; \
			for crate in $$crates; do $(_ts_verdicts) record kani $$crate $(_ts_kani_salt) || exit 1; done; \
		else \
			tail -n 40 $$log >&2; \
			echo "kani: FAIL - $$(echo $$crates): the log is in $$log; no verdict recorded" >&2; \
			failed=1; \
		fi; \
	fi; \
	if [ -n "$$names" ]; then \
		wait; cat $(KANI_OUT)/mutants.out; \
		[ "$$(cat $(KANI_OUT)/mutants.rc)" = 0 ] || exit 1; \
		for pair in $$pairs; do \
			name=$${pair%%:*}; crate=$${pair##*:}; \
			$(_ts_verdicts) record kani $$crate $(_ts_mutant_salt) || exit 1; \
		done; \
	fi; \
	exit $$failed

# A product repo's make fuzz, as NoWayOS's: with no fuzz/
# workspace, nothing to fuzz; CI_SCOPE=changed keeps only the targets a
# change since CI_BASE reaches (tools/ci-scope); fuzz-run runs the rest.
fuzz:
	@if [ ! -f fuzz/Cargo.toml ]; then echo "fuzz: nothing to fuzz - $(notdir $(_ts_repo_dir)) has no fuzz/ workspace"; exit 0; fi; \
	targets="$(strip $(FUZZ_TARGETS))"; \
	if [ "$(CI_SCOPE)" = changed ] && [ -z "$(FUZZ_INPUT)" ] && [ -n "$$targets" ]; then \
		targets="$$($(PYTHON) $(_ts_nowayos)/tools/ci-scope --root $(_ts_repo_dir) --base $(or $(CI_BASE),origin/main) fuzz $$targets)" || exit 1; \
		if [ -z "$$targets" ]; then echo "fuzz: SKIP - no fuzz target reaches a change (CI_SCOPE=changed; CI_SCOPE=full runs them all)"; exit 0; fi; \
		items="$$targets"; $(call _ts_keep,fuzz,$(_ts_fuzz_salt),fuzz); targets="$$items"; \
		if [ -z "$$targets" ]; then exit 0; fi; \
	fi; \
	$(MAKE) --no-print-directory -f $(_ts_main_mk) fuzz-run FUZZ_TARGETS="$$targets"
endif

# make fuzz's run, for NoWayOS and every product repo:
# rustfmt and clippy, then every target in FUZZ_TARGETS built with
# libFuzzer's instrumentation (FUZZ_RUSTFLAGS) for the container's own
# target, and each run for FUZZ_SECONDS seconds, FUZZ_JOBS side by side,
# its corpus kept in FUZZ_OUT/corpus/TARGET and its verdict
# recorded (CI_VERDICTS). A crash, a broken check or a hang (an input taking
# over 10 s) fails the run, and libFuzzer saves the input in
# FUZZ_OUT/artifacts/TARGET/; FUZZ_INPUT=FILE runs the first of FUZZ_TARGETS
# on that one input, to show the failure. Fixed, the input becomes an
# ordinary regression test in the crate that had the bug.
fuzz-run:
	@test -n "$(FUZZ_TARGETS)" || { echo "error: no fuzz target: FUZZ_TARGETS is empty" >&2; exit 1; }
	cd fuzz && $(CARGO) fmt -- --check
	cd fuzz && $(CARGO) clippy --locked --all-targets --target-dir $(FUZZ_TARGET_DIR) -- -D warnings
	host=$$(rustc -vV | sed -n 's/^host: //p') && cd fuzz && RUSTFLAGS="$(FUZZ_RUSTFLAGS)" \
		$(CARGO) build --locked --release --target $$host --target-dir $(FUZZ_TARGET_DIR) \
		$(addprefix --bin ,$(FUZZ_TARGETS))
	@mkdir -p $(FUZZ_OUT)$(if $(filter build/%,$(FUZZ_OUT)), && { test -f build/.gitignore || printf '*\n' > build/.gitignore; })
	@bin=$(FUZZ_TARGET_DIR)/$$(rustc -vV | sed -n 's/^host: //p')/release; \
	if [ -n "$(FUZZ_INPUT)" ]; then \
		echo "==> $(firstword $(FUZZ_TARGETS)) on $(FUZZ_INPUT)"; \
		$$bin/$(firstword $(FUZZ_TARGETS)) $(FUZZ_INPUT); exit $$?; \
	fi; \
	echo "==> $(words $(FUZZ_TARGETS)) target(s), $(FUZZ_SECONDS) s each, $(FUZZ_JOBS) at a time"; \
	running=0; \
	for target in $(FUZZ_TARGETS); do \
		mkdir -p $(FUZZ_OUT)/corpus/$$target $(FUZZ_OUT)/artifacts/$$target; \
		rm -f $(FUZZ_OUT)/$$target.status; \
		{ $$bin/$$target -max_total_time=$(FUZZ_SECONDS) -timeout=10 -print_final_stats=1 \
			-artifact_prefix=$(FUZZ_OUT)/artifacts/$$target/ $(FUZZ_OUT)/corpus/$$target \
			> $(FUZZ_OUT)/$$target.log 2>&1; echo $$? > $(FUZZ_OUT)/$$target.status; } & \
		running=$$((running + 1)); \
		if [ $$running -ge $(FUZZ_JOBS) ]; then wait; running=0; fi; \
	done; \
	wait; \
	failed=0; \
	for target in $(FUZZ_TARGETS); do \
		log=$(FUZZ_OUT)/$$target.log; \
		if [ "$$(cat $(FUZZ_OUT)/$$target.status 2>/dev/null)" = 0 ]; then \
			echo "fuzz: PASS - $$target"; \
			sed -n 's/^#\([0-9]*\)[[:space:]]*DONE[[:space:]]*/    \1 runs, /p' $$log; \
			$(_ts_verdicts) record fuzz $$target $(_ts_fuzz_salt) || exit 1; \
		else \
			tail -n 25 $$log >&2; \
			echo "fuzz: FAIL - $$target: the input is in $(FUZZ_OUT)/artifacts/$$target/ and the log in $$log;" \
				"make fuzz FUZZ_TARGETS=$$target FUZZ_INPUT=FILE runs it again" >&2; \
			failed=1; \
		fi; \
	done; \
	exit $$failed

std-crates-check:
	$(PYTHON) $(_ts_nowayos)/tools/check-std-crates --deps $(_ts_pd_elf_dir)/deps \
		--lock $(PDS_DIR)/Cargo.lock --list $(PDS_STD_CRATES) \
		--library "$$(rustc +$(_ts_pds_toolchain) --print sysroot)/lib/rustlib/src/rust/library"
	$(PYTHON) $(_ts_nowayos)/tools/check-backends --deps $(_ts_pd_elf_dir)/deps

tools-test:
	cd $(_ts_nowayos) && PYTHONDONTWRITEBYTECODE=1 $(PYTHON) tools/run-tests

sel4-config-check:
	$(PYTHON) $(_ts_nowayos)/tools/sel4-config check

# make ci runs Kani, the fuzz targets and Miri only where a change reaches
# (CI_SCOPE=changed; tools/ci-scope); make ci CI_SCOPE=full runs every one,
# and CI_BASE (origin/main) is what a change is measured from.
_ts_ci_scope_args = CI_SCOPE=$(or $(CI_SCOPE),changed) CI_BASE=$(or $(CI_BASE),origin/main)
container-ci:
	$(MAKE) -C $(_ts_nowayos) $(_ts_ci_root_steps) CARGO=cargo $(_ts_ci_scope_args)
	$(MAKE) -f $(_ts_system_mk) $(_ts_ci_system_steps) $(_ts_ci_scope_args)

# The release evidence pack (tools/release-evidence), for the commit
# checked out: it refuses uncommitted changes in nowayos and in the screen-compiler
# clone screen-compiler-check uses, then runs container-ci's steps one by one.
release-evidence:
	@commit=$$(git -C $(_ts_nowayos) rev-parse --short=12 HEAD) || exit 1; \
	screen_compiler=; \
	if [ -f $(SCREEN_COMPILER_DIR)/Makefile ] && [ "$$(cd $(SCREEN_COMPILER_DIR)/../nowayos 2>/dev/null && pwd -P)" = "$$(cd $(_ts_nowayos) && pwd -P)" ]; then \
		screen_compiler="--screen-compiler $(SCREEN_COMPILER_DIR)"; \
	fi; \
	set -x; $(PYTHON) $(_ts_nowayos)/tools/release-evidence --out $(_ts_nowayos)/build/release-evidence/$$commit \
		--root-steps "$(_ts_ci_root_steps)" --system-steps "$(_ts_ci_system_steps)" --make "$(MAKE)" \
		$$screen_compiler $(if $(EVIDENCE_PI_BOARD),--pi-board $(EVIDENCE_PI_BOARD))

# Screen Compiler's generated screens against this nowayos-ui, drawn
# by this nowayos-render-eg and matched with Screen Compiler's approved images:
# run Screen Compiler's own check when a screen-compiler clone sits beside
# nowayos (the workspace layout the dev container mounts), skipped with a
# notice when not. A deliberate change to layout or drawing fails it until
# the images are redrawn in screen-compiler (make bless-images) and approved there.
SCREEN_COMPILER_DIR ?= $(_ts_nowayos)/../screen-compiler
screen-compiler-check:
	@if [ ! -f $(SCREEN_COMPILER_DIR)/Makefile ]; then \
		echo "notice: no screen-compiler clone at $(SCREEN_COMPILER_DIR) - skipping the check that Screen Compiler's generated screens build against nowayos-ui and draw as approved"; \
	elif [ ! -f $(SCREEN_COMPILER_DIR)/contract-check/Cargo.toml ]; then \
		echo "error: the screen-compiler clone at $(SCREEN_COMPILER_DIR) predates make contract-check: pull it" >&2; exit 1; \
	elif [ ! -d $(SCREEN_COMPILER_DIR)/contract-check/golden ]; then \
		echo "error: the screen-compiler clone at $(SCREEN_COMPILER_DIR) predates its approved images: pull it" >&2; exit 1; \
	elif [ "$$(cd $(SCREEN_COMPILER_DIR)/../nowayos 2>/dev/null && pwd -P)" != "$$(cd $(_ts_nowayos) && pwd -P)" ]; then \
		echo "notice: $(SCREEN_COMPILER_DIR) builds against $(SCREEN_COMPILER_DIR)/../nowayos, not this clone ($(_ts_nowayos)) - skipping the check that Screen Compiler's generated screens build against nowayos-ui and draw as approved"; \
	else \
		CARGO_TARGET_DIR=/cache/target/screen-compiler $(MAKE) --no-print-directory -C $(SCREEN_COMPILER_DIR) \
			contract-check CARGO=cargo LOCKED=--locked; \
	fi

# A clean build every time: fresh source, our settings merged onto the
# defconfig and checked, then u-boot.bin and its sha256. No version-control
# suffix (.scmversion), and U-Boot's release date as the build date.
uboot:
	@test -f "$$UBOOT_SOURCE" || { echo "error: this dev image has no U-Boot source (UBOOT_SOURCE): rebuild it with 'make container'" >&2; exit 1; }
	@case "$$(uname -m)" in aarch64) ;; *) echo "error: make uboot uses the dev container's native AArch64 compiler, and this container is $$(uname -m) with no cross-compiler" >&2; exit 1 ;; esac
	rm -rf $(_ts_uboot_out) $(UBOOT_BUILD_DIR)
	mkdir -p $(_ts_uboot_out) $(UBOOT_BUILD_DIR)
	@test -f $(BUILD_ROOT)/.gitignore || printf '*\n' > $(BUILD_ROOT)/.gitignore
	tar -xjf "$$UBOOT_SOURCE" -C $(UBOOT_BUILD_DIR) --strip-components=1 --no-same-owner
	: > $(UBOOT_BUILD_DIR)/.scmversion
	$(_ts_uboot_make) -s $(UBOOT_DEFCONFIG)
	cd $(UBOOT_BUILD_DIR) && ./scripts/kconfig/merge_config.sh -m .config $(UBOOT_FRAGMENT) >/dev/null
	$(_ts_uboot_make) -s olddefconfig
	@sed -n -e '/^CONFIG_/p' -e '/^# CONFIG_.* is not set$$/p' $(UBOOT_FRAGMENT) | while IFS= read -r line; do \
		grep -qxF "$$line" $(UBOOT_BUILD_DIR)/.config || { echo "error: U-Boot's final .config lacks '$$line' from $(UBOOT_FRAGMENT)" >&2; exit 1; }; \
	done
	$(_ts_uboot_make) -s -j$$(nproc) u-boot.bin
	cp $(UBOOT_BUILD_DIR)/u-boot.bin $(_ts_uboot_out)/
	cp $(UBOOT_BUILD_DIR)/.config $(_ts_uboot_out)/u-boot.config
	cd $(_ts_uboot_out) && sha256sum u-boot.bin | tee u-boot.bin.sha256
	@echo "==> U-Boot $$NOWAYOS_UBOOT_VERSION for the Pi 5: $(_ts_uboot_out)/u-boot.bin ($(UBOOT_DEFCONFIG) + $(notdir $(UBOOT_FRAGMENT)))"

# Checked before anything is built: the board, the firmware in the image, and
# that config.txt neither programs OTP nor enables secure boot: no over_voltage
# setting either, as one with force_turbo=1 sets the warranty bit in OTP.
_ts-check-sdcard:
	@case "$(BOARD)" in rpi5b_2gb) ;; *) echo "error: BOARD=$(BOARD): make sdcard is for BOARD=rpi5b_2gb only" >&2; exit 1 ;; esac
	@test -d "$$RPI_FIRMWARE" || { echo "error: this dev image has no Raspberry Pi firmware (RPI_FIRMWARE): rebuild it with 'make container'" >&2; exit 1; }
	@if grep -n -i -E '^[[:space:]]*(program_|revoke_devkey|signed_boot|over_voltage|never_over_voltage)' $(SDCARD_CONFIG) >&2; then \
		echo "error: $(SDCARD_CONFIG) would program OTP or enable secure boot - never on a development board" >&2; exit 1; \
	fi

# The card's boot partition, rebuilt from scratch: the same inputs give the
# same files, listed with their sha256 in SHA256SUMS.
sdcard: _ts-check-sdcard image uboot
	rm -rf $(_ts_sdcard)
	mkdir -p $(_ts_sdcard)
	cd "$$RPI_FIRMWARE" && cp -R $(SDCARD_FIRMWARE) $(_ts_sdcard)/
	cp $(_ts_uboot_out)/u-boot.bin $(_ts_loader) $(_ts_out)/OFL.txt $(_ts_out)/LICENSE-LUCIDE.txt $(_ts_sdcard)/
	cp $(SDCARD_CONFIG) $(_ts_sdcard)/config.txt
	cd $(_ts_sdcard) && find . -type f ! -name SHA256SUMS | sed 's|^\./||' | LC_ALL=C sort | xargs sha256sum > SHA256SUMS
	$(_ts_image_sbom_cmd) --uboot-bin $(_ts_uboot_out)/u-boot.bin
	@echo "==> Pi 5 SD card for $(SYSTEM): $(_ts_sdcard) ($$(wc -l < $(_ts_sdcard)/SHA256SUMS) files and SHA256SUMS, firmware $$NOWAYOS_RPI_FIRMWARE_VERSION, U-Boot $$NOWAYOS_UBOOT_VERSION): copy them to the card's FAT32 boot partition"

sdk-boards:
	@echo "Microkit SDK $$(cat $(MICROKIT_SDK)/VERSION) at $(MICROKIT_SDK):"
	@for b in $$(ls $(MICROKIT_SDK)/board); do \
		printf '  %-22s %s\n' "$$b" "$$(ls $(MICROKIT_SDK)/board/$$b | tr '\n' ' ')"; \
	done

system-clean:
	rm -rf $(BUILD_ROOT) $(PDS_TARGET_DIR) $(UBOOT_BUILD_DIR)

pi-run pi-uboot pi-show:
	@$(_ts_pi_host_only)

else

# ---- On the host: run the same target inside the dev container -------------

_ts_container_nowayos = /work/$(patsubst $(WS)/%,%,$(_ts_nowayos))
# In the container, NoWayOS's systems run this file; a product's run its own
# Makefile, in its own folder, which sets SYSTEM_REPO there too.
_ts_container_make = $(if $(_ts_is_nowayos),make -C $(_ts_container_nowayos) -f tools/make/system.mk,make -C /work/$(patsubst $(WS)/%,%,$(_ts_repo_dir)))
_ts_check_repo_in_ws = test -n "$(filter $(WS)/%,$(_ts_repo_dir))" || { echo "error: $(_ts_repo_dir) is outside the workspace $(WS), which is all the dev container can see: keep the repo beside nowayos/" >&2; exit 1; }
SYSTEM_FORWARD ?=
_ts_forward = BOARD=$(BOARD) MICROKIT_CONFIG=$(MICROKIT_CONFIG) SYSTEM=$(SYSTEM) \
	QEMU_TIMEOUT=$(QEMU_TIMEOUT) QEMU_SETTLE=$(QEMU_SETTLE) $(if $(QEMU_BAUD),QEMU_BAUD=$(QEMU_BAUD)) \
	$(if $(VNC),VNC=$(VNC) VNC_PORT=$(VNC_PORT)) \
	$(if $(NOWAYOS_UPDATE_SNAPSHOTS),NOWAYOS_UPDATE_SNAPSHOTS=$(NOWAYOS_UPDATE_SNAPSHOTS)) \
	EVIDENCE_PI_BOARD=$(EVIDENCE_PI_BOARD) $(if $(SOURCE_DATE_EPOCH),SOURCE_DATE_EPOCH=$(SOURCE_DATE_EPOCH)) \
	$(if $(CI_SCOPE),CI_SCOPE=$(CI_SCOPE)) $(if $(CI_BASE),CI_BASE=$(CI_BASE)) \
	$(if $(UPDATE_BUNDLES_KEEP),UPDATE_BUNDLES_KEEP=$(UPDATE_BUNDLES_KEEP)) \
	FUZZ_SECONDS=$(FUZZ_SECONDS) $(if $(FUZZ_INPUT),FUZZ_INPUT=$(FUZZ_INPUT)) \
	$(if $(filter command line,$(origin FUZZ_TARGETS)),FUZZ_TARGETS="$(FUZZ_TARGETS)") \
	$(SYSTEM_FORWARD)

# The image is built under a candidate tag, then checked: its Debian packages
# (which come from the live bookworm archive, unlike every other download)
# must be exactly the reviewed list for its architecture. Only then does it
# replace $(IMAGE). UPDATE_DEBIAN_LOCK=1 writes the list from the new image
# instead, for review in the same change.
#
# The engine's layer cache reuses the Debian layer of an earlier build, so a
# cached build never sees the archive move on. NO_CACHE=1 rebuilds every
# layer: use it to refresh the packages (with UPDATE_DEBIAN_LOCK=1), and after
# pulling a reviewed update of the list.
UPDATE_DEBIAN_LOCK ?=
NO_CACHE ?=
_ts_candidate = $(IMAGE)-candidate
_ts_debian_lock_dir = $(_ts_nowayos)/tools/container

container:
	$(ENGINE) build $(if $(filter 1,$(NO_CACHE)),--no-cache) \
		--label $(NOWAYOS_IMAGE_LABEL)=$(_ts_containerfile_sha) \
		-t $(_ts_candidate) -f $(_ts_containerfile) $(_ts_nowayos)/tools/container
	@set -e; \
	arch=$$($(ENGINE) run --rm --pull=never $(_ts_candidate) dpkg --print-architecture); \
	lock=$(_ts_debian_lock_dir)/debian-packages-$$arch.lock; \
	actual=$$(mktemp); trap 'rm -f "$$actual"' EXIT; \
	$(ENGINE) run --rm --pull=never $(_ts_candidate) \
		dpkg-query -W -f '$${Package}=$${Version}\n' | LC_ALL=C sort > "$$actual"; \
	if [ "$(UPDATE_DEBIAN_LOCK)" = 1 ]; then \
		{ echo "# Debian packages in $(IMAGE) ($$arch), checked by 'make container'."; \
		  echo "# Regenerate with 'make container NO_CACHE=1 UPDATE_DEBIAN_LOCK=1' and review the diff."; \
		  cat "$$actual"; } > "$$lock"; \
		echo "Wrote $$lock: review the diff before committing it."; \
	elif [ ! -f "$$lock" ]; then \
		echo "error: no reviewed Debian package list for $$arch: run" >&2; \
		echo "  'make container NO_CACHE=1 UPDATE_DEBIAN_LOCK=1' to write $$lock, and review it." >&2; \
		exit 1; \
	elif ! grep -v '^#' "$$lock" | diff -u - "$$actual"; then \
		echo "error: the Debian packages in the new image differ from $$lock (diff above)." >&2; \
		echo "  If you pulled an update of that list, rebuild without the layer cache:" >&2; \
		echo "  'make container NO_CACHE=1'. If you changed the Containerfile's packages or its" >&2; \
		echo "  DEBIAN_SNAPSHOT, review the change and, if it is acceptable, run" >&2; \
		echo "  'make container NO_CACHE=1 UPDATE_DEBIAN_LOCK=1' and commit the updated list." >&2; \
		echo "  $(IMAGE) was left as it was; the new build is tagged $(_ts_candidate)." >&2; \
		exit 1; \
	fi; \
	$(ENGINE) tag $(_ts_candidate) $(IMAGE); \
	$(ENGINE) rmi $(_ts_candidate) >/dev/null; \
	echo "$(IMAGE) built from $(_ts_containerfile) (sha256 $(_ts_containerfile_sha)), $$arch."

shell:
	$(IN_CONTAINER) bash

# The container sees only the workspace, so the log must lie inside it; it
# is passed on as the same file's path under /work.
_ts_log_abs = $(abspath $(LOG))
_ts_log_in_container = $(if $(filter $(WS)/%,$(_ts_log_abs)),/work/$(patsubst $(WS)/%,%,$(_ts_log_abs)))

check-serial:
	@test -n "$(LOG)" || { echo "error: name the captured log: make check-serial SYSTEM=$(SYSTEM) LOG=FILE" >&2; exit 1; }
	@test -n "$(_ts_log_in_container)" || { echo "error: LOG=$(LOG) is outside the workspace $(WS), which is all the dev container can see: capture the log inside it, for example to $(BUILD_ROOT)/rpi5b_2gb/$(SYSTEM)/serial.log" >&2; exit 1; }
	@$(_ts_check_repo_in_ws)
	$(IN_CONTAINER) $(_ts_container_make) $@ $(_ts_forward) LOG=$(_ts_log_in_container)

# Each image is built in the container for rpi5b_2gb, then tools/pi-run runs
# them all on the board, here on the host, and checks each log with
# check-serial.
pi-run:
	@$(_ts_pi_check)
	@for s in $(PI_SYSTEMS); do \
		$(MAKE) -f $(_ts_main_mk) image BOARD=rpi5b_2gb SYSTEM=$$s MICROKIT_CONFIG=$(MICROKIT_CONFIG) || exit 1; \
	done
	$(PYTHON) $(_ts_nowayos)/tools/pi-run run --device $(PI_DEVICE) --logs $(PI_LOGS) \
		--expect-dir $(SYSTEMS_DIR) --watchdog $(PI_WATCHDOG) \
		$(foreach s,$(PI_SYSTEMS),$(s)$(if $(wildcard $(SYSTEMS_DIR)/rpi5b_2gb/$(s).expect),=rpi5b_2gb/$(s))@$(BUILD_ROOT)/rpi5b_2gb/$(s)/loader.img)

pi-uboot:
	@$(_ts_pi_check)
	@mkdir -p $(_ts_pi_dir)
	$(PYTHON) $(_ts_nowayos)/tools/pi-run uboot --device $(PI_DEVICE) --log $(_ts_pi_dir)/uboot.log "$(PI_COMMANDS)"

# One image with no watchdog, for a person to watch on the board's screen:
# it runs until someone power-cycles the board; a power cycle within PI_HOLD
# seconds is parked at U-Boot's prompt, and after that the image is left
# running. PI_TYPE=1: the person drives it from this terminal.
pi-show:
	@$(_ts_pi_check)
	$(MAKE) -f $(_ts_main_mk) image BOARD=rpi5b_2gb SYSTEM=$(SYSTEM) MICROKIT_CONFIG=$(MICROKIT_CONFIG)
	$(PYTHON) $(_ts_nowayos)/tools/pi-run show --device $(PI_DEVICE) --logs $(PI_LOGS) \
		--expect-dir $(SYSTEMS_DIR) --hold $(PI_HOLD) $(if $(filter 1,$(PI_TYPE)),--type) \
		$(SYSTEM)$(if $(wildcard $(SYSTEMS_DIR)/rpi5b_2gb/$(SYSTEM).expect),=rpi5b_2gb/$(SYSTEM))@$(BUILD_ROOT)/rpi5b_2gb/$(SYSTEM)/loader.img

# make qemu VNC=1: QEMU listens for VNC on the container's port 5900, which
# the engine publishes on the host's localhost only.
qemu: CONTAINER_RUN_ARGS += $(if $(filter 1,$(VNC)),-p 127.0.0.1:$(VNC_PORT):5900)

$(_ts_container_targets):
	@$(_ts_check_repo_in_ws)
	$(IN_CONTAINER) $(_ts_container_make) $@ $(_ts_forward)

endif

.DEFAULT_GOAL := $(_ts_saved_goal)

endif

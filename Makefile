# SPDX-License-Identifier: MIT OR Apache-2.0
# NoWayOS - developer entry points. Run `make` (or `make help`) for the list.
#
# Everything runs in the pinned dev container (tools/container/) by default:
# from the host, each target starts it itself (tools/make/container.mk and
# tools/make/system.mk), so no host Rust is needed. The everyday
# targets - fmt, lint, test, doc and clean - can run on the host instead with
# IN=host, using the toolchain pinned in rust-toolchain.toml (1.93.1), or the
# host's stable toolchain where that already is 1.93.1:
#   make test IN=host CARGO="cargo +stable"
# Nothing here installs a toolchain on the host: RUSTUP_AUTO_INSTALL=0 below
# makes rustup report a missing toolchain or target instead of downloading
# it. Written for GNU Make 3.81+.

CARGO ?= cargo
NOSTD_TARGET ?= aarch64-unknown-none
# Never let a cargo, rustc or rustup call made from here install anything.
RUSTUP_AUTO_INSTALL ?= 0
export RUSTUP_AUTO_INSTALL

# Every crate must build without std (checked by `make nostd`) except these,
# which run on the host only: a new std crate fails until it is fixed or
# listed here (the NoWayOS baseline, tools/baseline.toml).
HOST_ONLY_CRATES := nowayos-testkit
# make sbom writes a CycloneDX SBOM for each workspace here.
SBOM_DIR := build/sbom
# make publication-scan reads these repositories: the open ones.
PUBLICATION_REPOS ?= . ../screen-compiler ../showcase
# make fuzz runs each target in fuzz/fuzz_targets/ for FUZZ_SECONDS
# seconds - in make ci too - keeping its corpus and any crash in FUZZ_OUT
# (build/fuzz): its settings and its run are tools/make/system.mk's, which
# product repos share.
# Where make update-bundles writes the update tests' public keys and records.
UPDATE_BUNDLES_OUT := build/update-bundles

# The glyph atlases: tools/glyph-atlas, a host tool in a Cargo
# workspace of its own, makes them from the signed-off font in
# GLYPHS_CRATE - its glyphs.toml pins every font file - into its src/atlas/,
# where they are checked in, and make atlases-check makes them again into
# ATLAS_OUT and compares. The rasteriser and the font parsers it reads with,
# RASTERISER_CRATES, are in that workspace's Cargo.lock alone: atlases-check
# fails if cargo tree finds one in any other workspace.
GLYPHS_CRATE := crates/nowayos-glyphs
ATLAS_TOOL := tools/glyph-atlas
ATLAS_OUT := build/atlases
RASTERISER_CRATES := swash skrifa read-fonts font-types zeno yazi
ATLAS_TARGET_DIR = $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR)-atlas,$(CURDIR)/$(ATLAS_TOOL)/target)
ATLAS_RUN = $(CARGO) run --locked --quiet --manifest-path $(ATLAS_TOOL)/Cargo.toml --target-dir $(ATLAS_TARGET_DIR) --
# The icons' alpha masks: tools/icon-masks, a Python tool, draws the
# Lucide icons ICONS_CRATE's icons.toml pins into its src/masks/; make atlases
# writes them with the atlases, and make atlases-check draws them again into
# ICONS_OUT and compares.
ICONS_CRATE := crates/nowayos-icons
ICONS_OUT := build/icon-masks
# The pictures: tools/pictures, a host tool in a Cargo workspace of its own,
# converts the PNG and JPEG files each crate in PICTURE_CRATES pins in its
# pictures.toml into the module folder it names, where they are checked in;
# make atlases writes them, and make atlases-check makes them again into
# PICTURES_OUT and compares. The JPEG decoder, JPEG_CRATES, is in that
# workspace's Cargo.lock alone, and the PNG decoder, png, besides only in
# the root workspace's host-only test kit, never the PDs' or the fuzz
# harness's: atlases-check fails if cargo tree finds one elsewhere.
PICTURE_TOOL := tools/pictures
PICTURE_CRATES := crates/nowayos-pictures
PICTURES_OUT := build/pictures
JPEG_CRATES := zune-jpeg zune-core
PICTURE_TARGET_DIR = $(if $(CARGO_TARGET_DIR),$(CARGO_TARGET_DIR)-pictures,$(CURDIR)/$(PICTURE_TOOL)/target)
PICTURE_RUN = $(CARGO) run --locked --quiet --manifest-path $(PICTURE_TOOL)/Cargo.toml --target-dir $(PICTURE_TARGET_DIR) --

-include tools/make/container.mk
-include tools/make/system.mk

.DEFAULT_GOAL := help

.PHONY: help fmt lint test doc demo atlases nostd deny sbom secrets fuzz fuzz-skip fuzz-cached kani atlases-check publication-scan update-bundles ci clean

help:
	@echo "NoWayOS - make targets"
	@echo ""
	@echo "Everyday targets - in the dev container, or on the host with IN=host"
	@echo "(and CARGO=\"cargo +stable\" to use an installed stable 1.93.1):"
	@echo "  make fmt               format all Rust code"
	@echo "  make lint              rustfmt check, then clippy with warnings as errors"
	@echo "  make test              unit, property, doc and snapshot tests"
	@echo "                         (NOWAYOS_UPDATE_SNAPSHOTS=1 rewrites golden images)"
	@echo "  make doc               build the API docs, warnings as errors"
	@echo "  make demo              the demo's scripted session, every frame checked"
	@echo "                         (NOWAYOS_UPDATE_SNAPSHOTS=1 rewrites its golden images)"
	@echo "  make atlases           the glyph atlases, made again from the pinned font into $(GLYPHS_CRATE)/src/atlas/,"
	@echo "                         the icons' masks, from the pinned Lucide icons into $(ICONS_CRATE)/src/masks/,"
	@echo "                         and the pictures of $(PICTURE_CRATES), from the files each pictures.toml pins"
	@echo "                         ($(ATLAS_TOOL), $(PICTURE_TOOL)); review the change and commit it"
	@echo "  make clean             remove the target directory (the container's, or the host's with IN=host)"
	@echo ""
	@echo "In the dev container only (these start it themselves; ENGINE=podman|docker):"
	@echo "  make container         build the dev image (host only; once, and after Containerfile changes)"
	@echo "  make shell             interactive shell in the container, workspace at /work"
	@echo "  make nostd             build every crate but $(HOST_ONLY_CRATES) for $(NOSTD_TARGET)"
	@echo "  make deny              licences, sources, bans and advisories (cargo-deny)"
	@echo "  make sbom              a CycloneDX SBOM for each workspace: $(SBOM_DIR)/nowayos.cdx.json, nowayos-pds.cdx.json,"
	@echo "                         nowayos-fuzz.cdx.json, nowayos-glyph-atlas.cdx.json and nowayos-pictures-tool.cdx.json"
	@echo "  make secrets           scan the tree and every commit for keys and credentials, offline"
	@echo "                         (tools/secrets-scan; reviewed false positives in secrets-allow.toml)"
	@echo "  make kani              Kani's proofs of the record decoders, the network queues' checks and the fault"
	@echo "                         policy's rule (src/proofs.rs in each crate of KANI_CRATES), then mutants each"
	@echo "                         with a check removed, which must fail (KANI_MUTATIONS=NAME for some)"
	@echo "  make atlases-check     the atlas tool's rustfmt, clippy and tests; the atlases made again into $(ATLAS_OUT)/"
	@echo "                         and compared with the checked-in ones, the icons' masks and the pictures likewise"
	@echo "                         (with the picture tool's rustfmt, clippy and tests); and no rasteriser or image"
	@echo "                         decoder crate in any other workspace's cargo tree"
	@echo "  make fuzz              the fuzz targets (fuzz/): rustfmt and clippy, then each built with libFuzzer"
	@echo "                         and run for FUZZ_SECONDS ($(FUZZ_SECONDS)) s; corpus and crashes in $(FUZZ_OUT)/"
	@echo "                         (FUZZ_TARGETS=NAME for one; FUZZ_INPUT=FILE runs one target on one input)"
	@echo "  make publication-scan TERMS=FILE  maintainers, before publishing: the open repositories' trees and"
	@echo "                         histories for the terms in FILE (a private list kept outside every"
	@echo "                         repository), private links, assistant files, wiki references and identities"
	@echo "                         (tools/publication-scan; not in make ci)"
	@echo "  make update-bundles    throwaway test keys, made and deleted in the container's /tmp, and the"
	@echo "                         signed trust lists, manifests and bundles the update tests read, in"
	@echo "                         $(UPDATE_BUNDLES_OUT)/ (tools/update-bundles; only public keys and records)"
	@echo "  make ci                the full CI (container-ci): everything below, in order"
	@echo "  make release-evidence  the release evidence pack for the commit checked out (refuses uncommitted"
	@echo "                         changes): make ci step by step, every image rebuilt, SBOMs, versions,"
	@echo "                         grants inventory, crypto crates and INDEX.md in build/release-evidence/COMMIT/"
	@echo ""
	@echo "  make tools-test        tests for the tools in tools/"
	@echo "  make sel4-config-check the seL4 verification position is for the pinned Microkit SDK"
	@echo "  make unsafe-audit      unsafe code only in the approved crates (tools/unsafe-allowlist.toml)"
	@echo "  make pds-lint          rustfmt and clippy for the protection-domain workspace (pds/)"
	@echo "  make pds-deny          cargo-deny for pds/"
	@echo "  make pds-test          tests and clippy for the pds library crates"
	@echo "  make pds-miri          Miri over the approved-unsafe pds crates"
	@echo "  make pds [SYSTEM=hello]        build a system's protection domains"
	@echo "  make image [SYSTEM=hello]      also make its Microkit image, check its grants and record"
	@echo "                                 its seL4 configuration (sel4-config.txt) and SBOM (sbom.cdx.json) beside it"
	@echo "  make image-sbom [SYSTEM=hello] make image, then name its SBOM: the kernel, loader, monitor, capDL"
	@echo "                                 initialiser, every PD and its crates, with sha256s (tools/image-sbom)"
	@echo "  make qemu [SYSTEM=hello]       boot it in QEMU; quit with Ctrl-A, then x"
	@echo "  make qemu-test [SYSTEM=hello]  boot it headless and check its output"
	@echo "  make qemu-test-all     qemu-test every systems/*.system"
	@echo "  make check-serial SYSTEM=hello LOG=FILE  check a board's captured serial log the same way"
	@echo "  make pi-run PI_DEVICE=/dev/cu.usbmodemXXXX PI_SYSTEMS=\"hello ipc-demo\"  on the host: build them for the Pi 5,"
	@echo "                         run each on the board over the Debug Probe, unattended, and check each log"
	@echo "                         (tools/pi-run; the PM watchdog resets the board after PI_WATCHDOG s; logs in"
	@echo "                         build/rpi5b_2gb/pi-run/; the board is left at U-Boot's prompt; a system's .qmp of"
	@echo "                         wait, pause and serial steps typed through the probe)"
	@echo "  make pi-uboot PI_DEVICE=... PI_COMMANDS=\"bdinfo; md.l 107d20001c 3\"  on the host: U-Boot commands"
	@echo "                         that read, at the parked board's prompt"
	@echo "  make pi-show PI_DEVICE=... SYSTEM=ui-demo  on the host: run it with no watchdog, for a person to watch,"
	@echo "                         until they power-cycle the board; parked if that is within PI_HOLD s, else left running;"
	@echo "                         PI_TYPE=1 to drive it from this terminal: keys to the debug UART (SYSTEM=uart-input-test)"
	@echo "  make std-crates-check  standard-library crates in the PDs match pds/std-crates.txt"
	@echo "  make screen-compiler-check   Screen Compiler's generated screens against this nowayos-ui, drawn as approved (needs ../screen-compiler; else a notice)"
	@echo "  make uboot             U-Boot for the Pi 5, reproducibly: build/rpi5b_2gb/u-boot/u-boot.bin"
	@echo "  make sdcard BOARD=rpi5b_2gb [SYSTEM=hello]  the Pi 5 SD card's boot files, with SHA256SUMS"
	@echo "  make sdk-boards        boards and configurations in the pinned Microkit SDK"
	@echo "  make system-clean      remove build/ and the PD build output"
	@echo ""
	@echo "More detail on the container and system targets: make system-help."
	@echo "Guides: https://www.nowayos.com/docs; the API: make doc."
	@echo "Current settings: IN=\"$(IN)\" CARGO=\"$(CARGO)\" ENGINE=\"$(ENGINE)\" IMAGE=\"$(IMAGE)\""

# The everyday targets run here inside the dev container or with IN=host;
# otherwise they start the container (tools/make/container.mk).
ifneq ($(NOWAYOS_CONTAINER)$(filter host,$(IN)),)
fmt:
	$(CARGO) fmt --all
	$(CARGO) fmt --all --manifest-path fuzz/Cargo.toml
	$(CARGO) fmt --all --manifest-path $(ATLAS_TOOL)/Cargo.toml

lint:
	$(CARGO) fmt --all -- --check
	$(CARGO) clippy --workspace --all-targets --locked -- -D warnings

test:
	$(CARGO) test --workspace --locked

doc:
	RUSTDOCFLAGS="-D warnings" $(CARGO) doc --workspace --no-deps --locked

# The clickable demo in host simulation (crates/nowayos-demo): its scripted
# session, each frame checked against crates/nowayos-demo/session/*.png.
demo:
	$(CARGO) run --locked --quiet -p nowayos-demo --example session

# The glyph atlases made again from the pinned font, into the atlas
# crate: the tool refuses a font that differs from its pin, an unpinned file
# in fonts/ or a character the font lacks, and then writes nothing. The same
# inputs give the same bytes, on the host or in the container.
atlases:
	$(ATLAS_RUN) $(GLYPHS_CRATE)
	python3 tools/icon-masks $(ICONS_CRATE)
	@for crate in $(PICTURE_CRATES); do $(PICTURE_RUN) $$crate || exit 1; done

clean:
	$(CARGO) clean
else ifdef IN_CONTAINER
fmt lint test doc demo atlases clean:
	$(IN_CONTAINER) make $@ CARGO=cargo $(if $(NOWAYOS_UPDATE_SNAPSHOTS),NOWAYOS_UPDATE_SNAPSHOTS=$(NOWAYOS_UPDATE_SNAPSHOTS))
else
fmt lint test doc demo atlases clean:
	@echo "error: make $@ needs tools/make/container.mk, or IN=host to run on the host" >&2
	@exit 1
endif

# nostd needs the aarch64-unknown-none target and deny needs cargo-deny; both
# are in the dev container, not on the host. So are sbom and secrets, which
# need its Python (3.11, for tomllib), fuzz, which compiles libFuzzer with
# its C++ compiler, and atlases-check, which needs the PDs' nightly for their
# cargo tree.
ifeq ($(NOWAYOS_CONTAINER),1)
# One crate at a time, so one crate's features cannot rescue another's.
nostd:
	@set -e; crates=$$($(CARGO) metadata --locked --no-deps --format-version 1 | \
		python3 -c 'import json, sys; print(" ".join(sorted(p["name"] for p in json.load(sys.stdin)["packages"])))'); \
	for crate in $(HOST_ONLY_CRATES); do \
		case " $$crates " in *" $$crate "*) ;; *) echo "error: HOST_ONLY_CRATES names $$crate, which is not a workspace member" >&2; exit 1 ;; esac; \
	done; \
	for crate in $$crates; do \
		case " $(HOST_ONLY_CRATES) " in *" $$crate "*) echo "==> $$crate: host only, skipped"; continue ;; esac; \
		echo "==> $$crate ($(NOSTD_TARGET))"; \
		$(CARGO) build --locked -p $$crate --target $(NOSTD_TARGET); \
	done

# The NoWayOS baseline first: lints, profiles and deny settings of all
# four workspaces (tools/check-baseline), then cargo-deny itself, for the
# root workspace, the fuzz harness's and the atlas tool's (pds/: make
# pds-deny).
deny:
	python3 tools/check-baseline . pds fuzz $(ATLAS_TOOL) $(PICTURE_TOOL)
	$(CARGO) deny --locked check
	$(CARGO) deny --locked --manifest-path fuzz/Cargo.toml --config fuzz/deny.toml check
	$(CARGO) deny --locked --manifest-path $(ATLAS_TOOL)/Cargo.toml --config $(ATLAS_TOOL)/deny.toml check
	$(CARGO) deny --locked --manifest-path $(PICTURE_TOOL)/Cargo.toml --config $(PICTURE_TOOL)/deny.toml check

# A CycloneDX SBOM for each workspace (tools/sbom). What a workspace's
# product is built from is `required`; dev-dependencies, build-time tools,
# host-only crates and other platforms' dependencies are marked `excluded`.
# The PDs' SBOM also lists the standard-library crates build-std compiles
# into them (pds/std-crates.txt). The fuzz harness is a test harness: nothing
# in it ships, so everything in its SBOM is `excluded`. The atlas tool runs
# on the build host only: its SBOM lists what it is built from as `required`
# for the tool, which ships nothing but the atlases it writes. Two runs on
# one commit give the same bytes.
sbom:
	@mkdir -p $(SBOM_DIR) && { test -f build/.gitignore || printf '*\n' > build/.gitignore; }
	python3 tools/sbom --workspace . --name nowayos --target $(NOSTD_TARGET) \
		$(addprefix --host-only ,$(HOST_ONLY_CRATES)) --out $(SBOM_DIR)/nowayos.cdx.json
	python3 tools/sbom --workspace pds --name nowayos-pds --kind application --target aarch64-sel4 \
		--std-crates pds/std-crates.txt --toolchain $(PDS_TOOLCHAIN) \
		--std-library "$$(rustc +$(PDS_TOOLCHAIN) --print sysroot)/lib/rustlib/src/rust/library" \
		--out $(SBOM_DIR)/nowayos-pds.cdx.json
	python3 tools/sbom --workspace fuzz --name nowayos-fuzz --host-only-workspace \
		--out $(SBOM_DIR)/nowayos-fuzz.cdx.json
	python3 tools/sbom --workspace $(ATLAS_TOOL) --name nowayos-glyph-atlas \
		--out $(SBOM_DIR)/nowayos-glyph-atlas.cdx.json
	python3 tools/sbom --workspace $(PICTURE_TOOL) --name nowayos-pictures-tool \
		--out $(SBOM_DIR)/nowayos-pictures-tool.cdx.json

# The fuzz harness (fuzz/): fuzz-run (tools/make/system.mk) runs
# FUZZ_TARGETS, each for FUZZ_SECONDS seconds; libFuzzer itself is signed off
# for development and CI fuzzing only.
fuzz-skip:
	@echo "fuzz: SKIP - no fuzz target left to run: none reaches a change, or each passed before with its inputs $(_ci_scope_note)"


# Kani's proofs: each crate in KANI_CRATES has its proofs in
# src/proofs.rs, compiled only under cfg(kani). They show that the record
# decoders - nowayos-ipc's header, decode and decode_exact, the input event,
# the display contract's records, the clock's sample and reading, the socket
# records, the seed record and the update records - never panic,
# overflow or read out of bounds for any input up to the longest record, and
# keep their rules: all but the update records every rule; the manifest and a
# bundle's chunk the rules on their lengths, characters and offset; the
# trust list none (its rules, too big for the dev container's memory, and the
# update records' one encoding are left to property tests and fuzzing); that
# the terminal decoder (nowayos-input), given any six bytes and then the
# end of input, never panics or overflows, makes only the keys it sends and is
# left between keys; that the fault policy's rule (nowayos-fault-policy's decide) never panics
# or overflows for any input and never answers restart once either of its
# limits is reached, and its reader of a test system's stand-in counts
# reads only what tools/board-system writes; and that wall-clock
# time's interval arithmetic never overflows or panics, that an interval's
# earliest never goes back, and
# by exactly how much it and the latest move, for any drift with the stated
# properties - the real drift checked against them below 2^32 ns
# (nowayos-time); that an interval never narrows follows from those
# amounts and rests on them and the property tests. Kani
# 0.68.0 is in the dev image; its CBMC is signed off for development use only.
# Each harness's verification time is printed and every log kept in
# KANI_OUT. nowayos-net's proofs are of the checks each end of a
# network queue makes on its peer's writes: a descriptor decodes from any
# bytes, DataRegion accepts only a buffer inside the region, on a boundary
# and no longer than a buffer, and neither end reads or writes a descriptor
# when its peer's index puts more than the capacity in flight.
# nowayos-usb's proofs are of the video frame ring's checks
# between the USB host PD and its client - a layout the client takes fits
# its region, a slot's record is taken only for its frame and within its
# slot, and neither side takes a count that puts more frames in flight than
# slots - and of the payload header's decoder. Then
# tools/kani-mutation shows the proofs can fail: in a copy of the workspace
# with one check removed - nowayos-ipc's Reader::read_bytes indexing instead
# of checking its length (read-bytes-length), DataRegion::buffer's bounds
# check (data-region-bounds) or dequeue's in-flight check (dequeue-in-flight)
# - a proof must fail; KANI_MUTATIONS names some, every one by default.
KANI_CRATES := nowayos-ipc nowayos-input nowayos-display nowayos-fault-policy nowayos-time nowayos-update nowayos-socket nowayos-seed nowayos-net nowayos-signing nowayos-audio nowayos-usb
KANI_MUTATIONS ?=
KANI_OUT := build/kani

# make ci passes CI_SCOPE=changed: Kani's crates and mutants and the fuzz
# targets are then cut to those a change since CI_BASE can reach
# (tools/ci-scope: committed since it, staged, unstaged or untracked, through
# every NoWayOS crate that depends on what changed; a change to the toolchain,
# a lock file, a manifest, the Makefile or the container reaches them all).
# make ci CI_SCOPE=full, make kani, make fuzz and release-evidence run every one.
CI_BASE ?= origin/main
ifeq ($(CI_SCOPE),changed)
override KANI_CRATES := $(shell python3 tools/ci-scope --base $(CI_BASE) kani $(KANI_CRATES))
override KANI_MUTATIONS := $(shell python3 tools/ci-scope --base $(CI_BASE) mutations $(KANI_MUTATIONS))
override FUZZ_TARGETS := $(shell python3 tools/ci-scope --base $(CI_BASE) fuzz $(FUZZ_TARGETS))
_ci_scope_note = (CI_SCOPE=changed, against $(CI_BASE); CI_SCOPE=full runs them all)
# In the container, the crates and targets a change reaches are cut again to
# those with no verdict for their inputs as they are now
# (tools/ci-verdicts, CI_VERDICTS in tools/make/system.mk).
ifeq ($(NOWAYOS_CONTAINER),1)
_kani_reached := $(KANI_CRATES)
override KANI_CRATES := $(shell $(_ts_verdicts) keep kani $(KANI_CRATES) $(_ts_kani_salt))
_kani_cached := $(filter-out $(KANI_CRATES),$(_kani_reached))
_fuzz_reached := $(FUZZ_TARGETS)
override FUZZ_TARGETS := $(shell $(_ts_verdicts) keep fuzz $(FUZZ_TARGETS) $(_ts_fuzz_salt))
_fuzz_cached := $(filter-out $(FUZZ_TARGETS),$(_fuzz_reached))
endif
_ci_skip_fuzz := $(if $(strip $(FUZZ_TARGETS)),,1)
_ci_skip_mutants := $(if $(strip $(KANI_MUTATIONS)),,1)
endif
_ci_cached_note = passed before with exactly these inputs (CI_SCOPE=full runs it again)

# Read after CI_SCOPE has cut FUZZ_TARGETS: with none left, fuzz says so.
fuzz: $(if $(_fuzz_cached),fuzz-cached) $(if $(_ci_skip_fuzz),fuzz-skip,fuzz-run)
fuzz-cached:
	@for target in $(_fuzz_cached); do echo "fuzz: CACHED - $$target: $(_ci_cached_note)"; done
kani:
	@mkdir -p $(KANI_OUT) && { test -f build/.gitignore || printf '*\n' > build/.gitignore; }
	$(if $(_ci_scope_note),@echo "kani: proving [$(strip $(KANI_CRATES))] $(_ci_scope_note)")
	@for crate in $(_kani_cached); do echo "kani: CACHED - $$crate: $(_ci_cached_note)"; done
	@crates="$(strip $(KANI_CRATES))"; \
	if [ -n "$$crates" ]; then \
		log=$(KANI_OUT)/kani.log; \
		start=$$(date +%s); \
		echo "==> $$crates, in one pool"; \
		if $(CARGO) kani $(addprefix -p ,$(KANI_CRATES)) $(_ts_kani_jobs) > $$log 2>&1; then \
			sed -n -e 's/^\(Thread [0-9]*: \)\{0,1\}Checking harness \(.*\)\.\.\.$$/    \2/p' $$log; \
			echo "kani: PASS - $$crates: $$(sed -n 's/^Complete - //p' $$log) in $$(( $$(date +%s) - start )) s"; \
			for crate in $$crates; do $(_ts_verdicts) record kani $$crate $(_ts_kani_salt) || exit 1; done; \
		else \
			tail -n 40 $$log >&2; \
			echo "kani: FAIL - $$crates: the log is in $$log; no verdict recorded" >&2; \
			exit 1; \
		fi; \
	fi
	$(if $(_ci_skip_mutants),@echo "kani-mutation: SKIP - no mutant's crate reaches a change $(_ci_scope_note)",python3 tools/kani-mutation --out $(KANI_OUT) $(addprefix --mutation ,$(KANI_MUTATIONS)))

# The glyph atlases checked: the tool's rustfmt, clippy and tests
# (the pins, an unpinned font file, a character the font lacks), then the
# atlases made again from the pinned font into ATLAS_OUT and compared, file
# by file, with the checked-in ones; then cargo tree over the other three
# workspaces - the root's no_std crates, the PDs and the fuzz harness - for
# RASTERISER_CRATES, which must be in none of them (tools/check-crates-absent:
# a cargo tree that fails, or lists no crate, checks nothing and fails too).
atlases-check:
	cd $(ATLAS_TOOL) && $(CARGO) fmt --all -- --check
	cd $(ATLAS_TOOL) && $(CARGO) clippy --locked --all-targets --target-dir $(ATLAS_TARGET_DIR) -- -D warnings
	cd $(ATLAS_TOOL) && $(CARGO) test --locked --target-dir $(ATLAS_TARGET_DIR)
	@rm -rf $(ATLAS_OUT) && mkdir -p build && { test -f build/.gitignore || printf '*\n' > build/.gitignore; }
	$(ATLAS_RUN) --out $(ATLAS_OUT) $(GLYPHS_CRATE)
	@diff -r $(GLYPHS_CRATE)/src/atlas $(ATLAS_OUT) || { echo "atlases-check: FAIL - the checked-in atlases in" \
		"$(GLYPHS_CRATE)/src/atlas differ from what the pinned font gives (above): run make atlases, review the change" \
		"and commit it" >&2; exit 1; }
	@for workspace in . pds fuzz; do \
		case $$workspace in pds) cargo="$(CARGO) +$(PDS_TOOLCHAIN)" ;; *) cargo="$(CARGO)" ;; esac; \
		python3 tools/check-crates-absent --what "the cargo tree of $$workspace" \
			$(addprefix --forbid ,$(RASTERISER_CRATES)) \
			--reason "the rasteriser and its font parsers belong to $(ATLAS_TOOL) alone, never to a device crate" \
			-- $$cargo tree --locked --manifest-path $$workspace/Cargo.toml --workspace --target all \
			--edges normal,build,dev --prefix none --format '{p}' || exit 1; \
	done
	@rm -rf $(ICONS_OUT)
	python3 tools/icon-masks --out $(ICONS_OUT) $(ICONS_CRATE)
	@diff -r $(ICONS_CRATE)/src/masks $(ICONS_OUT) || { echo "atlases-check: FAIL - the checked-in icon masks in" \
		"$(ICONS_CRATE)/src/masks differ from what the pinned icons give (above): run make atlases, review the change" \
		"and commit it" >&2; exit 1; }
	cd $(PICTURE_TOOL) && $(CARGO) fmt --all -- --check
	cd $(PICTURE_TOOL) && $(CARGO) clippy --locked --all-targets --target-dir $(PICTURE_TARGET_DIR) -- -D warnings
	cd $(PICTURE_TOOL) && $(CARGO) test --locked --target-dir $(PICTURE_TARGET_DIR)
	@rm -rf $(PICTURES_OUT)
	@for crate in $(PICTURE_CRATES); do \
		module=$$(sed -n 's/^module = "\(.*\)"$$/\1/p' $$crate/pictures.toml); \
		$(PICTURE_RUN) --out $(PICTURES_OUT)/$$crate $$crate || exit 1; \
		diff -r $$crate/$$module $(PICTURES_OUT)/$$crate || { echo "atlases-check: FAIL - the checked-in pictures in" \
			"$$crate/$$module differ from what its pinned files give (above): run make atlases, review the change" \
			"and commit it" >&2; exit 1; }; \
	done
	@for workspace in . pds fuzz; do \
		case $$workspace in pds) cargo="$(CARGO) +$(PDS_TOOLCHAIN)" ;; *) cargo="$(CARGO)" ;; esac; \
		case $$workspace in .) decoders="$(JPEG_CRATES)" ;; *) decoders="$(JPEG_CRATES) png" ;; esac; \
		python3 tools/check-crates-absent --what "the cargo tree of $$workspace" \
			$$(for crate in $$decoders; do printf ' --forbid %s' $$crate; done) \
			--reason "the image decoders belong to $(PICTURE_TOOL) (and png to the root's test kit) alone, never to a device crate" \
			-- $$cargo tree --locked --manifest-path $$workspace/Cargo.toml --workspace --target all \
			--edges normal,build,dev --prefix none --format '{p}' || exit 1; \
	done
	@echo "atlases-check: PASS - $(GLYPHS_CRATE)/src/atlas is what the pinned font gives, $(ICONS_CRATE)/src/masks" \
		"what the pinned icons give, $(PICTURE_CRATES)' pictures what their pinned files give, and no workspace but" \
		"$(ATLAS_TOOL) has the rasteriser in its cargo tree, none but $(PICTURE_TOOL) the JPEG decoder"

# The secrets scan (tools/secrets-scan): the tree, and every
# version of every file and every commit message on every ref, for keys and
# credentials, offline. A finding names its file and line, never the text;
# reviewed false positives go in secrets-allow.toml, one exact entry each with
# its reason. It needs the whole history: a shallow clone fails.
secrets:
	python3 tools/secrets-scan --repo . --allow secrets-allow.toml

# Throwaway test keys and the signed records the update tests read
# (tools/update-bundles): openssl makes the key pairs
# in the container's /tmp, deleted when the run ends, so no private key ever
# reaches the workspace; only public keys and signed records reach
# UPDATE_BUNDLES_OUT.
update-bundles:
	python3 tools/update-bundles --out $(UPDATE_BUNDLES_OUT)

# The pre-publication scan (tools/publication-scan): each of
# PUBLICATION_REPOS - its tree, every version of every file, every commit
# message, identity, ref and tag - for the terms in TERMS, a list kept
# outside every repository, and for links into Azure DevOps, assistant
# instruction files and wiki references, with every identity. A finding names
# where it is and the term's line in TERMS, never the term. Not part of make
# ci: the term list is in no repository.
publication-scan:
	@test -n "$(TERMS)" || { echo "error: name the term list: make publication-scan TERMS=FILE (outside every repository)" >&2; exit 1; }
	python3 tools/publication-scan --terms $(TERMS) $(PUBLICATION_REPOS)
else ifdef IN_CONTAINER
# From the host the scans run with no network at all.
secrets publication-scan: override CONTAINER_RUN_ARGS += --network=none
nostd deny sbom secrets atlases-check update-bundles:
	$(IN_CONTAINER) make $@ CARGO=cargo
fuzz:
	$(IN_CONTAINER) make $@ CARGO=cargo FUZZ_SECONDS=$(FUZZ_SECONDS) FUZZ_TARGETS="$(FUZZ_TARGETS)" \
		$(if $(FUZZ_INPUT),FUZZ_INPUT=$(FUZZ_INPUT))
kani:
	$(IN_CONTAINER) make $@ CARGO=cargo $(if $(KANI_CRATES),KANI_CRATES="$(KANI_CRATES)") \
		$(if $(KANI_MUTATIONS),KANI_MUTATIONS="$(KANI_MUTATIONS)")
# The container sees only the workspace, so the term list must lie in it,
# outside every repository: in the workspace folder beside them.
_terms_abs = $(abspath $(TERMS))
_terms_in_container = $(if $(filter $(WS)/%,$(_terms_abs)),/work/$(patsubst $(WS)/%,%,$(_terms_abs)))
publication-scan:
	@test -n "$(TERMS)" || { echo "error: name the term list: make publication-scan TERMS=FILE (outside every repository, inside $(WS))" >&2; exit 1; }
	@test -n "$(_terms_in_container)" || { echo "error: TERMS=$(TERMS) is outside the workspace $(WS), which is all the dev container can see: keep the list there, beside the repositories and outside every one of them" >&2; exit 1; }
	$(IN_CONTAINER) make $@ CARGO=cargo TERMS=$(_terms_in_container) PUBLICATION_REPOS="$(PUBLICATION_REPOS)"
else
nostd deny sbom secrets fuzz kani atlases-check publication-scan update-bundles:
	@echo "error: make $@ runs in the dev container and needs tools/make/container.mk" >&2
	@exit 1
endif

ci: container-ci

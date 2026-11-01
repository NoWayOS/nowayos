# SPDX-License-Identifier: MIT OR Apache-2.0
# container.mk: run make targets inside the NoWayOS dev container.
#
# Shared by all four repos. Include it with -include, so that a missing file
# never breaks host-only targets:
#
#   nowayos:      -include tools/make/container.mk
#   other repos:  -include ../nowayos/tools/make/container.mk
#
# Then run any target in the container with:
#
#   $(IN_CONTAINER) make TARGET CARGO=cargo
#
# Where the everyday targets run - fmt, lint, test, doc, clean and
# screen-compiler's fixtures, bless, run and no-nowayos - is set by one variable,
# defined here:
#
#   IN=container  in the dev container: the default, so no host Rust is needed
#   IN=host       on the host, with the toolchain rust-toolchain.toml pins; a
#                 missing toolchain is reported, never installed (each
#                 Makefile exports RUSTUP_AUTO_INSTALL=0). Where the host's
#                 stable toolchain is that version: IN=host CARGO="cargo +stable"
#
# Inside the container (NOWAYOS_CONTAINER=1) they always run directly.
# Each Makefile runs a target here when
#   $(NOWAYOS_CONTAINER)$(filter host,$(IN))
# is non-empty, and otherwise passes it to $(IN_CONTAINER).
#
# The workspace (the folder holding nowayos/ and the repos beside it) is
# bind-mounted at /work, and the command runs in /work/REPO
# (the directory make was started in). Cargo's registry/git caches and target
# directories live in named volumes, never on the bind mount, so host and
# container builds never share a target directory. Files written to the bind
# mount belong to the host user. The container gets no Linux capabilities and
# cannot gain privileges (no-new-privileges).
#
# Build the image once with `make container` in nowayos/. The image carries
# the sha256 of the Containerfile it was built from, and every run checks it:
# after a pull that changes tools/container/Containerfile, runs stop with
# "rebuild it with 'make container'" instead of using a stale toolchain.
#
# Variables (override on the command line, e.g. `make ci ENGINE=docker`):
#   ENGINE               podman (default) or docker
#   IMAGE                dev container image (nowayos-dev:0.1)
#   WS                   workspace root, bind-mounted at /work
#   CARGO_VOLUME         named volume for CARGO_HOME (registry, git, advisory DB)
#   TARGET_VOLUME        named volume for cargo target directories
#   CONTAINER_BIND_OPTS  extra bind-mount options, e.g. :z on SELinux Linux hosts
#   CONTAINER_HARDENING  security options for `$(ENGINE) run`
#   CONTAINER_RUN_ARGS   extra arguments for `$(ENGINE) run`
#
# Written for GNU Make 3.81 (the version macOS ships) and later.

ifndef NOWAYOS_CONTAINER_MK
NOWAYOS_CONTAINER_MK := 1

_ts_container_mk_dir := $(dir $(lastword $(MAKEFILE_LIST)))

IN ?= container
ifeq ($(filter container host,$(IN)),)
$(error IN=$(IN): use IN=container (the default) or IN=host)
endif

ENGINE ?= podman
IMAGE ?= nowayos-dev:0.1
WS ?= $(abspath $(_ts_container_mk_dir)../../..)
CARGO_VOLUME ?= nowayos-cargo
TARGET_VOLUME ?= nowayos-target
CONTAINER_BIND_OPTS ?=
CONTAINER_HARDENING ?= --cap-drop=ALL --security-opt=no-new-privileges
CONTAINER_RUN_ARGS ?=

# The Containerfile the image must have been built from, and the label that
# records its sha256 (set by `make container` in nowayos/tools/make/system.mk).
NOWAYOS_IMAGE_LABEL := nowayos.containerfile.sha256
_ts_containerfile := $(abspath $(_ts_container_mk_dir)../container/Containerfile)
_ts_containerfile_sha := $(firstword $(shell (sha256sum '$(_ts_containerfile)' 2>/dev/null || shasum -a 256 '$(_ts_containerfile)') 2>/dev/null))

# Where make was started, relative to the workspace (e.g. "screen-compiler").
_ts_ws_rel = $(patsubst $(WS)/%,%,$(filter $(WS)/%,$(CURDIR)))
_ts_repo = $(firstword $(subst /, ,$(_ts_ws_rel)))

CONTAINER_WORKDIR ?= $(if $(_ts_ws_rel),/work/$(_ts_ws_rel),/work)
CONTAINER_TARGET_DIR ?= /cache/target/$(if $(_ts_repo),$(_ts_repo),workspace)

# Run as the host user, so files written to the bind mount are the host
# user's. Rootless podman: --userns=keep-id maps the host user in. Rootful
# podman (the default podman machine on macOS may be rootful) and docker: run
# as the host UID:GID.
_ts_podman_rootless = $(shell $(ENGINE) info --format '{{.Host.Security.Rootless}}' 2>/dev/null)
CONTAINER_USER_ARGS ?= $(if $(and $(findstring podman,$(notdir $(ENGINE))),$(filter true,$(_ts_podman_rootless))),--userns=keep-id,--user $(shell id -u):$(shell id -g))

# Allocate a terminal only when make's own stdin and stdout are one (an
# interactive `make shell` or `make qemu`). Tested in the recipe's shell:
# $(shell ...) always runs with its stdout on a pipe, so it cannot see one.
_ts_tty_test = if [ -t 0 ] && [ -t 1 ]; then _ts_tty=-t; else _ts_tty=; fi

# The image exists, and was built from the current Containerfile.
_ts_image_check = { _ts_label=$$($(ENGINE) image inspect --format '{{index .Config.Labels "$(NOWAYOS_IMAGE_LABEL)"}}' $(IMAGE) 2>/dev/null) || \
	{ echo "error: $(ENGINE) cannot find image $(IMAGE): is $(ENGINE) running? If it is, build the image with 'make container' in nowayos/" >&2; exit 1; }; \
	[ "$$_ts_label" = "$(_ts_containerfile_sha)" ] || \
	{ echo "error: $(IMAGE) was not built from the current $(_ts_containerfile) (image label: '$$_ts_label'; file sha256: $(_ts_containerfile_sha)): rebuild it with 'make container' in nowayos/" >&2; exit 1; }; }

# --init: a minimal init as PID 1 forwards signals and reaps processes (QEMU).
IN_CONTAINER = $(_ts_image_check) && $(_ts_tty_test) && \
	$(ENGINE) run --rm --init -i $$_ts_tty --pull=never \
	$(CONTAINER_USER_ARGS) \
	$(CONTAINER_HARDENING) \
	-e HOME=/tmp \
	-e CARGO_TARGET_DIR=$(CONTAINER_TARGET_DIR) \
	-v $(WS):/work$(CONTAINER_BIND_OPTS) \
	-v $(CARGO_VOLUME):/cache/cargo \
	-v $(TARGET_VOLUME):/cache/target \
	-w $(CONTAINER_WORKDIR) \
	$(CONTAINER_RUN_ARGS) \
	$(IMAGE)

endif

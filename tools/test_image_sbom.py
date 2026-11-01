# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/image-sbom: the CycloneDX 1.5 SBOM `make image` writes
for each system image. A synthetic Microkit SDK, build folder,
Containerfile and cargo output stand in for the real ones; the components
file is the real tools/image-components.toml, with the real audit of the
capDL initialiser (tools/initialiser-audit.toml), unless a test says
otherwise. The SBOM must list the kernel, loader, monitor and capDL
initialiser, every program image and the crates compiled into it, each with
its sha256, and the image's own sha256; flag a GPL part as needing a source
offer; hold the crates the initialiser's audit found inside it, marked as
the audit stands - and mark it 'composition unaudited', claiming nothing the
audit found, when the pinned SDK is not the one audited; list U-Boot and the
firmware for an SD-card board; list the glyph atlases' notice beside every
image, naming the font the atlases were made from, and name it on the atlas
crate's component too; be byte-identical across runs; and refuse
what nobody has reviewed, a marker that claims more than its audit shows,
and a marker or audit file that is not well formed (every such refusal is
run).

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only.
"""

import contextlib
import hashlib
import importlib.machinery
import importlib.util
import io
import json
import os
import re
import sys
import tempfile
import tomllib
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    """Import an extensionless script in this directory as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


image_sbom = load("image-sbom")

CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
STD = "path+file:///toolchain/lib/rustlib/src/rust/library"
SDK_SHA = {"arm64": "a" * 64, "amd64": "b" * 64}
COMPONENTS = os.path.join(HERE, "image-components.toml")
INITIALISER = "microkit-sdk:board/qemu_virt_aarch64/debug/elf/initialiser.elf"
with open(os.path.join(HERE, "initialiser-audit.toml"), "rb") as _handle:
    AUDIT = tomllib.load(_handle)
# The SDK the real audit was done for.
AUDITED_SHA = {"arm64": AUDIT["audit"]["sdk-sha256-arm64"], "amd64": AUDIT["audit"]["sdk-sha256-amd64"]}
# A synthetic audit's crates: one from each source, and one licence off the house list.
CRATES = [
    {"name": "initialiser", "version": "2.3.1", "licence": "BSD-2-Clause", "source": "microkit"},
    {"name": "sel4", "version": "0.1.0", "licence": "MIT", "source": "rust-sel4"},
    {"name": "log", "version": "0.4.33", "licence": "MIT OR Apache-2.0", "source": "crates.io", "checksum": "1" * 64},
    {"name": "dlmalloc", "version": "0.2.14", "licence": "MIT/Apache-2.0", "source": "crates.io",
     "checksum": "2" * 64},
    {"name": "compiler_builtins", "version": "0.1.160", "source": "rust", "pending": "the project owner",
     "licence": "MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)"},
]
NOTICE = "Copyright 2026 The Example Font Project Authors\n\nThis Font Software is licensed under the OFL.\n"
GLYPH_MANIFEST = """crate = "nowayos-glyphs"
licence = "OFL-1.1"
notice = "OFL.txt"
copyright = "Copyright 2026 The Example Font Project Authors"
rasteriser = "swash 0.2.10"
made = "unhinted"

[[font]]
family = "Example Sans"
version = "1.000"
style = "Regular"
file = "fonts/ExampleSans-Regular.ttf"
sha256 = "%s"
source = "https://example.invalid/fonts/ExampleSans-Regular.ttf"
decision = "signed off, 2026-01-01"
""" % ("e" * 64)
DESCRIPTION = """<?xml version="1.0" encoding="UTF-8"?>
<system>
    <memory_region name="screen" size="0x1000" prefill_path="screen-8x8.bin" />
    <protection_domain name="alpha" priority="2">
        <program_image path="pd-a.elf" />
        <protection_domain name="child" id="1" priority="1">
            <program_image path="pd-b.elf" />
        </protection_domain>
    </protection_domain>
    <protection_domain name="gamma" priority="3">
        <program_image path="pd-a.elf" />
    </protection_domain>
</system>
"""


def sha(data):
    return hashlib.sha256(data).hexdigest()


def audit_text(crates, **head):
    """A synthetic audit file: [audit] for the default fixture's SDK, then the
    crates. Values are written as JSON, which TOML reads alike for strings and
    numbers."""
    values = {"date": "2026-01-02", "sdk": "2.3.1", "sdk-sha256-arm64": SDK_SHA["arm64"],
              "sdk-sha256-amd64": SDK_SHA["amd64"], "target": "aarch64-unknown-none", "microkit-commit": "e" * 40,
              "rust-sel4-commit": "f" * 40, "rust": "1.94.0 (test)", "established": "by a test",
              "advisories": "none"}
    values.update({key.replace("_", "-"): value for key, value in head.items()})
    lines = ["[audit]"] + [f"{key} = {json.dumps(value)}" for key, value in values.items() if value is not None]
    for crate in crates:
        lines += ["", "[[crates]]"] + [f"{key} = {json.dumps(value)}" for key, value in crate.items()]
    return "\n".join(lines) + "\n"


def package(name, version, source, licence, kind="lib", manifest=None):
    pid = f"{source}#{name}@{version}"
    return {"name": name, "version": version, "id": pid, "source": None if source.startswith("path+") else source,
            "license": licence, "license_file": None, "description": f"the {name} crate",
            "manifest_path": manifest or f"/cargo/registry/src/{name}-{version}/Cargo.toml",
            "targets": [{"kind": [kind], "name": name}]}


class Fixture:
    """A synthetic SDK, build folder, PD workspace and cargo output in a temporary directory."""

    def __init__(self, root, sdk_sha=SDK_SHA):
        self.root = root
        self.sdk = os.path.join(root, "sdk")
        self.elf = os.path.join(self.sdk, "board", "qemu_virt_aarch64", "debug", "elf")
        self.dir = os.path.join(root, "build", "qemu_virt_aarch64", "demo")
        self.pds = os.path.join(root, "pds")
        for path in (self.elf, os.path.join(self.sdk, "bin"), self.dir, self.pds):
            os.makedirs(path)
        self.write(os.path.join(self.sdk, "VERSION"), "2.3.1\n")
        for name in ("sel4.elf", "loader.elf", "monitor.elf", "initialiser.elf"):
            self.write(os.path.join(self.elf, name), f"sdk {name}")
        self.write(os.path.join(self.sdk, "bin", "microkit"), "the microkit tool")
        self.write(os.path.join(self.dir, "demo.system"), DESCRIPTION)
        for name in ("loader.img", "pd-a.elf", "pd-b.elf", "screen-8x8.bin"):
            self.write(os.path.join(self.dir, name), f"built {name}")
        # The glyph atlas crate: its notice and manifest; make image copies the notice beside the image.
        self.glyphs = os.path.join(root, "glyphs")
        os.makedirs(os.path.join(self.glyphs, "src", "atlas"))
        self.write(os.path.join(self.glyphs, "OFL.txt"), NOTICE)
        self.write(os.path.join(self.glyphs, "src", "atlas", "manifest.toml"), GLYPH_MANIFEST)
        self.write(os.path.join(self.dir, "OFL.txt"), NOTICE)
        self.containerfile = self.write(os.path.join(root, "Containerfile"), "\n".join([
            "ARG MICROKIT_VERSION=2.3.1",
            f"ARG MICROKIT_SHA256_ARM64={sdk_sha['arm64']}",
            f"ARG MICROKIT_SHA256_AMD64={sdk_sha['amd64']}",
            "ARG UBOOT_VERSION=2026.07",
            f"ARG UBOOT_SHA256={'c' * 64}",
            "ARG RPI_FIRMWARE_VERSION=1.20260915",
            f"ARG RPI_FIRMWARE_COMMIT={'d' * 40}",
        ]) + "\n")
        self.position = self.write(os.path.join(root, "position.toml"), "\n".join([
            "[sdk]", 'version = "2.3.1"', f'sha256-arm64 = "{sdk_sha["arm64"]}"',
            f'sha256-amd64 = "{sdk_sha["amd64"]}"', 'sel4 = "16.0.0"',
        ]) + "\n")
        self.std_crates = self.write(os.path.join(root, "std-crates.txt"), "\n".join([
            "core MIT OR Apache-2.0", "alloc MIT OR Apache-2.0",
            "compiler_builtins MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)",
        ]) + "\n")
        self.packages = [
            package("pd-a", "0.1.0", f"path+file://{self.pds}/a", None, "bin", f"{self.pds}/a/Cargo.toml"),
            package("pd-b", "0.1.0", f"path+file://{self.pds}/b", None, "bin", f"{self.pds}/b/Cargo.toml"),
            package("shared", "1.0.0", CRATES_IO, "MIT OR Apache-2.0"),
            package("only-a", "2.0.0", CRATES_IO, "GPL-2.0-only OR MIT"),
            package("linked-gpl", "0.3.0", CRATES_IO, "MIT AND GPL-2.0-only"),
            package("derive", "1.0.0", CRATES_IO, "MIT", "proc-macro"),
            package("codegen", "0.9.0", CRATES_IO, "Apache-2.0"),
        ]
        ids = {p["name"]: p["id"] for p in self.packages}
        self.ids = ids
        self.metadata = self.write_json("metadata.json", {
            "packages": self.packages,
            "workspace_members": [ids["pd-a"], ids["pd-b"]],
            "workspace_root": self.pds,
            "resolve": {"nodes": [{"id": pid, "deps": []} for pid in ids.values()]},
        })
        self.write(os.path.join(self.pds, "Cargo.lock"), "\n".join(
            f'[[package]]\nname = "{p["name"]}"\nversion = "{p["version"]}"\n'
            + (f'source = "{CRATES_IO}"\nchecksum = "{sha(p["name"].encode())}"\n' if p["source"] else "")
            for p in self.packages))
        target = "aarch64-sel4"

        def unit(pkg_id, kind, name, platform, deps, mode="build"):
            return {"pkg_id": pkg_id, "target": {"kind": [kind], "name": name}, "platform": platform,
                    "mode": mode, "features": [], "dependencies": [{"index": i} for i in deps]}

        self.units = [
            unit(ids["pd-a"], "bin", "pd-a", target, [2, 3, 4, 5, 9]),                  # 0
            unit(ids["pd-b"], "bin", "pd-b", target, [2, 5, 10]),                       # 1
            unit(ids["shared"], "lib", "shared", target, [5, 6, 7, 8]),                 # 2
            unit(ids["only-a"], "lib", "only_a", target, [5]),                          # 3
            unit(f"{STD}/alloc#0.0.0", "lib", "alloc", target, [5, 7]),                 # 4
            unit(f"{STD}/core#0.0.0", "lib", "core", target, []),                       # 5
            unit(ids["shared"], "custom-build", "build-script-build", target, [11],
                 mode="run-custom-build"),                                              # 6
            unit(f"{STD}/compiler-builtins/compiler-builtins#compiler_builtins@0.1.160",
                 "lib", "compiler_builtins", target, [5]),                              # 7
            unit(ids["derive"], "proc-macro", "derive", None, [12]),                    # 8
            unit(ids["linked-gpl"], "lib", "linked_gpl", target, [5]),                  # 9
            unit(ids["derive"], "proc-macro", "derive", None, [12]),                    # 10 (as cargo lists it again)
            unit(ids["shared"], "custom-build", "build-script-build", None, [12]),      # 11
            unit(ids["codegen"], "lib", "codegen", None, []),                           # 12
        ]
        self.graph = self.write_json("unit-graph.json", {"version": 1, "units": self.units, "roots": [0, 1]})
        self.card_config = self.write(os.path.join(root, "config.txt"), "kernel=u-boot.bin\n")
        self.fragment = self.write(os.path.join(root, "u-boot.config"), "CONFIG_BOOTDELAY=0\n")
        self.firmware_files = {
            "boot/COPYING.linux": "c" * 64, "boot/LICENCE.broadcom": "1" * 64,
            "boot/bcm2712-rpi-5-b.dtb": "2" * 64, "boot/fixup4.dat": "3" * 64,
            "boot/overlays/README": "4" * 64, "boot/overlays/act-led.dtbo": "5" * 64,
            "boot/start4.elf": "6" * 64,
        }
        self.firmware = self.write_firmware(self.firmware_files)
        self.uboot_bin = self.write(os.path.join(root, "u-boot.bin"), "u-boot binary")

    def write(self, path, text):
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        return path

    def write_json(self, name, value):
        return self.write(os.path.join(self.root, name), json.dumps(value))

    def write_firmware(self, files, name="rpi-firmware.sha256"):
        return self.write(os.path.join(self.root, name),
                          "".join(f"{digest}  {path}\n" for path, digest in sorted(files.items())))

    def argv(self, out, *extra):
        return ["image-sbom", "--system", "demo", "--board", "qemu_virt_aarch64", "--config", "debug",
                "--dir", self.dir, "--sdk", self.sdk, "--pds", self.pds, "--toolchain", "nightly-2026-04-04",
                "--std-crates", self.std_crates, "--containerfile", self.containerfile,
                "--position", self.position, "--unit-graph", self.graph, "--metadata", self.metadata,
                "--arch", "arm64", "--glyphs", self.glyphs, "--out", out, *extra]

    def sdcard_args(self, *extra):
        return ("--sdcard-config", self.card_config, "--uboot-defconfig", "rpi_arm64_defconfig",
                "--uboot-fragment", self.fragment, "--firmware", self.firmware, *extra)


def run(argv):
    """(exit status, stdout, stderr) of image-sbom's main with argv."""
    out, err = io.StringIO(), io.StringIO()
    saved = sys.argv
    sys.argv = argv
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            try:
                status = image_sbom.main()
            except SystemExit as exit_:
                status = exit_.code
    finally:
        sys.argv = saved
    return status, out.getvalue(), err.getvalue()


def props(entry):
    return {prop["name"]: prop["value"] for prop in entry.get("properties", [])}


class ImageSbomTest(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.fx = Fixture(self.tmp.name)
        self.out = os.path.join(self.tmp.name, "sbom.cdx.json")

    def tearDown(self):
        self.tmp.cleanup()

    def make(self, *extra):
        status, _, err = run(self.fx.argv(self.out, *extra))
        self.assertEqual(status, 0, err)
        with open(self.out, encoding="utf-8") as handle:
            document = json.load(handle)
        self.assertEqual(image_sbom.sbom.validate(document), [])
        return document, {entry["bom-ref"]: entry for entry in document["components"]}

    def refuse(self, *extra):
        status, _, err = run(self.fx.argv(self.out, *extra))
        self.assertEqual(status, 2, err)
        return err

    def components(self, composition, audit=None):
        """--components FILE: the real components file with the initialiser's
        composition replaced, and its audit, when given, a synthetic one."""
        with open(COMPONENTS, encoding="utf-8") as handle:
            text = handle.read()
        named = 'audit = "audit.toml"\n' if audit is not None else ""
        text, count = re.subn(r'^composition = ".*"\n(?:audit = ".*"\n)?', f'composition = "{composition}"\n{named}',
                              text, flags=re.MULTILINE)
        self.assertEqual(count, 1)
        if audit is not None:
            self.fx.write(os.path.join(self.tmp.name, "audit.toml"), audit)
        return "--components", self.fx.write(os.path.join(self.tmp.name, "components.toml"), text)

    def edited(self, components, old, new):
        """--components FILE as components() wrote it, with `old` made `new`."""
        flag, path = components
        with open(path, encoding="utf-8") as handle:
            text = handle.read()
        self.assertEqual(text.count(old), 1, old)
        return flag, self.fx.write(path, text.replace(old, new))

    def test_lists_the_sdk_parts_and_the_image_sha256(self):
        document, parts = self.make()
        image = document["metadata"]["component"]
        self.assertEqual(image["hashes"], [{"alg": "SHA-256", "content": sha(b"built loader.img")}])
        self.assertEqual(props(document["metadata"])["nowayos:image-sha256"], sha(b"built loader.img"))
        elf = "microkit-sdk:board/qemu_virt_aarch64/debug/elf/"
        expected = {"sel4.elf": ("sel4", "16.0.0", "GPL-2.0-only"),
                    "loader.elf": ("microkit-loader", "2.3.1", "BSD-2-Clause"),
                    "monitor.elf": ("microkit-monitor", "2.3.1", "BSD-2-Clause"),
                    "initialiser.elf": ("microkit-capdl-initialiser", "2.3.1", "BSD-2-Clause")}
        for name, (component, version, licence) in expected.items():
            entry = parts[elf + name]
            self.assertEqual((entry["name"], entry["version"], entry["scope"]), (component, version, "required"))
            self.assertEqual(entry["hashes"][0]["content"], sha(f"sdk {name}".encode()))
            self.assertEqual(entry["licenses"], [{"expression": licence}])
            self.assertEqual(entry["externalReferences"][0]["hashes"][0]["content"], SDK_SHA["arm64"])
        tool = parts["microkit-sdk:bin/microkit"]
        self.assertEqual(tool["scope"], "excluded")
        self.assertIn("build time only", props(tool)["nowayos:excluded"])
        root = next(d for d in document["dependencies"] if d["ref"] == "image:demo")
        self.assertEqual(root["dependsOn"], sorted([elf + n for n in expected] + [
            "file:demo.system", "file:screen-8x8.bin", "notice:OFL.txt", "path:a@0.1.0", "path:b@0.1.0"]))

    def test_a_restart_image_is_named_as_one_not_as_configuration(self):
        name, what, source = image_sbom.prefill_file("restart-worker.bin")
        self.assertIn("restart image", what)
        self.assertEqual(source, "written by tools/restart-image from the PD's program image")
        self.assertIn("board-system", image_sbom.prefill_file("screen-8x8.bin")[2])
        self.assertIn("board-system", image_sbom.prefill_file("restart-.bin")[2])

    def test_a_test_only_stand_in_is_named_as_one(self):
        name, what, source = image_sbom.prefill_file("test-counts-4-0.bin")
        self.assertEqual(name, "test-counts-4-0.bin")
        self.assertIn("test-only stand-in", what)
        self.assertIn("refuses it outside a test system", what)
        self.assertIn("board-system", source)

    def test_a_devices_facts_are_named_as_the_board_files(self):
        name, what, source = image_sbom.prefill_file("device-debug_uart.bin")
        self.assertEqual(name, "device-debug_uart.bin")
        self.assertIn("compatible string", what)
        self.assertIn("device_facts", what)
        self.assertIn("from the board file", source)
        self.assertIn("Configuration", image_sbom.prefill_file("device-.bin")[1])

    def test_the_build_time_is_listed_with_its_sha256_as_the_floor_for_wall_clock_time(self):
        name, what, source = image_sbom.prefill_file("build-time.bin")
        self.assertIn("build time", what)
        self.assertIn("wall-clock", what)
        self.assertIn("SOURCE_DATE_EPOCH", source)
        self.assertIn("commit time", source)
        # In an image that prefills it: listed with its sha256.
        self.fx.write(os.path.join(self.fx.dir, "demo.system"), DESCRIPTION.replace(
            '<memory_region name="screen" size="0x1000" prefill_path="screen-8x8.bin" />',
            '<memory_region name="screen" size="0x1000" prefill_path="screen-8x8.bin" />\n'
            '    <memory_region name="build_time" size="0x1000" prefill_path="build-time.bin" />'))
        floor = (1_800_000_000).to_bytes(8, "little")
        with open(os.path.join(self.fx.dir, "build-time.bin"), "wb") as handle:
            handle.write(floor)
        _, parts = self.make()
        entry = parts["file:build-time.bin"]
        self.assertEqual(entry["hashes"][0]["content"], sha(floor))
        self.assertEqual(entry["scope"], "required")
        self.assertIn("floor", entry["description"])

    def test_each_program_image_with_its_crates(self):
        document, parts = self.make()
        pd_a = parts["path:a@0.1.0"]
        self.assertEqual(pd_a["type"], "application")
        self.assertEqual(pd_a["hashes"][0]["content"], sha(b"built pd-a.elf"))
        self.assertEqual(props(pd_a)["nowayos:program-image"], "pd-a.elf")
        self.assertEqual(props(pd_a)["nowayos:protection-domains"], "alpha, gamma")
        self.assertEqual(props(parts["path:b@0.1.0"])["nowayos:protection-domains"], "child")
        # Crates compiled for the device: in the program images that use them.
        self.assertEqual(props(parts["pkg:cargo/shared@1.0.0"])["nowayos:program-images"], "pd-a.elf, pd-b.elf")
        self.assertEqual(props(parts["pkg:cargo/only-a@2.0.0"])["nowayos:program-images"], "pd-a.elf")
        self.assertEqual(parts["pkg:cargo/shared@1.0.0"]["hashes"][0]["content"], sha(b"shared"))
        for crate, version in (("core", "0.0.0"), ("alloc", "0.0.0"), ("compiler_builtins", "0.1.160")):
            entry = parts[f"rust-std:{crate}@{version}"]
            self.assertEqual(entry["scope"], "required")
        self.assertEqual(props(parts["rust-std:alloc@0.0.0"])["nowayos:program-images"], "pd-a.elf")
        # Build-time only: a proc macro and a build script's dependency.
        for ref in ("pkg:cargo/derive@1.0.0", "pkg:cargo/codegen@0.9.0"):
            self.assertEqual(parts[ref]["scope"], "excluded")
            self.assertIn("build time only", props(parts[ref])["nowayos:excluded"])
        graph = {d["ref"]: d["dependsOn"] for d in document["dependencies"]}
        self.assertEqual(graph["path:a@0.1.0"], sorted(["pkg:cargo/shared@1.0.0", "pkg:cargo/only-a@2.0.0",
                                                        "pkg:cargo/linked-gpl@0.3.0", "rust-std:alloc@0.0.0",
                                                        "rust-std:core@0.0.0"]))
        self.assertIn("pkg:cargo/derive@1.0.0", graph["pkg:cargo/shared@1.0.0"])
        self.assertIn("pkg:cargo/codegen@0.9.0", graph["pkg:cargo/shared@1.0.0"])
        self.assertEqual(parts["file:screen-8x8.bin"]["hashes"][0]["content"], sha(b"built screen-8x8.bin"))
        self.assertEqual(parts["file:demo.system"]["hashes"][0]["content"], sha(DESCRIPTION.encode()))

    def test_gpl_parts_need_a_source_offer(self):
        _, parts = self.make()
        offer = "nowayos:source-offer"
        self.assertIn(offer, props(parts["microkit-sdk:board/qemu_virt_aarch64/debug/elf/sel4.elf"]))
        self.assertIn(offer, props(parts["pkg:cargo/linked-gpl@0.3.0"]))
        self.assertNotIn(offer, props(parts["pkg:cargo/only-a@2.0.0"]))  # GPL-2.0-only OR MIT: take MIT
        self.assertNotIn(offer, props(parts["microkit-sdk:board/qemu_virt_aarch64/debug/elf/loader.elf"]))

    def test_the_initialiser_holds_the_crates_its_audit_found(self):
        # The real components file and audit, with the audited SDK pinned.
        self.fx = Fixture(self.tmp.name + "/audited", AUDITED_SHA)
        document, parts = self.make()
        initialiser = parts[INITIALISER]
        crates = AUDIT["crates"]
        pending = [c for c in crates if "pending" in c]
        marker = props(initialiser)["nowayos:composition"]
        self.assertTrue(marker.startswith(f"{'audited, sign-off pending' if pending else 'audited'}: "
                                          f"the {len(crates)} crates"), marker)
        for crate in pending:
            self.assertIn(f"{crate['name']} {crate['version']}", marker)
        self.assertIn("tools/initialiser-audit.toml: 2026-10-04", props(initialiser)["nowayos:audit"])
        self.assertEqual(document["compositions"], [{"aggregate": "complete", "assemblies": [INITIALISER]}])
        inside = {entry["bom-ref"]: entry for entry in initialiser["components"]}
        self.assertEqual(sorted(inside), sorted(f"initialiser:{c['name']}@{c['version']}" for c in crates))
        for crate in crates:
            entry = inside[f"initialiser:{crate['name']}@{crate['version']}"]
            self.assertEqual((entry["type"], entry["scope"]), ("library", "required"))
            self.assertEqual(entry["licenses"], [{"expression": image_sbom.spdx(crate["licence"])}])
            self.assertEqual("checksum" in crate, "hashes" in entry)
            if "checksum" in crate:
                self.assertEqual(entry["hashes"][0]["content"], crate["checksum"])
                self.assertEqual(entry["purl"], f"pkg:cargo/{crate['name']}@{crate['version']}")
            sign_off = props(entry).get("nowayos:licence-sign-off", "")
            self.assertEqual(sign_off.startswith("pending: "), "pending" in crate)
        self.assertIn(AUDIT["audit"]["rust-sel4-commit"], inside["initialiser:sel4-capdl-initializer@0.1.0"]["purl"])
        self.assertEqual(inside["initialiser:dlmalloc@0.2.14"]["licenses"], [{"expression": "MIT OR Apache-2.0"}])
        # Inside the initialiser only: no top-level component and no dependency entry names them.
        self.assertFalse(set(inside) & set(parts))
        self.assertFalse(set(inside) & {d["ref"] for d in document["dependencies"]})

    def test_an_audit_for_another_sdk_leaves_the_initialiser_unaudited(self):
        # The default fixture pins an SDK nobody audited.
        document, parts = self.make()
        initialiser = parts[INITIALISER]
        marker = props(initialiser)["nowayos:composition"]
        self.assertTrue(marker.startswith("composition unaudited: the audit in tools/initialiser-audit.toml"), marker)
        self.assertIn("re-run it for this SDK", marker)
        self.assertNotIn("components", initialiser)
        self.assert_no_claim_on_the_audit(initialiser)
        self.assertEqual(document["compositions"], [{"aggregate": "unknown", "assemblies": [INITIALISER]}])

    def assert_no_claim_on_the_audit(self, part):
        """Only the marker names the audit, to say it does not hold: no other
        property points to its crates or says what it found."""
        properties = props(part)
        self.assertEqual([name for name, value in properties.items() if "audit.toml" in value],
                         ["nowayos:composition"], properties)
        for name in ("nowayos:audit", "nowayos:audit-established", "nowayos:audit-advisories",
                     "nowayos:note"):
            self.assertNotIn(name, properties)

    def test_an_audit_for_another_architecture_does_not_hold(self):
        audit = audit_text(CRATES, target="riscv64gc-unknown-none-elf")
        _, parts = self.make(*self.components("audited, sign-off pending", audit))
        marker = props(parts[INITIALISER])["nowayos:composition"]
        self.assertTrue(marker.startswith("composition unaudited: the audit in audit.toml (2026-01-02) is for the build for "
                                          "riscv64gc-unknown-none-elf"), marker)
        self.assertNotIn("components", parts[INITIALISER])
        self.assert_no_claim_on_the_audit(parts[INITIALISER])

    def test_an_unaudited_part_is_marked_so(self):
        document, parts = self.make(*self.components("unaudited"))
        self.assertTrue(props(parts[INITIALISER])["nowayos:composition"].startswith("composition unaudited: "))
        self.assertEqual(document["compositions"], [{"aggregate": "unknown", "assemblies": [INITIALISER]}])
        self.assertIn("audit", self.refuse(*self.components("unaudited", audit_text(CRATES))))
        self.assertIn("audit", self.refuse(*self.components("audited")))  # names no audit
        # A marker the components file cannot hold.
        self.assertIn('composition must be "audited", "audited, sign-off pending" or "unaudited"',
                      self.refuse(*self.components("partly audited")))
        self.assertIn("needs a note", self.refuse(*self.edited(self.components("unaudited"), 'note = "Audited for',
                                                               '# note = "Audited for')))
        self.assertIn("audit must be a file name", self.refuse(*self.edited(
            self.components("audited, sign-off pending", audit_text(CRATES)), 'audit = "audit.toml"',
            'audit = "tools/audit.toml"')))
        self.assertIn("[sdk-tool] is not in the image: it has no composition", self.refuse(*self.edited(
            self.components("unaudited"), "[sdk-tool]\n", '[sdk-tool]\ncomposition = "unaudited"\nnote = "a tool"\n')))

    def test_the_marker_never_claims_more_than_the_audit_shows(self):
        _, parts = self.make(*self.components("audited, sign-off pending", audit_text(CRATES)))
        marker = props(parts[INITIALISER])["nowayos:composition"]
        self.assertTrue(marker.startswith("audited, sign-off pending: the 5 crates"), marker)
        self.assertIn("pending for compiler_builtins 0.1.160", marker)
        err = self.refuse(*self.components("audited", audit_text(CRATES)))
        self.assertIn("compiler_builtins 0.1.160 is pending", err)
        signed = [dict(c) for c in CRATES]
        del signed[-1]["pending"]
        signed[-1]["signed-off"] = "2026-10-31"
        self.assertIn('no sign-off is pending', self.refuse(*self.components("audited, sign-off pending",
                                                                             audit_text(signed))))
        _, parts = self.make(*self.components("audited", audit_text(signed)))
        initialiser = parts[INITIALISER]
        self.assertTrue(props(initialiser)["nowayos:composition"].startswith("audited: the 5 crates"))
        inside = {entry["bom-ref"]: entry for entry in initialiser["components"]}
        self.assertEqual(props(inside["initialiser:compiler_builtins@0.1.160"])["nowayos:licence-sign-off"],
                         "signed off: 2026-10-31")
        self.assertEqual(inside["initialiser:sel4@0.1.0"]["externalReferences"],
                         [{"type": "vcs", "url": "https://github.com/seL4/rust-sel4"}])
        self.assertNotIn("purl", inside["initialiser:compiler_builtins@0.1.160"])  # Rust's own library

    def test_a_gpl_crate_inside_an_audited_part_needs_a_source_offer(self):
        crates = CRATES + [{"name": "copyleft", "version": "1.0.0", "licence": "GPL-2.0-only", "source": "crates.io",
                            "checksum": "5" * 64, "pending": "the project owner"}]
        status, out, err = run(self.fx.argv(self.out, *self.components("audited, sign-off pending",
                                                                          audit_text(crates))))
        self.assertEqual(status, 0, err)
        # The kernel, the fixture's linked-gpl crate, and the crate inside the initialiser.
        self.assertIn("; 3 need a source offer;", out)
        with open(self.out, encoding="utf-8") as handle:
            document = json.load(handle)
        initialiser = next(c for c in document["components"] if c["bom-ref"] == INITIALISER)
        inside = {entry["bom-ref"]: entry for entry in initialiser["components"]}
        self.assertIn("nowayos:source-offer", props(inside["initialiser:copyleft@1.0.0"]))
        self.assertNotIn("nowayos:source-offer", props(inside["initialiser:log@0.4.33"]))

    def test_refuses_an_audit_that_is_not_reviewed_or_not_well_formed(self):
        def refused(crates, **head):
            return refused_text(audit_text(crates, **head))

        def refused_text(text):
            return self.refuse(*self.components("audited, sign-off pending", text))

        unreviewed = [dict(c) for c in CRATES]
        del unreviewed[-1]["pending"]  # off the house list, and silent about it
        self.assertIn("compiler_builtins 0.1.160: MIT AND Apache-2.0 WITH LLVM-exception", refused(unreviewed))
        zlib = CRATES + [{"name": "zlibbed", "version": "1.0.0", "licence": "Zlib", "source": "crates.io",
                          "checksum": "3" * 64}]
        self.assertIn("zlibbed 1.0.0: Zlib needs more than the house list", refused(zlib))
        needless = [dict(c) for c in CRATES]
        needless[2]["pending"] = "nothing"
        self.assertIn("log 0.4.33: MIT OR Apache-2.0 is on the house list", refused(needless))
        badly_signed = [dict(c) for c in CRATES]
        del badly_signed[-1]["pending"]
        badly_signed[-1]["signed-off"] = "yes"
        self.assertIn("the day it was signed off", refused(badly_signed))
        both = [dict(c) for c in CRATES]
        both[-1]["signed-off"] = "2026-10-31"  # and still pending
        self.assertIn("compiler_builtins 0.1.160: has both signed-off and pending - say where its sign-off stands with "
                      "one of them, not both", refused(both))
        for change, message in ((("log", "checksum", None), "checksum"), (("sel4", "checksum", "4" * 64), "checksum"),
                                (("log", "checksum", "XYZ"), "hex"), (("sel4", "source", "github"), "source must"),
                                (("log", "licence", "MIT OR"), "SPDX"), (("log", "colour", "blue"), "unknown keys"),
                                (("sel4", "source", None), "entry 2 needs source"),
                                (("sel4", "version", None), "entry 2 needs version"),
                                (("sel4", "version", 1), "entry 2: every value must be a non-empty string"),
                                (("log", "note", ""), "entry 3: every value must be a non-empty string")):
            (name, key, value), crates = change, [dict(c) for c in CRATES]
            crate = next(c for c in crates if c["name"] == name)
            if value is None:
                del crate[key]
            else:
                crate[key] = value
            self.assertIn(message, refused(crates), change)
        self.assertIn("listed twice", refused(CRATES + [CRATES[1]]))
        self.assertIn("hex", refused(CRATES, sdk_sha256_arm64="not-a-sha"))
        self.assertIn("[audit] needs exactly", refused(CRATES, advisories=None))
        self.assertIn("YYYY-MM-DD", refused(CRATES, date="4 October"))
        # The file's shape: its two tables, and each crate a table.
        whole = audit_text(CRATES)
        self.assertIn("unknown tables extra", refused_text(whole + '\n[extra]\nnote = "x"\n'))
        self.assertIn("no [audit] table", refused_text(whole[whole.index("[[crates]]"):]))
        self.assertIn("no [[crates]] entries", refused([]))
        self.assertIn("[[crates]] entry 1 must be a table", refused_text('crates = ["initialiser"]\n' + audit_text([])))

    def test_on_house_list(self):
        cases = {"MIT": True, "MIT OR Apache-2.0": True, "0BSD OR MIT OR Apache-2.0": True, "ISC": True,
                 "BSD-3-Clause AND MIT": True, "Zlib": False, "MIT AND Zlib": False, "Unicode-3.0": False,
                 "(MIT OR Apache-2.0) AND Unicode-3.0": False, "Apache-2.0 WITH LLVM-exception": False,
                 "MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)": False,
                 "Apache-2.0 WITH LLVM-exception OR MIT": True, "GPL-2.0-only": False}
        for expression, expected in cases.items():
            self.assertEqual(image_sbom.on_house_list(expression), expected, expression)
        self.assertEqual(image_sbom.spdx("MIT/Apache-2.0"), "MIT OR Apache-2.0")

    def test_the_structure_check_covers_nested_parts(self):
        document, _ = self.make(*self.components("audited, sign-off pending", audit_text(CRATES)))
        self.assertEqual(image_sbom.sbom.validate(document), [])
        initialiser = next(c for c in document["components"] if c["bom-ref"] == INITIALISER)
        inner = initialiser["components"][0]
        # A dependency may name a nested part; a ref must be unique at any depth; one level only.
        document["dependencies"].append({"ref": inner["bom-ref"], "dependsOn": []})
        self.assertEqual(image_sbom.sbom.validate(document), [])
        inner["components"] = [dict(inner, **{"bom-ref": "deeper"})]
        initialiser["components"].append(dict(inner, **{"bom-ref": INITIALISER}))
        problems = image_sbom.sbom.validate(document)
        self.assertTrue(any("must not nest components" in problem for problem in problems), problems)
        self.assertTrue(any("is not unique" in problem for problem in problems), problems)
        initialiser["components"] = []
        self.assertTrue(any("non-empty array" in p for p in image_sbom.sbom.validate(document)))

    def test_avoids_gpl(self):
        cases = {"MIT": True, "GPL-2.0-only": False, "GPL-2.0-only OR MIT": True, "MIT AND GPL-2.0-only": False,
                 "GPL-2.0-only AND (MIT OR Apache-2.0)": False, "(GPL-2.0-only AND MIT) OR BSD-2-Clause": True,
                 "LGPL-2.1-or-later": False, "AGPL-3.0-only OR GPL-3.0-only": False,
                 "GPL-2.0-only WITH Linux-syscall-note": False, "Apache-2.0 WITH LLVM-exception": True,
                 "MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)": True}
        for expression, expected in cases.items():
            self.assertEqual(image_sbom.avoids_gpl(expression), expected, expression)
        for bad in ("", "MIT OR", "(MIT", "MIT AND AND BSD-2-Clause", "MIT WITH"):
            with self.assertRaises(image_sbom.InputError, msg=bad):
                image_sbom.avoids_gpl(bad)

    def test_two_runs_give_the_same_bytes_and_no_absolute_path(self):
        self.make()
        with open(self.out, "rb") as handle:
            first = handle.read()
        self.make()
        with open(self.out, "rb") as handle:
            self.assertEqual(handle.read(), first)
        self.assertNotIn(self.tmp.name.encode(), first)

    def test_sdcard_board_lists_uboot_the_firmware_and_config(self):
        document, parts = self.make(*self.fx.sdcard_args())
        uboot = parts["sdcard:u-boot.bin"]
        self.assertEqual((uboot["name"], uboot["version"]), ("u-boot", "2026.07"))
        self.assertEqual(uboot["licenses"], [{"expression": "GPL-2.0-or-later"}])
        self.assertIn("nowayos:source-offer", props(uboot))
        self.assertNotIn("hashes", uboot)
        source = uboot["externalReferences"][0]
        self.assertEqual(source["url"], "https://ftp.denx.de/pub/u-boot/u-boot-2026.07.tar.bz2")
        self.assertEqual(source["hashes"][0]["content"], "c" * 64)
        self.assertIn(sha(b"CONFIG_BOOTDELAY=0\n"), props(uboot)["nowayos:configuration"])
        for path, digest in self.fx.firmware_files.items():
            entry = parts["sdcard:" + path[len("boot/"):]]
            self.assertEqual((entry["version"], entry["hashes"][0]["content"]), ("1.20260915", digest))
        self.assertIn("nowayos:source-offer", props(parts["sdcard:overlays/act-led.dtbo"]))
        self.assertNotIn("nowayos:source-offer", props(parts["sdcard:bcm2712-rpi-5-b.dtb"]))
        self.assertEqual(parts["sdcard:start4.elf"]["licenses"][0]["expression"],
                         "LicenseRef-Broadcom-Raspberry-Pi-Firmware")
        self.assertNotIn("licenses", parts["sdcard:LICENCE.broadcom"])
        self.assertEqual(parts["sdcard:config.txt"]["hashes"][0]["content"], sha(b"kernel=u-boot.bin\n"))
        root = next(d for d in document["dependencies"] if d["ref"] == "image:demo")
        self.assertIn("sdcard:u-boot.bin", root["dependsOn"])
        _, parts = self.make(*self.fx.sdcard_args("--uboot-bin", self.fx.uboot_bin))
        self.assertEqual(parts["sdcard:u-boot.bin"]["hashes"][0]["content"], sha(b"u-boot binary"))

    def test_the_glyph_atlases_notice_is_beside_every_image_naming_the_font(self):
        document, parts = self.make()
        notice = parts["notice:OFL.txt"]
        self.assertEqual((notice["type"], notice["name"], notice["scope"]), ("file", "OFL.txt", "required"))
        self.assertEqual(notice["hashes"][0]["content"], sha(NOTICE.encode()))
        self.assertNotIn("licenses", notice)  # it is the licence text itself
        found = props(notice)
        self.assertEqual(found["nowayos:font-family"], "Example Sans")
        self.assertEqual(found["nowayos:font-version"], "1.000")
        self.assertEqual(found["nowayos:font-file"], "fonts/ExampleSans-Regular.ttf")
        self.assertEqual(found["nowayos:font-sha256"], "e" * 64)
        self.assertIn("Copyright 2026 The Example Font Project Authors", found["nowayos:notice-for"])
        self.assertTrue(found["nowayos:atlases-in"].startswith("none"))
        self.assertEqual(found["nowayos:location"], image_sbom.BESIDE_IMAGE)
        root = next(d for d in document["dependencies"] if d["ref"] == "image:demo")
        self.assertIn("notice:OFL.txt", root["dependsOn"])
        _, parts = self.make(*self.fx.sdcard_args())
        self.assertIn(image_sbom.ON_CARD, props(parts["notice:OFL.txt"])["nowayos:location"])

    def test_the_atlas_crate_names_the_font_it_was_made_from(self):
        glyphs = package("nowayos-glyphs", "0.1.0", f"path+file://{self.fx.glyphs}", "OFL-1.1",
                         manifest=f"{self.fx.glyphs}/Cargo.toml")
        units = self.fx.units + [{"pkg_id": glyphs["id"], "target": {"kind": ["lib"], "name": "nowayos_glyphs"},
                                  "platform": "aarch64-sel4", "mode": "build", "features": [],
                                  "dependencies": [{"index": 5}]}]
        units[1] = {**units[1], "dependencies": units[1]["dependencies"] + [{"index": len(units) - 1}]}
        with open(self.fx.graph, "w", encoding="utf-8") as handle:
            json.dump({"version": 1, "units": units, "roots": [0, 1]}, handle)
        with open(self.fx.metadata, encoding="utf-8") as handle:
            metadata = json.load(handle)
        metadata["packages"].append(glyphs)
        metadata["resolve"]["nodes"].append({"id": glyphs["id"], "deps": []})
        with open(self.fx.metadata, "w", encoding="utf-8") as handle:
            json.dump(metadata, handle)
        _, parts = self.make()
        crate = next(entry for entry in parts.values() if entry["name"] == "nowayos-glyphs")
        self.assertEqual((crate["scope"], crate["licenses"]), ("required", [{"expression": "OFL-1.1"}]))
        found = props(crate)
        self.assertEqual(found["nowayos:program-images"], "pd-b.elf")
        self.assertEqual(found["nowayos:font-sha256"], "e" * 64)
        self.assertEqual(found["nowayos:notice"], "OFL.txt beside loader.img")
        self.assertNotIn("nowayos:source-offer", found)
        self.assertEqual(props(parts["notice:OFL.txt"])["nowayos:atlases-in"], "pd-b.elf")

    def test_refuses_an_image_without_the_glyph_atlases_notice(self):
        os.remove(os.path.join(self.fx.dir, "OFL.txt"))
        err = self.refuse()
        self.assertIn("OFL.txt is missing", err)
        self.fx.write(os.path.join(self.fx.dir, "OFL.txt"), NOTICE + "changed\n")
        self.assertIn("differs from OFL.txt", self.refuse())

    def test_refuses_a_glyph_manifest_without_the_font(self):
        self.fx.write(os.path.join(self.fx.glyphs, "src", "atlas", "manifest.toml"),
                      GLYPH_MANIFEST.split("[[font]]")[0])
        self.assertIn("names no [[font]]", self.refuse())

    def test_refuses_a_standard_library_crate_not_on_the_reviewed_list(self):
        with open(self.fx.std_crates, "w", encoding="utf-8") as handle:
            handle.write("core MIT OR Apache-2.0\nalloc MIT OR Apache-2.0\n")
        self.assertIn("compiler_builtins", self.refuse())

    def test_refuses_an_sdk_file_the_components_file_does_not_list(self):
        self.fx.write(os.path.join(self.fx.elf, "extra.elf"), "new in the SDK")
        self.assertIn("extra.elf", self.refuse())

    def test_refuses_a_firmware_file_no_entry_covers(self):
        self.fx.firmware = self.fx.write_firmware({**self.fx.firmware_files, "boot/new-blob.bin": "7" * 64})
        self.assertIn("boot/new-blob.bin", self.refuse(*self.fx.sdcard_args()))

    def test_refuses_a_program_image_cargo_does_not_build(self):
        with open(self.fx.graph, "w", encoding="utf-8") as handle:
            json.dump({"version": 1, "units": self.fx.units, "roots": [0]}, handle)
        self.assertIn("pd-b", self.refuse())

    def test_a_kernel_the_dev_image_builds_names_its_source(self):
        """On rpi5b_2gb the kernel is the dev image's own build, from
        the Containerfile's SEL4_COMMIT, not the Microkit SDK archive's; the
        loader and every other board's kernel still come from the archive."""
        commit = "6e7c3b733d296cfd88d5fbf635c96e447a882374"
        elf = os.path.join(self.fx.sdk, "board", "rpi5b_2gb", "debug", "elf")
        os.makedirs(elf)
        for name in ("sel4.elf", "loader.elf", "monitor.elf", "initialiser.elf"):
            self.fx.write(os.path.join(elf, name), f"sdk {name}")
        with open(self.fx.containerfile, encoding="utf-8") as handle:
            pins = handle.read()
        self.assertIn("ARG SEL4_COMMIT", self.refuse("--board", "rpi5b_2gb"))
        self.fx.write(self.fx.containerfile, pins + f"ARG SEL4_COMMIT={commit}\n")
        _, parts = self.make("--board", "rpi5b_2gb")
        kernel = parts["microkit-sdk:board/rpi5b_2gb/debug/elf/sel4.elf"]
        self.assertEqual((kernel["name"], kernel["version"]), ("sel4", "16.0.0"))
        self.assertEqual(kernel["externalReferences"],
                         [{"type": "vcs", "url": f"https://github.com/seL4/seL4/tree/{commit}"}])
        self.assertIn(f"built in the dev image from seL4 {commit}", props(kernel)["nowayos:source"])
        loader = parts["microkit-sdk:board/rpi5b_2gb/debug/elf/loader.elf"]
        self.assertEqual(loader["externalReferences"][0]["type"], "distribution")
        _, parts = self.make()
        kernel = parts["microkit-sdk:board/qemu_virt_aarch64/debug/elf/sel4.elf"]
        self.assertEqual(kernel["externalReferences"][0]["type"], "distribution")
        self.assertIn("in the Microkit SDK 2.3.1", props(kernel)["nowayos:source"])

    def test_a_stale_verification_position_leaves_the_kernel_version_unknown(self):
        self.fx.write(self.fx.position, "\n".join([
            "[sdk]", 'version = "2.3.0"', f'sha256-arm64 = "{"0" * 64}"', f'sha256-amd64 = "{"0" * 64}"',
            'sel4 = "15.0.0"']) + "\n")
        _, parts = self.make()
        kernel = parts["microkit-sdk:board/qemu_virt_aarch64/debug/elf/sel4.elf"]
        self.assertTrue(kernel["version"].startswith("unknown"))

    def test_the_structure_check_covers_compositions(self):
        document, _ = self.make()
        document["compositions"] = [{"aggregate": "mostly", "assemblies": ["no-such-part"]}]
        problems = image_sbom.sbom.validate(document)
        self.assertTrue(any("aggregate" in problem for problem in problems), problems)
        self.assertTrue(any("no-such-part" in problem for problem in problems), problems)


if __name__ == "__main__":
    unittest.main()

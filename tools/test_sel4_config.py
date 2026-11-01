# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/sel4-config: the record `make image` writes beside each
image, and the check that ties the verification position to the pinned SDK
.

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
import platform
import sys
import tempfile
import textwrap
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    """Import an extensionless script in this directory as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


sel4_config = load("sel4-config")

ARM64_SHA = "a" * 64
AMD64_SHA = "b" * 64

CONTAINERFILE = f"""\
ARG RUST_STABLE=1.93.1
# seL4 Microkit SDK.
ARG MICROKIT_VERSION=2.3.1
ARG MICROKIT_SHA256_ARM64={ARM64_SHA}
ARG MICROKIT_SHA256_AMD64={AMD64_SHA}
"""

POSITION = f"""\
[sdk]
version = "2.3.1"
sha256-arm64 = "{ARM64_SHA}"
sha256-amd64 = "{AMD64_SHA}"
sel4 = "16.0.0"
checked = "2026-10-03"

[configuration]
SEL4_ARCH = "aarch64"
ARM_HYPERVISOR_SUPPORT = true
VERIFICATION_BUILD = true
DEBUG_BUILD = false
MAX_NUM_NODES = "1"
ENABLE_SMP_SUPPORT = false
KERNEL_MCS = false
MAX_NUM_BOOTINFO_UNTYPED_CAPS = "50"

[platforms]
verified = ["bcm2712"]
not-verified = ["qemu-arm-virt"]

[labels]
DEBUG_BUILD = "development build: a debug kernel"
ENABLE_SMP_SUPPORT = "outside seL4's proofs: a multi-core kernel"
MAX_NUM_NODES = "outside seL4's proofs: a multi-core kernel"
KERNEL_MCS = "outside seL4's completed proofs: an MCS kernel"
PLAT = "outside seL4's proofs: a platform not on the list"
"""

# gen_config.json as the SDK's configurations have it (the options above).
RELEASE = {
    "SEL4_ARCH": "aarch64", "PLAT": "bcm2712", "ARM_HYPERVISOR_SUPPORT": True,
    "VERIFICATION_BUILD": True, "DEBUG_BUILD": False, "MAX_NUM_NODES": "1",
    "ENABLE_SMP_SUPPORT": False, "KERNEL_MCS": True, "MAX_NUM_BOOTINFO_UNTYPED_CAPS": "230",
    "PRINTING": False,
}
DEBUG = dict(RELEASE, VERIFICATION_BUILD=False, DEBUG_BUILD=True, PRINTING=True)
SMP_RELEASE = dict(RELEASE, MAX_NUM_NODES="4", ENABLE_SMP_SUPPORT=True)
MATCHING = dict(RELEASE, KERNEL_MCS=False, MAX_NUM_BOOTINFO_UNTYPED_CAPS="50")


def write(directory, name, text, mode="w"):
    path = os.path.join(directory, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, mode, **({} if "b" in mode else {"encoding": "utf-8"})) as handle:
        handle.write(text)
    return path


def run(argv):
    """Run sel4-config's main() with `argv`; return (status, stdout, stderr)."""
    out, err = io.StringIO(), io.StringIO()
    saved = sys.argv
    sys.argv = ["sel4-config"] + argv
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = sel4_config.main()
    finally:
        sys.argv = saved
    return status, out.getvalue(), err.getvalue()


class Sel4Config(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.containerfile = write(self.dir, "Containerfile", CONTAINERFILE)
        self.position = write(self.dir, "position.toml", POSITION)
        self.sdk = os.path.join(self.dir, "sdk")
        write(self.sdk, "VERSION", "2.3.1\n")
        self.image = write(self.dir, "out/loader.img", b"an image", "wb")

    def tearDown(self):
        self.tmp.cleanup()

    def add_config(self, board, config, options):
        write(self.sdk, f"board/{board}/{config}/include/kernel/gen_config.json", json.dumps(options))
        write(self.sdk, f"board/{board}/{config}/elf/sel4.elf", b"kernel " + config.encode(), "wb")

    def record(self, board, config, options=None):
        if options is not None:
            self.add_config(board, config, options)
        out = os.path.join(self.dir, "out", "sel4-config.txt")
        status, stdout, stderr = run([
            "record", "--sdk", self.sdk, "--board", board, "--config", config, "--system", "hello",
            "--image", self.image, "--out", out, "--position", self.position,
            "--containerfile", self.containerfile,
        ])
        text = ""
        if os.path.exists(out):
            with open(out, encoding="utf-8") as handle:
                text = handle.read()
        return status, stdout + stderr, text

    def fields(self, text):
        """The record's `key: value` lines, as {key: [values]}."""
        found = {}
        for line in text.splitlines():
            if line and not line.startswith("#") and ": " in line and not line.startswith(" "):
                key, _, value = line.partition(": ")
                found.setdefault(key, []).append(value)
        return found

    # ---- record -------------------------------------------------------------

    def test_record_names_the_sdk_board_configuration_and_options(self):
        status, output, text = self.record("rpi5b_2gb", "release", RELEASE)
        self.assertEqual(status, 0, output)
        fields = self.fields(text)
        arch = sel4_config.MACHINE_ARCH[platform.machine()]
        self.assertEqual(fields["board"], ["rpi5b_2gb"])
        self.assertEqual(fields["configuration"], ["release"])
        self.assertEqual(fields["system"], ["hello"])
        self.assertEqual(fields["sdk"], ["Microkit 2.3.1"])
        self.assertTrue(fields["sdk-sha256"][0].startswith(ARM64_SHA if arch == "arm64" else AMD64_SHA))
        self.assertEqual(fields["image-sha256"], [hashlib.sha256(b"an image").hexdigest()])
        self.assertEqual(fields["kernel-sha256"], [hashlib.sha256(b"kernel release").hexdigest()])
        self.assertIn("checked 2026-10-03 for this SDK", fields["position"][0])
        # Every option in the position, and the platform, with this kernel's value.
        self.assertRegex(text, r"(?m)^PLAT +bcm2712 +one of: bcm2712$")
        self.assertRegex(text, r"(?m)^KERNEL_MCS +true +false  DIFFERS$")
        self.assertRegex(text, r"(?m)^MAX_NUM_BOOTINFO_UNTYPED_CAPS +230 +50  DIFFERS$")
        self.assertRegex(text, r"(?m)^ARM_HYPERVISOR_SUPPORT +true +true$")
        # An MCS kernel on a listed platform: labelled, and the other difference named.
        self.assertEqual(fields["status"], [
            "outside seL4's completed proofs: an MCS kernel",
            "not seL4's AArch64 verified configuration: differs in MAX_NUM_BOOTINFO_UNTYPED_CAPS",
        ])

    def test_a_debug_configuration_is_labelled_a_development_build(self):
        status, output, text = self.record("qemu_virt_aarch64", "debug", dict(DEBUG, PLAT="qemu-arm-virt"))
        self.assertEqual(status, 0, output)
        statuses = self.fields(text)["status"]
        self.assertEqual(statuses[0], "development build: a debug kernel")
        self.assertIn("outside seL4's proofs: a platform not on the list", statuses)
        self.assertIn("development build", output)

    def test_an_smp_configuration_is_labelled_outside_the_proofs_once(self):
        status, output, text = self.record("rpi5b_2gb", "smp-release", SMP_RELEASE)
        self.assertEqual(status, 0, output)
        statuses = self.fields(text)["status"]
        self.assertEqual(statuses.count("outside seL4's proofs: a multi-core kernel"), 1)

    def test_a_matching_kernel_is_never_called_verified(self):
        status, output, text = self.record("rpi5b_2gb", "release", MATCHING)
        self.assertEqual(status, 0, output)
        statuses = self.fields(text)["status"]
        self.assertEqual(len(statuses), 1)
        self.assertIn("not a verification claim", statuses[0])
        self.assertNotIn("DIFFERS", text)
        self.assertIn("not its boot code, the System MMU", text)

    def test_an_option_missing_from_the_sdk_fails(self):
        options = dict(RELEASE)
        del options["KERNEL_MCS"]
        status, output, _ = self.record("rpi5b_2gb", "release", options)
        self.assertEqual(status, 2)
        self.assertIn("has no KERNEL_MCS", output)

    def test_an_unknown_board_or_configuration_fails(self):
        status, output, _ = self.record("rpi5b_2gb", "release")
        self.assertEqual(status, 2)
        self.assertIn("gen_config.json", output)

    def test_an_sdk_other_than_the_pinned_one_fails(self):
        write(self.sdk, "VERSION", "2.4.0\n")
        status, output, _ = self.record("rpi5b_2gb", "release", RELEASE)
        self.assertEqual(status, 2)
        self.assertIn("rebuild the dev image", output)

    def test_a_position_for_another_sdk_is_noted_in_the_record(self):
        write(self.dir, "Containerfile", CONTAINERFILE.replace(ARM64_SHA, "c" * 64).replace(AMD64_SHA, "d" * 64))
        status, output, text = self.record("rpi5b_2gb", "release", RELEASE)
        self.assertEqual(status, 0, output)
        fields = self.fields(text)
        self.assertTrue(fields["position"][0].startswith("NOT RE-CHECKED for this SDK"))
        self.assertEqual(fields["status"][0], "unknown: the verification position has not been re-checked for this SDK")

    # ---- check --------------------------------------------------------------

    def check(self):
        return run(["check", "--position", self.position, "--containerfile", self.containerfile])

    def test_check_passes_for_the_pinned_sdk(self):
        status, stdout, stderr = self.check()
        self.assertEqual(status, 0, stderr)
        self.assertIn("PASS", stdout)

    def test_check_fails_when_the_sdk_version_changes(self):
        write(self.dir, "Containerfile", CONTAINERFILE.replace("MICROKIT_VERSION=2.3.1", "MICROKIT_VERSION=2.4.0"))
        status, _, stderr = self.check()
        self.assertEqual(status, 1)
        self.assertIn("pins Microkit 2.4.0", stderr)
        self.assertIn("re-check", stderr.lower())

    def test_check_fails_when_either_archive_changes(self):
        for arch, old in (("arm64", ARM64_SHA), ("amd64", AMD64_SHA)):
            with self.subTest(arch=arch):
                write(self.dir, "Containerfile", CONTAINERFILE.replace(old, "e" * 64))
                status, _, stderr = self.check()
                self.assertEqual(status, 1)
                self.assertIn(f"pins the {arch} SDK archive sha256 {'e' * 64}", stderr)

    def test_check_needs_exactly_one_pin_of_each(self):
        write(self.dir, "Containerfile", CONTAINERFILE.replace(f"ARG MICROKIT_SHA256_AMD64={AMD64_SHA}\n", ""))
        status, _, stderr = self.check()
        self.assertEqual(status, 2)
        self.assertIn("MICROKIT_SHA256_AMD64", stderr)

    def test_a_label_for_an_option_the_position_lacks_is_an_error(self):
        write(self.dir, "position.toml", POSITION + 'NOT_AN_OPTION = "x"\n')
        status, _, stderr = self.check()
        self.assertEqual(status, 2)
        self.assertIn("NOT_AN_OPTION", stderr)

    def test_the_position_needs_its_sdk_fields(self):
        write(self.dir, "position.toml", POSITION.replace('checked = "2026-10-03"\n', ""))
        status, _, stderr = self.check()
        self.assertEqual(status, 2)
        self.assertIn("checked", stderr)


if __name__ == "__main__":
    unittest.main()

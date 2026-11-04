# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/release-evidence: the release evidence pack. A
checkout with anything uncommitted is refused; make ci's summary keeps each
step's result lines without timings or the container's paths; the grants
inventory lists every PD's memory, devices, interrupts and channels from the
real systems/*.grants; the cryptographic crates of every workspace make sbom
covers are listed with where they are used, the fuzz harness's and the glyph
atlas tool's as never shipped; and the index lists every file with its
sha256 - all but the raw logs - and says when the pack is incomplete.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only; needs git.
"""

import hashlib
import importlib.machinery
import importlib.util
import json
import os
import re
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    """Import an extensionless script in this directory as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


EVIDENCE = load("release-evidence")
CHECK_SYSTEM = load("check-system")
GIT_ENV = {"GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull, "GIT_AUTHOR_NAME": "Test",
           "GIT_AUTHOR_EMAIL": "test@example.invalid", "GIT_COMMITTER_NAME": "Test",
           "GIT_COMMITTER_EMAIL": "test@example.invalid"}


def git(path, *args):
    subprocess.run(["git", "-C", path, "-c", "commit.gpgsign=false", *args], check=True, capture_output=True,
                   env={**os.environ, **GIT_ENV})


def write(path, text):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(text)


def sbom(components):
    return json.dumps({"components": components})


class ReleaseEvidenceTest(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name

    def tearDown(self):
        self.tmp.cleanup()

    def test_a_checkout_with_anything_uncommitted_is_refused(self):
        repo = os.path.join(self.root, "repo")
        os.makedirs(repo)
        git(repo, "init", "-q", "-b", "main")
        write(os.path.join(repo, "a.txt"), "one\n")
        git(repo, "add", "-A")
        git(repo, "commit", "-q", "-m", "first")
        self.assertRegex(EVIDENCE.require_clean(repo), r"^[0-9a-f]{40}$")
        write(os.path.join(repo, "new.txt"), "untracked\n")
        with self.assertRaisesRegex(EVIDENCE.InputError, "uncommitted"):
            EVIDENCE.require_clean(repo)
        os.remove(os.path.join(repo, "new.txt"))
        write(os.path.join(repo, "a.txt"), "changed\n")
        with self.assertRaisesRegex(EVIDENCE.InputError, "a.txt"):
            EVIDENCE.require_clean(repo)

    def test_the_ci_summary_keeps_results_without_timings_or_paths(self):
        log = os.path.join(self.root, "step.log")
        write(log, "\n".join([
            "   Compiling nowayos-ui v0.1.0 (/work/nowayos/crates/nowayos-ui)",
            "test result: ok. 32 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s",
            "test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.29s",
            "check-system: PASS - /work/nowayos/build/qemu_virt_aarch64/hello/hello.system: description agree",
            "qemu-test: [  0.041s] found: NOWAYOS: hello",
            "qemu-test: PASS - all 2 line(s) seen in order; last after 0.039s; nothing forbidden in the 1s after it",
            "secrets-scan: PASS - /work/nowayos: 291 files in the tree; 735 file versions and 52 commit messages "
            "in the history, from every ref; 0 allowed findings",
            "Ran 187 tests in 3.582s",
            "OK",
            "advisories ok, bans ok, licenses ok, sources ok",
        ]) + "\n")
        self.assertEqual(EVIDENCE.summarise(log), [
            "check-system: PASS - nowayos/build/qemu_virt_aarch64/hello/hello.system: description agree",
            "qemu-test: PASS - all 2 line(s) seen in order; nothing forbidden in the 1s after it",
            "secrets-scan: PASS - nowayos: 291 files in the tree; every file version and commit message in the "
            "history, from every ref; 0 allowed findings",
            "Ran 187 tests",
            "OK",
            "advisories ok, bans ok, licenses ok, sources ok",
            "cargo tests: 38 passed, 0 failed",
        ])

    def test_the_inventory_lists_each_pds_grants_from_the_real_systems(self):
        boards = ["qemu_virt_aarch64", "rpi5b_2gb"]
        text = EVIDENCE.inventory(CHECK_SYSTEM, boards)
        self.assertEqual(text, EVIDENCE.inventory(CHECK_SYSTEM, boards))
        section = text.split("## ui-interactive\n", 1)[1].split("\n## ", 1)[0]
        self.assertIn("Kind: **demo**.", section)
        rows = {line.split(" | ")[0][2:]: line for line in section.splitlines() if line.startswith("| ")}
        ui = rows["ui"].split(" | ")
        self.assertEqual(ui[1], "ui")                       # its role
        self.assertEqual(ui[4:6], ["-", "-"])               # no devices, no interrupts
        self.assertIn("framebuffer rw", ui[3])
        self.assertIn("display (both ways)", ui[6])
        self.assertIn("fw_cfg rw", rows["display"])
        self.assertIn("requests", rows["display"].split(" | ")[8])  # learns the DMA region's address
        self.assertIn("virtio_mmio_24_31 78", rows["virtio_input"])
        self.assertIn("input_broker (only this PD notifies)", rows["virtio_input"])
        self.assertIn("virtio_input (only virtio_input notifies)", rows["input_broker"])
        restart = text.split("## restart-test\n", 1)[1].split("\n## ", 1)[0]
        self.assertIn("can restart worker on its own", restart)
        self.assertIn("restartable by restarter", restart)
        devices = text.split("## Devices on Each Board", 1)[1]
        self.assertIn("| fw_cfg | registers 0x9020000, 0x1000 bytes; no interrupt |", devices)
        self.assertIn("| phys_timer |", devices)       # granted only as an interrupt
        self.assertIn("not on this board", devices.split("| fw_cfg |", 1)[1].splitlines()[0])

    def test_a_board_that_lacks_a_systems_devices_leaves_it_out_naming_them(self):
        self.assertIsNone(EVIDENCE.board_refusal("hello", "rpi5b_2gb"))
        reason = EVIDENCE.board_refusal("ui-interactive", "rpi5b_2gb")
        self.assertEqual(reason, "rpi5b_2gb has no fw_cfg, virtio_mmio_24_31")

    def test_a_boards_own_variant_of_a_system_is_built_and_listed(self):
        # The Pi 5 has no phys_timer, but builds its own timer-demo.
        systems = os.path.join(HERE, os.pardir, "systems")
        self.assertIsNone(EVIDENCE.board_refusal("timer-demo", "rpi5b_2gb"))
        self.assertEqual(os.path.realpath(EVIDENCE.system_file("timer-demo", "rpi5b_2gb", "grants")),
                         os.path.realpath(os.path.join(systems, "rpi5b_2gb", "timer-demo.grants")))
        self.assertEqual(os.path.realpath(EVIDENCE.system_file("timer-demo", "qemu_virt_aarch64", "system")),
                         os.path.realpath(os.path.join(systems, "timer-demo.system")))
        self.assertIn(("rpi5b_2gb", "timer-demo"), EVIDENCE.variants(["qemu_virt_aarch64", "rpi5b_2gb"]))
        text = EVIDENCE.inventory(CHECK_SYSTEM, ["qemu_virt_aarch64", "rpi5b_2gb"])
        qemu = text.split("## timer-demo\n", 1)[1].split("\n## ", 1)[0]
        pi = text.split("## timer-demo (rpi5b_2gb)\n", 1)[1].split("\n## ", 1)[0]
        self.assertIn("| - | phys_timer |", qemu)
        self.assertIn("| system_timer rw | system_timer 97 |", pi)
        self.assertNotIn("system_timer", qemu)
        devices = text.split("## Devices on Each Board", 1)[1]
        self.assertIn("| system_timer | not on this board | registers 0x107c003000, 0x1000 bytes; "
                      "interrupts 96, 97, 98, 99 |", devices)

    def test_cryptographic_crates_are_listed_with_where_they_are_used(self):
        sboms = os.path.join(self.root, "sbom")
        images = os.path.join(self.root, "images")
        write(os.path.join(sboms, "nowayos.cdx.json"), sbom([
            {"name": "sha2", "version": "0.10.8", "scope": "required"},
            {"name": "ppv-lite86", "version": "0.2.21", "scope": "excluded",
             "properties": [{"name": "nowayos:excluded", "value": "test only: a dev-dependency"}]},
            {"name": "serde", "version": "1.0.0", "scope": "required"}]))
        write(os.path.join(sboms, "nowayos-pds.cdx.json"), sbom([]))
        write(os.path.join(images, "qemu_virt_aarch64", "debug", "demo", "sbom.cdx.json"), sbom([
            {"name": "sha2", "version": "0.10.8", "scope": "required"}]))
        listed = os.path.join(self.root, "crypto-crates.txt")
        write(listed, "# a comment\nsha2\n")
        packages = {"nowayos": [
            {"name": "sha2", "version": "0.10.8", "categories": []},
            {"name": "ppv-lite86", "version": "0.2.21", "categories": ["cryptography", "no-std"]},
            {"name": "serde", "version": "1.0.0", "categories": ["encoding"]}], "nowayos-pds": []}
        text = EVIDENCE.crypto(packages, sboms, images, listed)
        self.assertIn("**In what ships: sha2 0.10.8.**", text)
        self.assertIn("| sha2 | 0.10.8 | named in tools/crypto-crates.txt | nowayos: shipped | "
                      "qemu_virt_aarch64/debug/demo |", text)
        self.assertIn("| ppv-lite86 | 0.2.21 | crates.io: cryptography | nowayos: test only | - |", text)
        self.assertNotIn("serde", text)
        packages["nowayos"] = packages["nowayos"][1:]
        self.assertIn("**In what ships: none.**", EVIDENCE.crypto(packages, sboms, images, listed))

    def test_crates_of_the_workspaces_that_never_ship_are_listed_as_never_shipped(self):
        # Regression: the glyph atlas tool's workspace, with
        # sha2 and its traits, and the fuzz harness's were not read at all;
        # and the atlas tool's SBOM marks what the tool is built from
        # `required`, which is not what ships.
        sboms = os.path.join(self.root, "sbom")
        excluded = [{"name": "nowayos:excluded",
                     "value": "host only: a workspace crate that never ships, or its dependency"}]
        write(os.path.join(sboms, "nowayos.cdx.json"), sbom([]))
        write(os.path.join(sboms, "nowayos-pds.cdx.json"), sbom([]))
        write(os.path.join(sboms, "nowayos-fuzz.cdx.json"), sbom([
            {"name": "getrandom", "version": "0.3.4", "scope": "excluded", "properties": excluded}]))
        write(os.path.join(sboms, "nowayos-glyph-atlas.cdx.json"), sbom([
            {"name": "sha2", "version": "0.10.9", "scope": "required"},
            {"name": "digest", "version": "0.10.7", "scope": "required"}]))
        listed = os.path.join(self.root, "crypto-crates.txt")
        write(listed, "sha2\ndigest\ngetrandom\n")
        packages = {"nowayos": [], "nowayos-pds": [],
                    "nowayos-fuzz": [{"name": "getrandom", "version": "0.3.4", "categories": []}],
                    "nowayos-glyph-atlas": [{"name": "sha2", "version": "0.10.9", "categories": []},
                                            {"name": "digest", "version": "0.10.7", "categories": []}]}
        text = EVIDENCE.crypto(packages, sboms, os.path.join(self.root, "images"), listed)
        self.assertIn("**In what ships: none.**", text)
        self.assertIn("| sha2 | 0.10.9 | named in tools/crypto-crates.txt | "
                      "nowayos-glyph-atlas: build tool, never shipped | - |", text)
        self.assertIn("| digest | 0.10.7 | named in tools/crypto-crates.txt | "
                      "nowayos-glyph-atlas: build tool, never shipped | - |", text)
        self.assertIn("| getrandom | 0.3.4 | named in tools/crypto-crates.txt | nowayos-fuzz: host only | - |", text)
        self.assertIn("`.`, `pds`, `fuzz`, `tools/glyph-atlas` and `tools/pictures`", text)

    def test_every_workspace_make_sbom_writes_is_read_for_crypto(self):
        with open(os.path.join(HERE, os.pardir, "Makefile"), encoding="utf-8") as handle:
            makefile = handle.read()
        sbom_names = set(re.findall(r"tools/sbom --workspace \S+ --name (\S+)", makefile))
        self.assertEqual(len(sbom_names), 5, makefile)
        self.assertEqual({name for name, _path, _what in EVIDENCE.WORKSPACES}, sbom_names)
        for name, path, _what in EVIDENCE.WORKSPACES:
            self.assertTrue(os.path.isfile(os.path.join(HERE, os.pardir, path, "Cargo.lock")), name)

    def test_an_sbom_of_a_workspace_crypto_does_not_read_is_refused(self):
        sboms = os.path.join(self.root, "sbom")
        for name in ("nowayos", "nowayos-pds", "nowayos-other"):
            write(os.path.join(sboms, f"{name}.cdx.json"), sbom([]))
        listed = os.path.join(self.root, "crypto-crates.txt")
        write(listed, "sha2\n")
        with self.assertRaises(EVIDENCE.InputError) as raised:
            EVIDENCE.crypto({"nowayos": [], "nowayos-pds": []}, sboms, os.path.join(self.root, "images"), listed)
        self.assertIn("nowayos-other", str(raised.exception))
        self.assertIn("WORKSPACES", str(raised.exception))

    def test_the_index_lists_every_file_but_the_logs_and_says_when_incomplete(self):
        out = os.path.join(self.root, "pack")
        write(os.path.join(out, "versions.txt"), "pins\n")
        write(os.path.join(out, "images", "SHA256SUMS"), "sums\n")
        write(os.path.join(out, "ci", "logs", "lint.log"), "timings\n")
        ci = [("secrets", "PASS"), ("lint", "FAIL"), ("test", "not run")]
        EVIDENCE.index(out, "0123456789abcdef" * 2 + "01234567", "2026-10-04T08:00:00+00:00", None, ci, [], [],
                       complete=False)
        with open(os.path.join(out, "INDEX.md"), encoding="utf-8") as handle:
            text = handle.read()
        self.assertIn("INCOMPLETE", text)
        self.assertIn("| lint | FAIL |", text)
        self.assertIn(f"| `versions.txt` | {hashlib.sha256(b'pins' + bytes([10])).hexdigest()} |", text)
        self.assertIn(f"| `images/SHA256SUMS` | {hashlib.sha256(b'sums' + bytes([10])).hexdigest()} |", text)
        self.assertNotIn("lint.log", text)
        for item, answer in EVIDENCE.PER_RELEASE:
            self.assertIn(f"| {item} | {answer} |", text)
        self.assertEqual(len(EVIDENCE.PER_RELEASE), 7)
        self.assertTrue(any("signing keys" in answer for _item, answer in EVIDENCE.PER_RELEASE))


if __name__ == "__main__":
    unittest.main()

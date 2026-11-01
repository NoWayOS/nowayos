# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/update-bundles: keys are made in a temporary
folder that is gone when the run ends, only public keys and signed records
are written, each record has its layout's length and header, a DER signature
becomes r then s, and a key the tool made, committed to a scratch repository,
is found by the secrets scan's private-key rule.

The records' contents - that each decodes, each OpenSSL signature verifies
with NoWayOS's RustCrypto check, and each bad bundle is refused for its
reason - are nowayos-update's `openssl_bundles` test.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only; openssl on PATH (the dev image).
"""

import contextlib
import glob
import importlib.machinery
import importlib.util
import io
import json
import os
import struct
import subprocess
import sys
import tempfile
import unittest
import unittest.mock

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    """Import an extensionless script in this directory as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


BUNDLES = load("update-bundles")
SCAN = load("secrets-scan")

GIT_ENV = {
    "GIT_AUTHOR_NAME": "Test",
    "GIT_AUTHOR_EMAIL": "test@example.invalid",
    "GIT_COMMITTER_NAME": "Test",
    "GIT_COMMITTER_EMAIL": "test@example.invalid",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
}


class MadeOnce(unittest.TestCase):
    """One run of the tool, shared by the tests that read what it wrote."""

    @classmethod
    def setUpClass(cls):
        cls.temp = tempfile.TemporaryDirectory()
        cls.out = os.path.join(cls.temp.name, "out")
        # The tool's key folder goes in a temporary folder of this class's own,
        # so another test running at the same time cannot add one to it.
        cls.keys_parent = os.path.join(cls.temp.name, "tmp")
        os.mkdir(cls.keys_parent)
        with contextlib.redirect_stdout(io.StringIO()), \
                unittest.mock.patch.object(tempfile, "tempdir", cls.keys_parent):
            cls.index = BUNDLES.make(cls.out)

    @classmethod
    def tearDownClass(cls):
        cls.temp.cleanup()

    def read(self, name):
        with open(os.path.join(self.out, name), "rb") as handle:
            return handle.read()

    def test_the_key_folder_is_gone_after_the_run(self):
        self.assertEqual(glob.glob(os.path.join(self.keys_parent, "update-keys-*")), [])

    def test_no_private_key_is_written(self):
        for path in glob.glob(os.path.join(self.out, "*")):
            with open(path, "rb") as handle:
                data = handle.read()
            self.assertNotIn(b"PRIVATE KEY", data, path)
            self.assertNotIn(b"-----BEGIN", data, path)

    def test_every_record_has_its_length_and_header(self):
        self.assertEqual(len(self.read("anchors.bin")), BUNDLES.KEY_LEN)
        for name in ("list-v0.rec", "list-v1.rec", "list-v2.rec"):
            record = self.read(name)
            self.assertEqual(len(record), BUNDLES.HEADER_LEN + BUNDLES.LIST_PAYLOAD_LEN, name)
            self.assertEqual(struct.unpack("<HHHH", record[:8]), (1, 1, 784, 0), name)
        for entry in self.index["bundles"]:
            data = self.read(entry["file"])
            self.assertEqual(len(data) % 512, 0, entry["name"])
            for at in range(0, len(data), 512):
                self.assertEqual(struct.unpack("<HHHH", data[at:at + 8]), (3, 1, 504, 0), entry["name"])

    def test_the_index_names_every_bundle_and_its_refusal(self):
        names = [entry["name"] for entry in self.index["bundles"]]
        self.assertEqual(names, ["good", "unsigned", "unknown-key", "removed-key", "list-version",
                                 "development", "security-version", "hash"])
        with open(os.path.join(self.out, "bundles.json"), encoding="utf-8") as handle:
            self.assertEqual(json.load(handle), self.index)


class DerSignatures(unittest.TestCase):
    def test_r_and_s_come_out_as_32_bytes_each(self):
        r = bytes([0x80] + [1] * 31)  # a high bit: DER puts a zero before it
        s = bytes([0, 0, 5] + [2] * 29)  # leading zeros: DER drops them
        der_r = b"\x02\x21\x00" + r
        der_s = b"\x02\x1e" + s[2:]
        der = b"\x30" + bytes([len(der_r) + len(der_s)]) + der_r + der_s
        self.assertEqual(BUNDLES.der_signature(der), r + s)

    def test_a_malformed_signature_stops_the_tool(self):
        for der in (b"\x31\x00", b"\x30\x03\x02\x01\x01", b"\x30\x06\x02\x01\x01\x02\x01\x01\x00"):
            with self.assertRaises(SystemExit):
                BUNDLES.der_signature(der)


class SecretsScan(unittest.TestCase):
    def test_a_key_the_tool_made_is_found_when_committed(self):
        with tempfile.TemporaryDirectory() as folder:
            key = BUNDLES.Key(folder, "leaked")
            repo = os.path.join(folder, "scratch")
            os.makedirs(repo)

            def git(*args):
                subprocess.run(["git", "-C", repo, "-c", "commit.gpgsign=false", *args], check=True,
                               capture_output=True, env={**os.environ, **GIT_ENV})

            git("init", "-q", "-b", "main")
            with open(key.path, "rb") as source, open(os.path.join(repo, "test-root.pem"), "wb") as copy:
                copy.write(source.read())
            git("add", "-A")
            git("commit", "-q", "-m", "a test key, committed by mistake")
            out = io.StringIO()
            saved = sys.argv
            sys.argv = ["secrets-scan", "--repo", repo]
            try:
                with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
                    status = SCAN.main()
            finally:
                sys.argv = saved
            self.assertEqual(status, SCAN.EXIT_FAIL, out.getvalue())
            self.assertIn("test-root.pem", out.getvalue())
            self.assertIn("private-key", out.getvalue())


if __name__ == "__main__":
    unittest.main()

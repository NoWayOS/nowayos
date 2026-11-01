# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/kani-mutation: each named mutation removes
exactly its check, and refuses to run when that code has changed; each names
a proof that exists in its crate; a mutant whose proof passes, or fails
anywhere but where the missing check must show, is a failure; mutations are
chosen by name, and an unknown name is refused. And a product repo's own
mutations: read from its JSON file, each judged by where it must
fail, and its mutant's "../" paths made absolute.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only.
"""

import contextlib
import importlib.machinery
import importlib.util
import io
import os
import re
import sys
import unittest
from unittest import mock

HERE = os.path.dirname(os.path.abspath(__file__))
NOWAYOS = os.path.dirname(HERE)


def load(name):
    """Import an extensionless script in this directory as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


MUTATION = load("kani-mutation")
READ_BYTES = MUTATION.BY_NAME["read-bytes-length"]
DATA_REGION = MUTATION.BY_NAME["data-region-bounds"]
DEQUEUE = MUTATION.BY_NAME["dequeue-in-flight"]

FAILED_LOG = """Checking harness proofs::header_parse_any_input...
SUMMARY:
 ** 1 of 240 failed
Failed Checks: range end index out of range for slice
 File: "crates/nowayos-ipc/src/codec.rs", line 167, in codec::Reader::<'_>::read_bytes

VERIFICATION:- FAILED
"""

REGION_LOG = """Checking harness proofs::data_region_accepts_only_buffers_inside_it...
SUMMARY:
 ** 2 of 174 failed
Failed Checks: "a descriptor outside the region was accepted"
 File: "crates/nowayos-net/src/proofs.rs", line 213, in proofs::data_region_accepts_only_buffers_inside_it
Failed Checks: assertion failed: buffer < buffers
 File: "crates/nowayos-net/src/proofs.rs", line 215, in proofs::data_region_accepts_only_buffers_inside_it

VERIFICATION:- FAILED
"""


def read(mutation):
    with open(os.path.join(NOWAYOS, mutation.path), encoding="utf-8") as handle:
        return handle.read()


class KaniMutationTest(unittest.TestCase):

    def test_each_real_file_still_has_the_check_its_mutation_removes(self):
        for mutation in MUTATION.MUTATIONS:
            with self.subTest(mutation.name):
                text = read(mutation)
                mutant = MUTATION.mutate(mutation, text)
                self.assertNotIn(mutation.checked, mutant)
                # Only the check changed: the text around it is as it was.
                at = text.index(mutation.checked)
                self.assertEqual(mutant[:at], text[:at])
                self.assertEqual(mutant[at:at + len(mutation.unchecked)], mutation.unchecked)
                self.assertEqual(mutant[at + len(mutation.unchecked):], text[at + len(mutation.checked):])

    def test_each_mutation_names_a_proof_in_its_crate(self):
        for mutation in MUTATION.MUTATIONS:
            with self.subTest(mutation.name):
                proofs = os.path.join(NOWAYOS, "crates", mutation.crate, "src", "proofs.rs")
                with open(proofs, encoding="utf-8") as handle:
                    text = handle.read()
                self.assertRegex(text, r"#\[kani::proof\]\n(#\[kani::[a-z_]+\([^)]*\)\]\n)*fn "
                                 + re.escape(mutation.harness) + r"\(\)")
                self.assertTrue(mutation.path.startswith(os.path.join("crates", mutation.crate, "")))

    def test_each_assertion_a_mutation_expects_is_in_its_proof(self):
        for mutation in (DATA_REGION, DEQUEUE):
            with self.subTest(mutation.name):
                with open(os.path.join(NOWAYOS, "crates", mutation.crate, "src", "proofs.rs"),
                          encoding="utf-8") as handle:
                    text = handle.read()
                self.assertTrue(mutation.assertion)
                self.assertIn(f'"{mutation.assertion}"', text)

    def test_the_names_are_unique(self):
        self.assertEqual(len(MUTATION.BY_NAME), len(MUTATION.MUTATIONS))

    def test_changed_code_is_refused_rather_than_left_unmutated(self):
        for mutation in MUTATION.MUTATIONS:
            with self.subTest(mutation.name):
                with self.assertRaisesRegex(MUTATION.MutationError, "no longer has"):
                    MUTATION.mutate(mutation, "fn changed() {}\n")
                with self.assertRaisesRegex(MUTATION.MutationError, "update the mutation's checked text"):
                    MUTATION.mutate(mutation, mutation.checked * 2)

    def test_a_failure_in_read_bytes_is_the_pass(self):
        verdict = MUTATION.judge(READ_BYTES, 1, FAILED_LOG)
        self.assertIn("out-of-bounds slice index", verdict)
        self.assertIn("read_bytes", verdict)

    def test_a_slice_index_panic_raised_in_core_is_the_pass(self):
        in_core = ("Failed Checks: This is a placeholder message; Kani doesn't support message formatted at "
                   "runtime\n File: \"/home/runner/.rustup/toolchains/nightly/lib/rustlib/src/rust/library/core/"
                   "src/slice/index.rs\", line 50, in core::slice::index::slice_index_fail::do_panic::runtime\n"
                   "VERIFICATION:- FAILED\n")
        self.assertIn("slice_index_fail", MUTATION.judge(READ_BYTES, 1, in_core))

    def test_the_region_proofs_assertion_is_the_pass(self):
        verdict = MUTATION.judge(DATA_REGION, 1, REGION_LOG)
        self.assertIn("'a descriptor outside the region was accepted'", verdict)
        self.assertIn("proofs.rs", verdict)

    def test_a_mutant_that_passes_is_a_failure(self):
        for mutation in MUTATION.MUTATIONS:
            with self.subTest(mutation.name):
                with self.assertRaisesRegex(MUTATION.MutationError, "do not test what they claim"):
                    MUTATION.judge(mutation, 0, "VERIFICATION:- SUCCESSFUL\n")
                with self.assertRaisesRegex(MUTATION.MutationError, "do not test what they claim"):
                    MUTATION.judge(mutation, 1, "error: could not compile\n")

    def test_a_failure_elsewhere_is_a_failure(self):
        elsewhere = FAILED_LOG.replace("codec.rs", "proofs.rs").replace("read_bytes", "header_parse_any_input")
        with self.assertRaisesRegex(MUTATION.MutationError, "not on an out-of-bounds slice index"):
            MUTATION.judge(READ_BYTES, 1, elsewhere)
        # The region's mutant failing on some other assertion, or on the
        # expected text outside the proofs, is not the pass either.
        other = REGION_LOG.replace('"a descriptor outside the region was accepted"', "assertion failed: inside")
        with self.assertRaisesRegex(MUTATION.MutationError, "not on its assertion"):
            MUTATION.judge(DATA_REGION, 1, other)
        in_code = REGION_LOG.replace("src/proofs.rs", "src/queue.rs")
        with self.assertRaisesRegex(MUTATION.MutationError, "not on its assertion"):
            MUTATION.judge(DATA_REGION, 1, in_code)
        # Nor is one mutant's failure taken for another's.
        with self.assertRaisesRegex(MUTATION.MutationError, "not on its assertion"):
            MUTATION.judge(DEQUEUE, 1, REGION_LOG)

    def test_mutations_are_chosen_by_name(self):
        self.assertEqual(MUTATION.select([]), list(MUTATION.MUTATIONS))
        self.assertEqual(MUTATION.select(["dequeue-in-flight", "read-bytes-length", "dequeue-in-flight"]),
                         [DEQUEUE, READ_BYTES])
        with self.assertRaisesRegex(MUTATION.MutationError, "no mutation named no-such-check"):
            MUTATION.select(["data-region-bounds", "no-such-check"])

    def test_an_unknown_name_fails_the_run_and_runs_nothing(self):
        stderr = io.StringIO()
        with mock.patch.object(sys, "argv", ["kani-mutation", "--out", "unused", "--mutation", "nope"]), \
                mock.patch.object(MUTATION, "run") as run, contextlib.redirect_stderr(stderr):
            self.assertEqual(MUTATION.main(), 1)
        run.assert_not_called()
        self.assertIn("no mutation named nope", stderr.getvalue())

    def test_every_mutation_runs_and_one_failure_fails_the_run(self):
        def run(mutation, out, root):
            if mutation is DATA_REGION:
                raise MUTATION.MutationError("data-region-bounds: the proof passed")
            return f"{mutation.name}: ok"
        stdout, stderr = io.StringIO(), io.StringIO()
        with mock.patch.object(sys, "argv", ["kani-mutation", "--out", "unused"]), \
                mock.patch.object(MUTATION, "run", side_effect=run) as runs, \
                contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
            self.assertEqual(MUTATION.main(), 1)
        self.assertEqual([call.args[0] for call in runs.call_args_list], list(MUTATION.MUTATIONS))
        self.assertEqual(stdout.getvalue().count("kani-mutation: PASS"), len(MUTATION.MUTATIONS) - 1)
        self.assertIn("kani-mutation: FAIL - data-region-bounds", stderr.getvalue())



SPLIT_AT_LOG = """Checking harness proofs::decode_action_any_input...
Failed Checks: mid > len
 File: "/rustc/library/core/src/slice/mod.rs", line 2012, in core::slice::<impl [T]>::split_at

VERIFICATION:- FAILED
"""

PRODUCT_MUTATION = {
    "name": "set-text-length",
    "crate": "showcase-kiosk-contract",
    "path": "kiosk/showcase-kiosk-contract/src/action.rs",
    "harness": "decode_action_any_input",
    "what": "SetText's length check",
    "checked": "checked\n",
    "unchecked": "unchecked\n",
    "expect_where": "slice/mod.rs",
    "expected_text": "a panic in slice::split_at",
}


class ProductMutationTest(unittest.TestCase):

    def load(self, mutations):
        import json
        import tempfile
        with tempfile.NamedTemporaryFile("w", suffix=".json", delete=False) as handle:
            json.dump({"mutations": mutations}, handle)
        try:
            return MUTATION.load_mutations(handle.name)
        finally:
            os.unlink(handle.name)

    def test_a_product_mutation_is_read_and_judged_by_where_it_fails(self):
        (mutation,) = self.load([PRODUCT_MUTATION])
        self.assertEqual(mutation.path, os.path.join("kiosk", "showcase-kiosk-contract", "src", "action.rs"))
        self.assertIn("decode_action_any_input fails as it must",
                      MUTATION.judge(mutation, 1, SPLIT_AT_LOG))
        with self.assertRaises(MUTATION.MutationError):
            MUTATION.judge(mutation, 1, FAILED_LOG)
        with self.assertRaises(MUTATION.MutationError):
            MUTATION.judge(mutation, 0, "VERIFICATION:- SUCCESSFUL")

    def test_a_mutation_that_says_nothing_of_where_it_fails_is_refused(self):
        partial = {key: value for key, value in PRODUCT_MUTATION.items() if key != "expect_where"}
        with self.assertRaises(MUTATION.MutationError):
            self.load([partial])
        with self.assertRaises(MUTATION.MutationError):
            self.load([])

    def test_a_mutant_finds_the_sibling_clones_by_absolute_paths(self):
        manifest = 'a = { path = "../nowayos/crates/a" }\nb = { path = "kiosk/b" }\n'
        self.assertEqual(MUTATION.absolute_paths(manifest, "/work/"),
                         'a = { path = "/work/nowayos/crates/a" }\nb = { path = "kiosk/b" }\n')


if __name__ == "__main__":
    unittest.main()

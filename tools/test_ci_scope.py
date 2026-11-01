# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/ci-scope: what a change since a base can reach."""
import importlib.machinery
import importlib.util
import os
import subprocess
import tempfile
import textwrap
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_loader = importlib.machinery.SourceFileLoader("ci_scope", os.path.join(HERE, "ci-scope"))
_spec = importlib.util.spec_from_loader("ci_scope", _loader)
ci_scope = importlib.util.module_from_spec(_spec)
_loader.exec_module(ci_scope)


def write(root, path, text):
    full = os.path.join(root, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w", encoding="utf-8") as handle:
        handle.write(textwrap.dedent(text))


def git(root, *args):
    subprocess.run(["git", "-C", root, *args], check=True, capture_output=True,
                   env={**os.environ, "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
                        "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid"})


class CiScopeTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        write(self.root, "Cargo.lock", "# lock\n")
        write(self.root, "crates/nowayos-a/Cargo.toml", '[package]\nname = "nowayos-a"\n')
        write(self.root, "crates/nowayos-a/src/lib.rs", "pub fn a() {}\n")
        write(self.root, "crates/nowayos-b/Cargo.toml",
              '[package]\nname = "nowayos-b"\n[dependencies]\nnowayos-a = { path = "../nowayos-a" }\n')
        write(self.root, "crates/nowayos-b/src/lib.rs", "pub fn b() {}\n")
        write(self.root, "crates/nowayos-c/Cargo.toml", '[package]\nname = "nowayos-c"\n')
        write(self.root, "crates/nowayos-c/src/lib.rs", "pub fn c() {}\n")
        write(self.root, "fuzz/fuzz_targets/b_target.rs", "use nowayos_b::b;\n")
        write(self.root, "fuzz/fuzz_targets/c_target.rs", "use nowayos_c::c;\n")
        write(self.root, "tools/kani-mutation", """\
            import sys
            print("a-mutant (#1): nowayos-a, a check; a_proof must fail")
            print("c-mutant (#2): nowayos-c, a check; c_proof must fail")
            """)
        git(self.root, "init", "-q")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "base")

    def tearDown(self):
        self.tmp.cleanup()

    def scope(self, kind, items=()):
        return ci_scope.scope(self.root, "HEAD", kind, list(items))

    def test_nothing_changed_reaches_nothing(self):
        self.assertEqual(self.scope("kani", ["nowayos-a", "nowayos-b"]), [])
        self.assertEqual(self.scope("fuzz", ["b_target", "c_target"]), [])
        self.assertEqual(self.scope("mutations"), [])

    def test_a_change_reaches_its_crate_and_every_crate_that_depends_on_it(self):
        write(self.root, "crates/nowayos-a/src/lib.rs", "pub fn a() { }\n")
        self.assertEqual(self.scope("kani", ["nowayos-a", "nowayos-b", "nowayos-c"]), ["nowayos-a", "nowayos-b"])
        self.assertEqual(self.scope("fuzz", ["b_target", "c_target"]), ["b_target"])
        self.assertEqual(self.scope("mutations"), ["a-mutant"])

    def test_an_untracked_file_counts(self):
        write(self.root, "crates/nowayos-c/src/new.rs", "\n")
        self.assertEqual(self.scope("kani", ["nowayos-a", "nowayos-c"]), ["nowayos-c"])

    def test_a_changed_fuzz_target_reaches_itself(self):
        write(self.root, "fuzz/fuzz_targets/c_target.rs", "use nowayos_c::c; // changed\n")
        self.assertEqual(self.scope("fuzz", ["b_target", "c_target"]), ["c_target"])

    def test_a_change_to_what_builds_everything_runs_everything(self):
        write(self.root, "Cargo.lock", "# lock, changed\n")
        self.assertEqual(self.scope("kani", ["nowayos-a", "nowayos-c"]), ["nowayos-a", "nowayos-c"])
        self.assertEqual(self.scope("mutations"), ["a-mutant", "c-mutant"])

    def test_a_workspace_dependency_counts(self):
        write(self.root, "crates/nowayos-d/Cargo.toml",
              '[package]\nname = "nowayos-d"\n[dependencies]\nnowayos-a.workspace = true\n')
        write(self.root, "crates/nowayos-d/src/lib.rs", "pub fn d() {}\n")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "d")
        write(self.root, "crates/nowayos-a/src/lib.rs", "pub fn a() { }\n")
        self.assertEqual(self.scope("kani", ["nowayos-c", "nowayos-d"]), ["nowayos-d"])

    def test_a_change_to_the_fuzz_harness_reaches_every_target(self):
        write(self.root, "fuzz/src/lib.rs", "pub fn check() {}\n")
        self.assertEqual(self.scope("fuzz", ["b_target", "c_target"]), ["b_target", "c_target"])
        self.assertEqual(self.scope("kani", ["nowayos-a", "nowayos-c"]), [])

    def test_a_base_git_cannot_find_runs_everything(self):
        self.assertEqual(ci_scope.scope(self.root, "no-such-ref", "fuzz", ["b_target", "c_target"]),
                         ["b_target", "c_target"])


class ProductRepoTest(unittest.TestCase):
    """A product repo: its crates are its workspace's members, which
    depend on each other with `.workspace = true` and on NoWayOS's, outside
    the repo; a new NoWayOS commit (nowayos.rev) reaches everything."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        write(self.root, "Cargo.toml", '''\
            [workspace]
            members = [
                "kiosk/*",
                "showcase/one",
            ]
            ''')
        write(self.root, "nowayos.rev", "abc\n")
        write(self.root, "kiosk/flow/Cargo.toml",
              '[package]\nname = "p-flow"\n[dependencies]\nnowayos-ipc.workspace = true\n')
        write(self.root, "kiosk/flow/src/lib.rs", "\n")
        write(self.root, "kiosk/contract/Cargo.toml",
              '[package]\nname = "p-contract"\n[dependencies]\np-flow.workspace = true\n')
        write(self.root, "kiosk/contract/src/lib.rs", "\n")
        write(self.root, "showcase/one/Cargo.toml", '[package]\nname = "p-one"\n')
        write(self.root, "showcase/one/src/lib.rs", "\n")
        write(self.root, "showcase/two/Cargo.toml", '[package]\nname = "p-two"\n')
        write(self.root, "fuzz/fuzz_targets/contract_decode.rs", "use p_contract::decode;\n")
        write(self.root, "fuzz/fuzz_targets/one_decode.rs", "use p_one::decode;\n")
        git(self.root, "init", "-q")
        git(self.root, "add", "-A")
        git(self.root, "commit", "-q", "-m", "base")

    def tearDown(self):
        self.tmp.cleanup()

    def scope(self, kind, items):
        return ci_scope.scope(self.root, "HEAD", kind, items)

    def test_its_members_are_its_crates(self):
        self.assertEqual(sorted(ci_scope.crates(self.root)), ["p-contract", "p-flow", "p-one"])

    def test_a_change_reaches_the_members_that_depend_on_it(self):
        write(self.root, "kiosk/flow/src/lib.rs", "// changed\n")
        self.assertEqual(self.scope("kani", ["p-contract", "p-one"]), ["p-contract"])
        self.assertEqual(self.scope("fuzz", ["contract_decode", "one_decode"]), ["contract_decode"])

    def test_a_new_nowayos_reaches_everything(self):
        write(self.root, "nowayos.rev", "def\n")
        self.assertEqual(self.scope("kani", ["p-contract", "p-one"]), ["p-contract", "p-one"])
        self.assertEqual(self.scope("fuzz", ["contract_decode", "one_decode"]),
                         ["contract_decode", "one_decode"])


if __name__ == "__main__":
    unittest.main()

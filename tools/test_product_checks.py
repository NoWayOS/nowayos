# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for the checks a product repo's PDs get from NoWayOS:
tools/check-git-pins (git sources only at the commits NoWayOS pins),
tools/check-unsafe --no-allowlist (no unsafe code anywhere in the repo) and
check-baseline's local-crate guard across a repo's two workspaces.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only.
"""

import contextlib
import importlib.machinery
import importlib.util
import io
import os
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


def run_main(module, argv):
    """Run a tool's main() with `argv`; return (status, stdout + stderr)."""
    out = io.StringIO()
    saved = sys.argv
    sys.argv = [module.__name__] + argv
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            status = module.main()
    finally:
        sys.argv = saved
    return status, out.getvalue()


def write(directory, name, text):
    path = os.path.join(directory, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(textwrap.dedent(text))
    return path


PIN = "d441a880938209f661201704dfbee7b91c5aa905"
OTHER = "0" * 40
RUST_SEL4 = "https://github.com/seL4/rust-sel4"
# A product's own git dependency (a fork of a library).
FORK = "https://git.example.invalid/project/fork"
FORK_PIN = "8ab3351b1cace5bdcc085619488967bd7f8f398c"

PINS = f"""\
[workspace]
members = []

[workspace.dependencies]
sel4-microkit = {{ git = "{RUST_SEL4}", rev = "{PIN}", default-features = false }}
sel4 = {{ git = "{RUST_SEL4}", rev = "{PIN}" }}
safe-mmio = "0.3.2"
"""


def lock(*packages):
    entries = ["version = 4\n"]
    for name, source in packages:
        entries.append(f'[[package]]\nname = "{name}"\nversion = "0.1.0"\n'
                       + (f'source = "{source}"\n' if source else "") + "\n")
    return "\n".join(entries)


def git(url, commit):
    return f"git+{url}?rev={commit}#{commit}"


class CheckGitPins(unittest.TestCase):
    tool = load("check-git-pins")

    def check(self, lock_text, pins_text=PINS, product_text=None):
        with tempfile.TemporaryDirectory() as tmp:
            pins = write(tmp, "nowayos/pds/Cargo.toml", pins_text)
            lock_file = write(tmp, "product/pds/Cargo.lock", lock_text)
            if product_text is not None:
                write(tmp, "product/pds/Cargo.toml", product_text)
            return run_main(self.tool, ["--lock", lock_file, "--pins", pins])

    def test_the_pinned_commit_passes(self):
        status, output = self.check(lock(("sel4-microkit", git(RUST_SEL4, PIN)), ("sel4", git(RUST_SEL4, PIN)),
                                         ("product-pd", None), ("cfg-if", "registry+https://github.com/rust-lang/crates.io-index")))
        self.assertEqual(status, 0, output)
        self.assertIn(f"{RUST_SEL4} at {PIN}", output)

    def test_no_git_sources_passes(self):
        status, output = self.check(lock(("product-pd", None)))
        self.assertEqual(status, 0, output)
        self.assertIn("no git sources", output)

    def test_another_commit_fails(self):
        status, output = self.check(lock(("sel4-microkit", git(RUST_SEL4, OTHER))))
        self.assertEqual(status, 1)
        self.assertIn(f"at {OTHER}, but", output)
        self.assertIn(f"pins {PIN}", output)

    def test_another_repository_fails(self):
        status, output = self.check(lock(("sel4-microkit", git("https://github.com/someone/rust-sel4-fork", PIN))))
        self.assertEqual(status, 1)
        self.assertIn("nor the product's manifest pins", output)

    def test_a_branch_source_with_the_same_commit_counts_by_its_commit(self):
        source = f"git+{RUST_SEL4}?branch=main#{PIN}"
        status, output = self.check(lock(("sel4-microkit", source)))
        self.assertEqual(status, 0, output)

    def product(self, url, rev, sel4_rev=PIN):
        return (f'[workspace]\nmembers = []\n\n[workspace.dependencies]\n'
                f'sel4-microkit = {{ git = "{RUST_SEL4}", rev = "{sel4_rev}" }}\n'
                f'fork = {{ git = "{url}", rev = "{rev}", default-features = false }}\n')

    def test_the_products_own_pin_passes(self):
        status, output = self.check(lock(("sel4-microkit", git(RUST_SEL4, PIN)), ("fork", git(FORK, FORK_PIN))),
                                    product_text=self.product(FORK, FORK_PIN))
        self.assertEqual(status, 0, output)
        self.assertIn(f"{FORK} at {FORK_PIN} (the product's own pin)", output)
        self.assertIn(f"{RUST_SEL4} at {PIN}", output)

    def test_the_products_own_repository_at_another_commit_fails(self):
        status, output = self.check(lock(("fork", git(FORK, OTHER))), product_text=self.product(FORK, FORK_PIN))
        self.assertEqual(status, 1)
        self.assertIn(f"but the product's manifest pins {FORK_PIN}", output)

    def test_a_repository_neither_pins_fails(self):
        status, output = self.check(lock(("fork", git(FORK, FORK_PIN))), product_text=self.product(FORK + "-other", FORK_PIN))
        self.assertEqual(status, 1)
        self.assertIn("neither", output)

    def test_the_product_cannot_move_the_nowayos_pin(self):
        status, output = self.check(lock(("sel4-microkit", git(RUST_SEL4, OTHER))),
                                    product_text=self.product(FORK, FORK_PIN, sel4_rev=OTHER))
        self.assertEqual(status, 2)
        self.assertIn("NoWayOS's pin rules", output)

    def test_the_products_own_pin_must_be_a_full_commit_hash(self):
        status, output = self.check(lock(), product_text=self.product(FORK, FORK_PIN).replace(f'rev = "{FORK_PIN}"', 'branch = "main"'))
        self.assertEqual(status, 2)
        self.assertIn("without a full commit hash", output)

    def test_a_pin_must_be_a_full_commit_hash(self):
        status, output = self.check(lock(), PINS.replace(f'rev = "{PIN}" }}\nsafe', 'tag = "v5.0.0" }\nsafe'))
        self.assertEqual(status, 2)
        self.assertIn("without a full commit hash", output)


class CheckUnsafeNoAllowlist(unittest.TestCase):
    """check-unsafe --no-allowlist: a product repo with no approved crate."""

    tool = load("check-unsafe")
    LINTS = """\
        [workspace.lints.rust]
        unsafe_code = "forbid"
        """

    def repo(self, tmp, source):
        write(tmp, "Cargo.toml", '[workspace]\nmembers = ["app/*"]\n\n' + textwrap.dedent(self.LINTS))
        write(tmp, "app/flow/Cargo.toml", '[package]\nname = "flow"\nversion = "0.1.0"\n\n[lints]\nworkspace = true\n')
        write(tmp, "app/flow/src/lib.rs", "pub fn f() {}\n")
        write(tmp, "pds/Cargo.toml", '[workspace]\nmembers = ["pd"]\n\n' + textwrap.dedent(self.LINTS))
        write(tmp, "pds/pd/Cargo.toml", '[package]\nname = "pd"\nversion = "0.1.0"\n\n[lints]\nworkspace = true\n')
        write(tmp, "pds/pd/src/main.rs", source)

    def test_a_repo_without_unsafe_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.repo(tmp, "// no unsafe here, only the word in a comment\nfn main() {}\n")
            status, output = run_main(self.tool, ["--root", tmp, "--no-allowlist"])
        self.assertEqual(status, 0, output)
        self.assertIn("PASS", output)

    def test_unsafe_anywhere_fails(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.repo(tmp, "fn main() { unsafe { core::hint::unreachable_unchecked() } }\n")
            status, output = run_main(self.tool, ["--root", tmp, "--no-allowlist"])
        self.assertEqual(status, 1)
        self.assertIn("pds/pd/src/main.rs:1: `unsafe` outside the approved-unsafe list", output)

    def test_no_miri_crates(self):
        with tempfile.TemporaryDirectory() as tmp:
            self.repo(tmp, "fn main() {}\n")
            status, output = run_main(self.tool, ["--root", tmp, "--no-allowlist", "--miri-crates", "pds"])
        self.assertEqual(status, 0, output)
        self.assertEqual(output.strip(), "")


class LocalCrateGuardAcrossWorkspaces(unittest.TestCase):
    """A product's pds/ may use the crates its root workspace declares."""

    tool = load("check-baseline")

    def test_a_member_of_a_workspace_checked_with_it_is_allowed(self):
        with tempfile.TemporaryDirectory() as root:
            write(root, "product/Cargo.toml", '[workspace]\nmembers = ["app/*"]\n')
            write(root, "product/app/flow/Cargo.toml", '[package]\nname = "flow"\nversion = "0.1.0"\n')
            write(root, "product/pds/Cargo.toml", '[workspace]\nmembers = ["pd"]\n')
            write(root, "product/pds/pd/Cargo.toml", '[package]\nname = "pd"\nversion = "0.1.0"\n')
            workspace = os.path.join(root, "product", "pds")

            def package(name, folder):
                return {"name": name, "version": "0.1.0", "id": f"{name}-id", "source": None, "publish": [],
                        "license": None, "manifest_path": os.path.join(root, folder, "Cargo.toml")}

            metadata = {"workspace_root": workspace, "workspace_members": ["pd-id"],
                        "packages": [package("pd", "product/pds/pd"), package("flow", "product/app/flow")]}
            alone = self.tool.local_crate_problems(metadata, root)
            self.assertEqual(len(alone), 1, alone)
            self.assertIn("flow 0.1.0", alone[0])
            siblings = self.tool.declared_member_dirs(os.path.join(root, "product"))
            self.assertEqual(self.tool.local_crate_problems(metadata, root, siblings), [])
            # A crate no workspace declares is still refused.
            metadata["packages"].append(package("stray", "product/vendor/stray"))
            found = self.tool.local_crate_problems(metadata, root, siblings)
            self.assertEqual(len(found), 1, found)
            self.assertIn("stray 0.1.0", found[0])


if __name__ == "__main__":
    unittest.main()

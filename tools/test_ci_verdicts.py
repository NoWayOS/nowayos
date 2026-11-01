# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/ci-verdicts: a verdict holds exactly while its inputs do."""
import importlib.machinery
import importlib.util
import os
import tempfile
import textwrap
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_loader = importlib.machinery.SourceFileLoader("ci_verdicts", os.path.join(HERE, "ci-verdicts"))
_spec = importlib.util.spec_from_loader("ci_verdicts", _loader)
ci_verdicts = importlib.util.module_from_spec(_spec)
_loader.exec_module(ci_verdicts)

LOCK = """\
version = 4

[[package]]
name = "nowayos-a"
version = "0.1.0"

[[package]]
name = "nowayos-b"
version = "0.1.0"
dependencies = [
 "nowayos-a",
 "x",
]

[[package]]
name = "x"
version = "{x}"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{x}"

[[package]]
name = "y"
version = "{y}"
source = "registry+https://github.com/rust-lang/crates.io-index"
checksum = "{y}"
"""


def write(root, path, text):
    full = os.path.join(root, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w", encoding="utf-8") as handle:
        handle.write(textwrap.dedent(text))


class CiVerdictsTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = os.path.join(self.tmp.name, "repo")
        self.dir = os.path.join(self.tmp.name, "verdicts")
        self.containerfile = os.path.join(self.tmp.name, "Containerfile")
        write(self.tmp.name, "Containerfile", "FROM debian\n")
        write(self.root, "Cargo.toml", '[workspace]\nmembers = ["crates/*"]\n\n[profile.release]\npanic = "abort"\n')
        write(self.root, "Cargo.lock", LOCK.format(x="1.0.0", y="1.0.0"))
        write(self.root, "README.md", "a repo\n")
        write(self.root, "crates/nowayos-a/Cargo.toml", '[package]\nname = "nowayos-a"\n')
        write(self.root, "crates/nowayos-a/src/lib.rs", "pub fn a() {}\n")
        write(self.root, "crates/nowayos-b/Cargo.toml",
              '[package]\nname = "nowayos-b"\n[dependencies]\nnowayos-a.workspace = true\nx = "1"\n')
        write(self.root, "crates/nowayos-b/src/lib.rs", "pub fn b() {}\n")
        write(self.root, "fuzz/Cargo.toml", '[package]\nname = "nowayos-fuzz"\n')
        write(self.root, "fuzz/Cargo.lock", "# fuzz lock\n")
        write(self.root, "fuzz/src/lib.rs", "pub fn check() {}\n")
        write(self.root, "fuzz/fuzz_targets/a_target.rs", "use nowayos_a::a;\n")
        write(self.root, "fuzz/fuzz_targets/b_target.rs", "use nowayos_b::b;\n")

    def tearDown(self):
        self.tmp.cleanup()

    def run_tool(self, action, kind, items, salt="flags", root=None, upstream=None, legacy=()):
        argv = [action, kind, *items, "--root", root or self.root, "--dir", self.dir,
                "--pins", self.containerfile, "--salt", salt]
        if upstream:
            argv += ["--upstream", upstream]
        for path in legacy:
            argv += ["--legacy-input", path]
        out = []
        real_print = print
        ci_verdicts.print = lambda *a, **k: out.append(" ".join(str(x) for x in a))
        try:
            self.assertEqual(ci_verdicts.main(argv), 0)
        finally:
            ci_verdicts.print = real_print
        return " ".join(out).split()

    def keep(self, kind="kani", items=("nowayos-a", "nowayos-b"), salt="flags"):
        return self.run_tool("keep", kind, list(items), salt)

    def record(self, kind="kani", items=("nowayos-a", "nowayos-b"), salt="flags"):
        self.run_tool("record", kind, list(items), salt)

    def test_an_item_with_no_verdict_is_kept_and_a_recorded_one_is_not(self):
        self.assertEqual(self.keep(), ["nowayos-a", "nowayos-b"])
        self.record(items=["nowayos-a"])
        self.assertEqual(self.keep(), ["nowayos-b"])
        self.record()
        self.assertEqual(self.keep(), [])

    def test_a_change_to_a_crate_reaches_it_and_what_depends_on_it_only(self):
        self.record()
        write(self.root, "crates/nowayos-b/src/lib.rs", "pub fn b() { }\n")
        self.assertEqual(self.keep(), ["nowayos-b"])
        self.record()
        write(self.root, "crates/nowayos-a/src/new.rs", "\n")
        self.assertEqual(self.keep(), ["nowayos-a", "nowayos-b"])

    def test_a_locked_package_reaches_the_crates_that_use_it_only(self):
        self.record()
        write(self.root, "Cargo.lock", LOCK.format(x="1.0.0", y="1.1.0"))
        self.assertEqual(self.keep(), [], "y is no one's dependency")
        write(self.root, "Cargo.lock", LOCK.format(x="1.0.1", y="1.1.0"))
        self.assertEqual(self.keep(), ["nowayos-b"])

    def test_a_new_member_keeps_the_verdicts_and_a_profile_change_does_not(self):
        self.record()
        write(self.root, "Cargo.toml",
              '[workspace]\nmembers = [\n    "crates/*",\n    "tools/x",\n]\n\n[profile.release]\npanic = "abort"\n')
        self.assertEqual(self.keep(), [])
        write(self.root, "Cargo.toml", '[workspace]\nmembers = ["crates/*"]\n\n[profile.release]\npanic = "unwind"\n')
        self.assertEqual(self.keep(), ["nowayos-a", "nowayos-b"])

    def test_the_container_and_the_flags_reach_everything(self):
        self.record()
        self.assertEqual(self.keep(salt="other flags"), ["nowayos-a", "nowayos-b"])
        write(self.tmp.name, "Containerfile", "FROM debian:newer\n")
        self.assertEqual(self.keep(), ["nowayos-a", "nowayos-b"])

    def test_a_package_added_to_the_image_reaches_nothing_and_a_tool_pin_everything(self):
        write(self.tmp.name, "Containerfile",
              "FROM debian\nARG KANI_VERSION=0.66.0\nRUN apt-get install -y curl\n")
        self.record()
        write(self.tmp.name, "Containerfile",
              "FROM debian\nARG KANI_VERSION=0.66.0\n# openssl too\nRUN apt-get install -y curl openssl\n")
        self.assertEqual(self.keep(), [], "a package, a comment: no tool pin moved")
        write(self.tmp.name, "Containerfile",
              "FROM debian\nARG KANI_VERSION=0.67.0\nRUN apt-get install -y curl openssl\n")
        self.assertEqual(self.keep(), ["nowayos-a", "nowayos-b"])

    def test_an_upstream_crate_reaches_the_product_crates_that_use_it_only(self):
        up = os.path.join(self.tmp.name, "nowayos")
        product = os.path.join(self.tmp.name, "product")
        write(up, "Cargo.toml", '[workspace]\nmembers = ["crates/*"]\n')
        write(up, "README.md", "nowayos\n")
        write(up, "crates/nowayos-ipc/Cargo.toml", '[package]\nname = "nowayos-ipc"\n')
        write(up, "crates/nowayos-ipc/src/lib.rs", "pub fn ipc() {}\n")
        write(up, "crates/nowayos-ui/Cargo.toml",
              '[package]\nname = "nowayos-ui"\n[dependencies]\nnowayos-ipc.workspace = true\n')
        write(up, "crates/nowayos-ui/src/lib.rs", "pub fn ui() {}\n")
        write(up, "crates/nowayos-unused/Cargo.toml", '[package]\nname = "nowayos-unused"\n')
        write(up, "crates/nowayos-unused/src/lib.rs", "pub fn unused() {}\n")
        write(product, "Cargo.toml", '[workspace]\nmembers = ["kiosk/*"]\n')
        write(product, "Cargo.lock", "version = 4\n")
        write(product, "nowayos.rev", "1111\n")
        write(product, "kiosk/app-a/Cargo.toml",
              '[package]\nname = "app-a"\n[dependencies]\nnowayos-ui.workspace = true\n')
        write(product, "kiosk/app-a/src/lib.rs", "pub fn a() {}\n")
        write(product, "kiosk/app-b/Cargo.toml", '[package]\nname = "app-b"\n')
        write(product, "kiosk/app-b/src/lib.rs", "pub fn b() {}\n")
        apps = ["app-a", "app-b"]

        def keep():
            return self.run_tool("keep", "kani", apps, root=product, upstream=up)

        self.run_tool("record", "kani", apps, root=product, upstream=up)
        self.assertEqual(keep(), [])
        write(product, "nowayos.rev", "2222\n")
        write(up, "README.md", "nowayos, bumped\n")
        write(up, "crates/nowayos-unused/src/lib.rs", "pub fn unused() { }\n")
        self.assertEqual(keep(), [], "a bump that changes no crate a proof reads")
        write(up, "crates/nowayos-ipc/src/lib.rs", "pub fn ipc() { }\n")
        self.assertEqual(keep(), ["app-a"], "nowayos-ipc reaches app-a through nowayos-ui")

    def legacy_verdict(self, item, scheme):
        """A verdict as an earlier key scheme recorded it, for today's inputs."""
        digest = ci_verdicts.key(self.root, "kani", item, [], ["flags"],
                                 pins=[self.containerfile] if scheme == 2 else (),
                                 scheme=scheme, legacy=[self.containerfile] if scheme == 1 else ())
        path = ci_verdicts.verdict_path(self.dir, "kani", item, digest)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        open(path, "w", encoding="utf-8").close()

    def test_a_verdict_under_an_earlier_scheme_for_todays_inputs_is_carried_over(self):
        self.legacy_verdict("nowayos-a", 1)
        self.legacy_verdict("nowayos-b", 2)
        keep = lambda: self.run_tool("keep", "kani", ["nowayos-a", "nowayos-b"], legacy=[self.containerfile])
        self.assertEqual(keep(), [])
        self.assertEqual(self.keep(), [], "carried over: today's key now holds them, no legacy needed")

    def test_an_earlier_schemes_verdict_for_other_inputs_is_not(self):
        self.legacy_verdict("nowayos-a", 1)
        self.legacy_verdict("nowayos-b", 2)
        write(self.root, "crates/nowayos-a/src/lib.rs", "pub fn a() { }\n")
        keep = lambda: self.run_tool("keep", "kani", ["nowayos-a", "nowayos-b"], legacy=[self.containerfile])
        self.assertEqual(keep(), ["nowayos-a", "nowayos-b"], "nowayos-b depends on nowayos-a")

    def test_a_workspace_dependency_reaches_only_the_crates_that_use_it(self):
        manifest = ('[workspace]\nmembers = ["crates/*"]\n\n[workspace.dependencies]\n'
                    'nowayos-a = {{ path = "crates/nowayos-a" }}\nw = "{w}"\n{extra}\n'
                    '[profile.release]\npanic = "abort"\n')
        write(self.root, "Cargo.toml", manifest.format(w="1", extra=""))
        write(self.root, "crates/nowayos-b/Cargo.toml",
              '[package]\nname = "nowayos-b"\n[dependencies]\nnowayos-a.workspace = true\n'
              'w.workspace = true\nx = "1"\n')
        self.record()
        write(self.root, "Cargo.toml", manifest.format(w="1", extra='nowayos-new = { path = "crates/new" }'))
        self.assertEqual(self.keep(), [], "a new crate's line reaches no one")
        write(self.root, "Cargo.toml", manifest.format(w="2", extra=""))
        self.assertEqual(self.keep(), ["nowayos-b"], "w is nowayos-b's alone")

    def test_a_file_outside_every_crate_reaches_nothing(self):
        self.record()
        write(self.root, "README.md", "a repo, changed\n")
        write(self.root, "Makefile", "ci:\n")
        self.assertEqual(self.keep(), [])

    def test_a_fuzz_target_holds_while_it_its_harness_and_its_crates_do(self):
        targets = ("a_target", "b_target")
        self.record("fuzz", targets)
        self.assertEqual(self.keep("fuzz", targets), [])
        write(self.root, "crates/nowayos-b/src/lib.rs", "pub fn b() { }\n")
        self.assertEqual(self.keep("fuzz", targets), ["b_target"])
        self.record("fuzz", targets)
        write(self.root, "fuzz/fuzz_targets/a_target.rs", "use nowayos_a::a; // changed\n")
        self.assertEqual(self.keep("fuzz", targets), ["a_target"])
        self.record("fuzz", targets)
        write(self.root, "fuzz/src/lib.rs", "pub fn check() { }\n")
        self.assertEqual(self.keep("fuzz", targets), ["a_target", "b_target"])
        self.record("fuzz", targets)
        write(self.root, "fuzz/Cargo.lock", "# fuzz lock, changed\n")
        self.assertEqual(self.keep("fuzz", targets), ["a_target", "b_target"])


if __name__ == "__main__":
    unittest.main()

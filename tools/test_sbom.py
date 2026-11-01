# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/sbom: the CycloneDX 1.5 SBOM every repo's `make sbom`
writes for each Cargo workspace. A synthetic workspace's cargo
metadata stands in for cargo; the SBOM must list every crate with its
version, source and licence, mark what the product is not built from the
same way every time, carry the standard-library crates when given them, be
byte-identical across runs, and pass the CycloneDX 1.5 structure check,
which must itself catch each kind of mistake.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only.
"""

import contextlib
import copy
import importlib.machinery
import importlib.util
import io
import json
import os
import sys
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


sbom = load("sbom")

ROOT = "/ws/demo"
CRATES_IO = "registry+https://github.com/rust-lang/crates.io-index"
GIT = "git+https://example.org/dep.git?rev=0123456789abcdef0123456789abcdef01234567#0123456789abcdef0123456789abcdef01234567"


def registry(name, version, licence="MIT", proc_macro=False):
    kind = ["proc-macro"] if proc_macro else ["lib"]
    return {"name": name, "version": version, "id": f"{CRATES_IO}#{name}@{version}", "source": CRATES_IO,
            "license": licence, "license_file": None, "description": f"the {name} crate",
            "manifest_path": f"/cargo/registry/src/{name}-{version}/Cargo.toml",
            "targets": [{"kind": kind, "name": name}]}


def local(name, directory, licence=None):
    return {"name": name, "version": "0.1.0", "id": f"path+file://{directory}#{name}@0.1.0", "source": None,
            "license": licence, "license_file": None, "description": None,
            "manifest_path": f"{directory}/Cargo.toml", "targets": [{"kind": ["lib"], "name": name}]}


PACKAGES = [
    local("app", f"{ROOT}/app"),
    local("helper", f"{ROOT}/helper"),
    local("shared", "/ws/other/crates/shared"),
    registry("lib", "1.0.0", "MIT/Apache-2.0"),
    registry("derive", "1.0.0", proc_macro=True),
    registry("synlike", "2.0.0", "MIT OR Apache-2.0"),
    registry("devonly", "0.5.0"),
    registry("buildtool", "0.3.0", "BSD-3-Clause"),
    registry("winonly", "0.1.0"),
    registry("helperdep", "1.0.0"),
    registry("unused", "1.0.0"),
    {"name": "gitdep", "version": "0.1.0", "id": f"{GIT}#gitdep@0.1.0", "source": GIT, "license": "BSD-2-Clause",
     "license_file": None, "description": None, "manifest_path": "/cargo/git/checkouts/dep/Cargo.toml",
     "targets": [{"kind": ["lib"], "name": "gitdep"}]},
]
IDS = {package["name"]: package["id"] for package in PACKAGES}


def edge(name, *kinds, target=None):
    return {"name": name, "pkg": IDS[name],
            "dep_kinds": [{"kind": None if kind == "normal" else kind, "target": target} for kind in kinds]}


def metadata(for_target):
    """cargo metadata for the synthetic workspace; for_target: as --filter-platform gives it."""
    app_deps = [edge("lib", "normal"), edge("derive", "normal"), edge("devonly", "dev"),
                edge("buildtool", "build"), edge("gitdep", "normal"), edge("shared", "normal")]
    packages = PACKAGES
    if not for_target:
        app_deps.append(edge("winonly", "normal", target="cfg(windows)"))
    else:
        packages = [package for package in PACKAGES if package["name"] != "winonly"]
    nodes = [
        {"id": IDS["app"], "deps": app_deps},
        {"id": IDS["helper"], "deps": [edge("helperdep", "normal")]},
        {"id": IDS["derive"], "deps": [edge("synlike", "normal")]},
    ]
    nodes += [{"id": IDS[name], "deps": []} for name in
              ("shared", "lib", "synlike", "devonly", "buildtool", "helperdep", "unused", "gitdep")
              + (() if for_target else ("winonly",))]
    return {"packages": copy.deepcopy(packages), "workspace_members": [IDS["app"], IDS["helper"]],
            "workspace_root": ROOT, "resolve": {"nodes": nodes, "root": None}}


LOCK = "".join(
    f'[[package]]\nname = "{package["name"]}"\nversion = "{package["version"]}"\n'
    f'source = "{CRATES_IO}"\nchecksum = "{(package["name"] * 64)[:64].encode().hex()[:64]}"\n\n'
    for package in PACKAGES if package["source"] == CRATES_IO
)


class Sbom(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.workspace = os.path.join(self.dir, "ws")
        os.makedirs(self.workspace)
        with open(os.path.join(self.workspace, "Cargo.lock"), "w", encoding="utf-8") as handle:
            handle.write(LOCK)
        self.metadata = self.write_json("metadata.json", metadata(False))
        self.metadata_target = self.write_json("metadata-target.json", metadata(True))
        library = os.path.join(self.dir, "library")
        for name, version in (("core", "0.0.0"), ("alloc", "0.0.0"), ("compiler-builtins", "0.1.160")):
            os.makedirs(os.path.join(library, name))
            with open(os.path.join(library, name, "Cargo.toml"), "w", encoding="utf-8") as handle:
                handle.write(f'[package]\nname = "{name}"\nversion = "{version}"\n')
        self.library = library
        self.std_crates = os.path.join(self.dir, "std-crates.txt")
        with open(self.std_crates, "w", encoding="utf-8") as handle:
            handle.write("# crate licence\ncore MIT OR Apache-2.0\nalloc MIT OR Apache-2.0\n"
                         "compiler_builtins MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)\n")

    def tearDown(self):
        self.tmp.cleanup()

    def write_json(self, name, value):
        path = os.path.join(self.dir, name)
        with open(path, "w", encoding="utf-8") as handle:
            json.dump(value, handle)
        return path

    def run_sbom(self, *extra, out="out.cdx.json"):
        out = os.path.join(self.dir, out)
        argv = ["sbom", "--workspace", self.workspace, "--name", "demo", "--out", out,
                "--metadata", self.metadata, "--target", "aarch64-unknown-none",
                "--metadata-target", self.metadata_target, "--host-only", "helper"] + list(extra)
        stdout, stderr = io.StringIO(), io.StringIO()
        saved = sys.argv
        sys.argv = argv
        try:
            with contextlib.redirect_stdout(stdout), contextlib.redirect_stderr(stderr):
                status = sbom.main()
        finally:
            sys.argv = saved
        text = None
        if os.path.exists(out):
            with open(out, encoding="utf-8") as handle:
                text = handle.read()
        return status, stdout.getvalue() + stderr.getvalue(), text

    def components(self, text):
        return {entry["name"]: entry for entry in json.loads(text)["components"]}

    @staticmethod
    def prop(entry, name):
        return next((p["value"] for p in entry.get("properties", []) if p["name"] == name), None)

    def test_every_crate_is_listed_with_version_source_and_licence(self):
        status, output, text = self.run_sbom()
        self.assertEqual(status, 0, output)
        found = self.components(text)
        self.assertEqual(set(found), {p["name"] for p in PACKAGES})
        for entry in found.values():
            self.assertTrue(entry["version"])
            self.assertTrue(entry["licenses"])
            self.assertTrue(self.prop(entry, "nowayos:source"))
        self.assertEqual(found["lib"]["licenses"], [{"expression": "MIT OR Apache-2.0"}])
        self.assertEqual(found["lib"]["purl"], "pkg:cargo/lib@1.0.0")
        self.assertEqual(found["lib"]["hashes"][0]["alg"], "SHA-256")
        self.assertEqual(self.prop(found["lib"], "nowayos:source"), "crates.io")
        self.assertEqual(self.prop(found["gitdep"], "nowayos:source"),
                         "git+https://example.org/dep.git?rev=0123456789abcdef0123456789abcdef01234567")
        self.assertEqual(found["gitdep"]["externalReferences"], [{"type": "vcs", "url": "https://example.org/dep.git"}])
        self.assertTrue(found["gitdep"]["purl"].startswith("pkg:cargo/gitdep@0.1.0?vcs_url="))
        # Local crates: relative paths only, and no licence invented for them.
        self.assertEqual(found["app"]["bom-ref"], "path:app@0.1.0")
        self.assertEqual(found["shared"]["bom-ref"], "path:../other/crates/shared@0.1.0")
        self.assertEqual(found["shared"]["licenses"], [{"license": {"name": sbom.UNDECLARED}}])
        self.assertNotIn("/ws/", text)

    def test_what_the_product_is_not_built_from_is_marked_excluded_with_the_reason(self):
        status, output, text = self.run_sbom()
        self.assertEqual(status, 0, output)
        found = self.components(text)
        required = {name for name, entry in found.items() if entry["scope"] == "required"}
        self.assertEqual(required, {"app", "lib", "gitdep", "shared"})
        reasons = {name: self.prop(entry, "nowayos:excluded") for name, entry in found.items()
                   if entry["scope"] == "excluded"}
        self.assertEqual(reasons, {
            "derive": sbom.REASONS["build"],
            "synlike": sbom.REASONS["build"],
            "buildtool": sbom.REASONS["build"],
            "devonly": sbom.REASONS["dev"],
            "winonly": sbom.REASONS["platform"],
            "helper": sbom.REASONS["host"],
            "helperdep": sbom.REASONS["host"],
            "unused": sbom.REASONS["unused"],
        })

    def test_without_a_target_every_platform_counts(self):
        status, output, text = self.run_sbom("--target", "", "--metadata-target", "")
        self.assertEqual(status, 0, output)
        found = self.components(text)
        self.assertEqual(found["winonly"]["scope"], "required")
        metadata = json.loads(text)["metadata"]
        self.assertEqual(self.prop(metadata, "nowayos:runs-on"), "the build host: a tool that never ships on a device")

    def test_a_host_only_workspace_ships_nothing(self):
        status, output, text = self.run_sbom("--host-only-workspace")
        self.assertEqual(status, 0, output)
        self.assertEqual({entry["scope"] for entry in self.components(text).values()}, {"excluded"})

    def test_standard_library_crates_are_included_with_their_licences(self):
        status, output, text = self.run_sbom("--std-crates", self.std_crates, "--std-library", self.library,
                                             "--toolchain", "nightly-2026-04-04")
        self.assertEqual(status, 0, output)
        found = self.components(text)
        self.assertEqual(found["core"]["version"], "0.0.0")
        self.assertEqual(found["compiler_builtins"]["version"], "0.1.160")
        self.assertEqual(found["compiler_builtins"]["licenses"],
                         [{"expression": "MIT AND Apache-2.0 WITH LLVM-exception AND (MIT OR Apache-2.0)"}])
        for name in ("core", "alloc", "compiler_builtins"):
            self.assertEqual(found[name]["scope"], "required")
            self.assertEqual(self.prop(found[name], "nowayos:source"), "rust-src of nightly-2026-04-04 (build-std)")

    def test_a_standard_library_crate_rust_src_lacks_fails(self):
        with open(self.std_crates, "a", encoding="utf-8") as handle:
            handle.write("std MIT OR Apache-2.0\n")
        status, output, _ = self.run_sbom("--std-crates", self.std_crates, "--std-library", self.library,
                                          "--toolchain", "nightly-2026-04-04")
        self.assertEqual(status, 2)
        self.assertIn("std", output)

    def test_two_runs_give_the_same_bytes(self):
        _, _, first = self.run_sbom(out="one.json")
        _, _, second = self.run_sbom(out="two.json")
        self.assertIsNotNone(first)
        self.assertEqual(first, second)
        self.assertTrue(first.endswith("}\n"))

    def test_an_unknown_host_only_crate_fails(self):
        status, output, text = self.run_sbom("--host-only", "nonesuch")
        self.assertEqual(status, 2)
        self.assertIn("nonesuch", output)
        self.assertIsNone(text)

    def test_the_output_is_valid_cyclonedx_with_a_dependency_graph(self):
        status, output, text = self.run_sbom()
        self.assertEqual(status, 0, output)
        document = json.loads(text)
        self.assertEqual(sbom.validate(document), [])
        self.assertEqual((document["bomFormat"], document["specVersion"], document["version"]), ("CycloneDX", "1.5", 1))
        graph = {entry["ref"]: entry["dependsOn"] for entry in document["dependencies"]}
        self.assertEqual(graph["workspace:demo"], ["path:app@0.1.0", "path:helper@0.1.0"])
        self.assertIn("pkg:cargo/lib@1.0.0", graph["path:app@0.1.0"])


class Validate(unittest.TestCase):
    """The CycloneDX 1.5 structure check catches each kind of mistake."""

    def setUp(self):
        self.good = {
            "bomFormat": "CycloneDX", "specVersion": "1.5", "version": 1,
            "metadata": {"tools": {"components": [{"type": "application", "name": "t"}]},
                         "component": {"type": "library", "bom-ref": "workspace:x", "name": "x"},
                         "properties": [{"name": "p", "value": "v"}]},
            "components": [
                {"type": "library", "bom-ref": "pkg:cargo/a@1.0.0", "name": "a", "version": "1.0.0", "scope": "required",
                 "hashes": [{"alg": "SHA-256", "content": "0" * 64}], "licenses": [{"expression": "MIT"}],
                 "purl": "pkg:cargo/a@1.0.0", "externalReferences": [{"type": "vcs", "url": "https://x"}],
                 "properties": [{"name": "nowayos:source", "value": "crates.io"}]},
                {"type": "library", "bom-ref": "path:b@0.1.0", "name": "b", "version": "0.1.0", "scope": "excluded",
                 "licenses": [{"license": {"name": "n"}}]},
            ],
            "dependencies": [{"ref": "workspace:x", "dependsOn": ["path:b@0.1.0"]},
                             {"ref": "path:b@0.1.0", "dependsOn": ["pkg:cargo/a@1.0.0"]}],
        }

    def broken(self, change):
        document = copy.deepcopy(self.good)
        change(document)
        return sbom.validate(document)

    def test_the_good_document_passes(self):
        self.assertEqual(sbom.validate(self.good), [])

    def test_each_mistake_is_caught(self):
        def first(document):
            return document["components"][0]

        cases = {
            "bomFormat": lambda d: d.update(bomFormat="SPDX"),
            "specVersion": lambda d: d.update(specVersion="1.4"),
            "version must be an integer": lambda d: d.update(version="1"),
            "does not have": lambda d: d.update(extra=1),
            "metadata.tools": lambda d: d["metadata"].update(tools=[]),
            "type 'gadget'": lambda d: first(d).update(type="gadget"),
            "name is required": lambda d: first(d).pop("name"),
            "scope": lambda d: first(d).update(scope="runtime"),
            "hashes[0]": lambda d: first(d).update(hashes=[{"alg": "SHA-256", "content": "xyz"}]),
            "only entry": lambda d: first(d).update(licenses=[{"expression": "MIT"}, {"license": {"name": "x"}}]),
            "exactly one of id or name": lambda d: first(d).update(licenses=[{"license": {"id": "MIT", "name": "MIT"}}]),
            "purl": lambda d: first(d).update(purl="cargo/a"),
            "externalReferences[0]": lambda d: first(d).update(externalReferences=[{"type": "repo", "url": "x"}]),
            "not unique": lambda d: d["components"][1].update({"bom-ref": "pkg:cargo/a@1.0.0"}),
            "names no component": lambda d: d["dependencies"][1].update(dependsOn=["pkg:cargo/zzz@9"]),
            "appears twice": lambda d: d["dependencies"].append({"ref": "path:b@0.1.0", "dependsOn": []}),
            "properties[0]": lambda d: first(d).update(properties=[{"name": "p", "value": 3}]),
        }
        for expected, change in cases.items():
            with self.subTest(expected):
                problems = self.broken(change)
                self.assertTrue(any(expected in problem for problem in problems), problems)


if __name__ == "__main__":
    unittest.main()

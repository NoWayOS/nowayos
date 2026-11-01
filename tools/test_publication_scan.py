# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/publication-scan: a term planted in a scratch
repository is listed wherever it is - in the tree, only in the history, in a
file name, a commit message, an author, a ref or a tag - naming where it is
and the term's line in the list, never the term; links into Azure DevOps,
assistant instruction files, wiki references and every identity are listed
too; and a term list inside a git work tree is refused.

The terms are invented for these tests and the repositories are temporary.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only; needs git.
"""

import contextlib
import importlib.machinery
import importlib.util
import io
import os
import subprocess
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


SCAN = load("publication-scan")

# git with no system or user configuration, and a fixed author.
GIT_ENV = {
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_AUTHOR_NAME": "Test",
    "GIT_AUTHOR_EMAIL": "test@example.invalid",
    "GIT_COMMITTER_NAME": "Test",
    "GIT_COMMITTER_EMAIL": "test@example.invalid",
}
# The invented term list: line 1 is a comment, so the terms are on lines 2 and 4.
TERMS = "# invented for the tests\nZebrafinch Holdings\n\nquokka-7\n"


class Repo:
    """A scratch git repository in a temporary directory."""

    def __init__(self, directory):
        self.path = directory
        os.makedirs(directory, exist_ok=True)
        self.git("init", "-q", "-b", "main")

    def git(self, *args, env=None):
        result = subprocess.run(["git", "-C", self.path, "-c", "commit.gpgsign=false", *args],
                                capture_output=True, text=True, env={**os.environ, **GIT_ENV, **(env or {})},
                                check=False)
        if result.returncode != 0:
            raise AssertionError(f"git {' '.join(args)}: {result.stderr}")
        return result.stdout.strip()

    def write(self, name, text):
        path = os.path.join(self.path, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, "w", encoding="utf-8") as handle:
            handle.write(text)
        return path

    def commit(self, message="change", env=None):
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message, env=env)
        return self.git("rev-parse", "HEAD")


def run(*argv):
    """(status, stdout + stderr) of publication-scan with argv."""
    out = io.StringIO()
    saved = sys.argv
    sys.argv = ["publication-scan", *argv]
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            status = SCAN.main()
    finally:
        sys.argv = saved
    return status, out.getvalue()


class PublicationScanTest(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Repo(os.path.join(self.tmp.name, "repo"))
        self.repo.write("README.md", "A library.\n")
        self.repo.write("src/lib.rs", "pub fn one() -> u32 {\n    1\n}\n")
        self.repo.commit("First commit")
        os.makedirs(os.path.join(self.tmp.name, "outside"))
        self.terms = os.path.join(self.tmp.name, "outside", "terms.txt")
        with open(self.terms, "w", encoding="utf-8") as handle:
            handle.write(TERMS)

    def tearDown(self):
        self.tmp.cleanup()

    def scan(self):
        status, output = run("--terms", self.terms, self.repo.path)
        for term in ("Zebrafinch", "zebrafinch", "quokka"):
            self.assertNotIn(term, output, "the report must never repeat a term")
        return status, output

    def test_a_clean_repository_lists_only_its_identities(self):
        status, output = self.scan()
        self.assertEqual(status, 0, output)
        self.assertIn("identity: Test <test@example.invalid> - author of 1; committer of 1", output)
        self.assertIn("0 term findings", output)

    def test_a_term_in_the_tree_names_its_file_line_and_list_line(self):
        self.repo.write("src/lib.rs", "pub fn one() -> u32 {\n    // for ZEBRAFINCH holdings\n    1\n}\n")
        self.repo.commit()
        status, output = self.scan()
        self.assertEqual(status, 1, output)
        self.assertIn("term 2: src/lib.rs:2 (in the tree)", output)

    def test_a_term_removed_from_the_tree_is_still_in_the_history(self):
        self.repo.write("notes.txt", "Meeting with quokka-7 on Monday.\n")
        added = self.repo.commit()
        self.repo.git("rm", "-q", "notes.txt")
        self.repo.commit()
        status, output = self.scan()
        self.assertEqual(status, 1, output)
        self.assertIn(f"term 4: notes.txt (in 1 version in the history, added by commit {added[:12]})", output)
        self.assertNotIn("(in the tree)", output)

    def test_terms_in_names_messages_identities_refs_and_tags(self):
        self.repo.write("profiles/quokka-7.toml", "x = 1\n")
        commit = self.repo.commit("Add the profile for Zebrafinch Holdings",
                                  env={"GIT_AUTHOR_NAME": "Someone at Zebrafinch Holdings"})
        self.repo.git("branch", "quokka-7-pilot")
        self.repo.git("tag", "-a", "v0.1.0", "-m", "Shown to Zebrafinch Holdings")
        status, output = self.scan()
        self.assertEqual(status, 1, output)
        short = commit[:12]
        for expected in ("term 4: profiles/[term 4].toml (its name, in the tree)",
                         f"term 2: commit {short}, message line 1",
                         "term 2: identity Someone at [term 2] <test@example.invalid> (author of 1)",
                         "term 4: ref refs/heads/[term 4]-pilot",
                         "term 2: tag refs/tags/v0.1.0, message line 1",
                         "identity: Someone at [term 2] <test@example.invalid> - author of 1"):
            self.assertIn(expected, output)

    def test_whole_words_only_where_the_term_starts_or_ends_with_a_letter(self):
        self.repo.write("src/names.rs", "// zebrafinch holdingsworth, quokka-77\n")
        self.repo.commit()
        status, output = self.scan()
        self.assertEqual(status, 0, output)

    def test_links_assistant_files_wiki_references_and_co_authors(self):
        # Built from fragments, so this file holds no link the real scan would list.
        self.repo.write("README.md", "Design: https://dev." + "azure.com/example/Project/_wiki (the wiki).\n")
        self.repo.write("CLAUDE.md", "How the assistant works here.\n")
        self.repo.write("docs/design.md", "See https://github." + "com/NoWayOS/Wiki/blob/main/Page.md.\n")
        commit = self.repo.commit("Docs\n\nCo-Authored-By: An Assistant <assistant@example.invalid>")
        status, output = self.scan()
        self.assertEqual(status, 1, output)
        self.assertIn("link: README.md:1 (in the tree)", output)
        self.assertIn("link: docs/design.md:1 (in the tree)", output, "the private wiki on GitHub")
        self.assertIn("assistant-file: CLAUDE.md (in the tree)", output)
        self.assertIn(f"assistant-file: CLAUDE.md (in 1 version in the history, added by commit {commit[:12]})",
                      output)
        self.assertIn("wiki-reference: README.md (1 line in the tree)", output)
        self.assertIn("identity: An Assistant <assistant@example.invalid> - co-author of 1", output)

    def test_a_term_list_inside_a_repository_is_refused(self):
        inside = self.repo.write("terms.txt", TERMS)
        status, output = run("--terms", inside, self.repo.path)
        self.assertEqual(status, 2, output)
        self.assertIn("outside every repository", output)

    def test_a_term_list_with_a_short_term_or_no_term_is_refused(self):
        for text in ("ab\n", "# nothing but a comment\n"):
            with open(self.terms, "w", encoding="utf-8") as handle:
                handle.write(text)
            status, output = run("--terms", self.terms, self.repo.path)
            self.assertEqual(status, 2, output)


if __name__ == "__main__":
    unittest.main()

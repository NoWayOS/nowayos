# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/secrets-scan: a key planted in a scratch
repository fails the scan, naming the file and line, wherever it is - in the
tree, only in the history, on another branch or in a commit message; every
rule finds its kind of secret; and an allow entry suppresses exactly the
finding it names, with a reason.

Every secret here is built from fragments when the test runs, and the
repositories are temporary: this file never holds a string any rule matches,
so the real scan of this repository stays clean.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only; needs git.
"""

import base64
import contextlib
import importlib.machinery
import importlib.util
import io
import os
import random
import re
import subprocess
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


SCAN = load("secrets-scan")

# git with no system or user configuration, and a fixed author.
GIT_ENV = {
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_AUTHOR_NAME": "Test",
    "GIT_AUTHOR_EMAIL": "test@example.invalid",
    "GIT_COMMITTER_NAME": "Test",
    "GIT_COMMITTER_EMAIL": "test@example.invalid",
}

ALPHANUMERIC = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789"
RANDOM = random.Random(8152)


def noise(length, alphabet=ALPHANUMERIC):
    return "".join(RANDOM.choice(alphabet) for _ in range(length))


def private_key():
    """A fake PEM private key: random bytes between the begin and end lines."""
    body = base64.b64encode(os.urandom(144)).decode()
    lines = [body[i:i + 64] for i in range(0, len(body), 64)]
    return "\n".join(["-----BEGIN " + "RSA PRIVATE" + " KEY-----", *lines, "-----END " + "RSA PRIVATE" + " KEY-----"])


def examples():
    """One fake secret per content rule, each on a line of its own."""
    return {
        "private-key": private_key(),
        "putty-private-key": "PuTTY-User-Key-File-" + "3: ssh-ed25519\nEncryption: none\nPublic-Lines: 1\nAAAA\n"
                             + "Private-" + "Lines: 1\nAAAA",
        "aws-access-key": "AKIA" + noise(16, "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"),
        "azure-storage-key": "DefaultEndpointsProtocol=https;AccountName=x;Account" + "Key="
                             + base64.b64encode(os.urandom(64)).decode(),
        "azure-client-secret": 'client = "' + noise(3) + "8Q" + "~" + noise(34) + '"',
        "github-token": "gh" + "p_" + noise(36),
        "gitlab-token": "gl" + "pat-" + noise(20),
        "slack-token": "xo" + "xb-123456789012-" + noise(24),
        "slack-webhook": "https://hooks.slack" + ".com/services/T0000/B0000/" + noise(24),
        "google-api-key": "AI" + "za" + noise(35),
        "anthropic-api-key": "sk-" + "ant-api03-" + noise(40),
        "openai-api-key": "sk-" + "proj-" + noise(20) + "T3Blbk" + "FJ" + noise(20),
        "npm-token": "np" + "m_" + noise(36),
        "stripe-key": "sk" + "_live_" + noise(24),
        "jwt": "ey" + "J" + noise(20) + ".ey" + "J" + noise(20) + "." + noise(30),
        "credentials-in-url": "https://deploy" + ":" + noise(12) + "@git.example.invalid/repo",
        "secret-assignment": "api_key" + ' = "' + noise(24) + '"',
    }


class Repo:
    """A scratch git repository in a temporary directory."""

    def __init__(self, directory):
        self.path = directory
        os.makedirs(directory, exist_ok=True)
        self.git("init", "-q", "-b", "main")

    def git(self, *args):
        result = subprocess.run(["git", "-C", self.path, "-c", "commit.gpgsign=false", *args],
                                capture_output=True, text=True, env={**os.environ, **GIT_ENV}, check=False)
        if result.returncode != 0:
            raise AssertionError(f"git {' '.join(args)}: {result.stderr}")
        return result.stdout.strip()

    def write(self, name, text, mode="w"):
        path = os.path.join(self.path, name)
        os.makedirs(os.path.dirname(path), exist_ok=True)
        with open(path, mode) as handle:
            handle.write(text)
        return path

    def commit(self, message="change"):
        self.git("add", "-A")
        self.git("commit", "-q", "--allow-empty", "-m", message)
        return self.git("rev-parse", "HEAD")


def run(path, *argv):
    """Run secrets-scan on the repository at `path`; return (status, stdout + stderr)."""
    out = io.StringIO()
    saved = sys.argv
    sys.argv = ["secrets-scan", "--repo", path, *argv]
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(out):
            status = SCAN.main()
    finally:
        sys.argv = saved
    return status, out.getvalue()


def ordinary(repo):
    """A small project with a history and nothing secret in it."""
    repo.write("README.md", "# Example\n\nSee the wiki. The password is never stored here.\n")
    repo.write("src/lib.rs", 'pub fn token_count(text: &str) -> usize {\n    text.split(" ").count()\n}\n')
    repo.commit("Start")
    repo.write("README.md", "# Example\n\nBuild with make.\n")
    repo.commit("Say how to build")


class SecretsScan(unittest.TestCase):

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.repo = Repo(os.path.join(self.tmp.name, "scratch"))
        ordinary(self.repo)

    def tearDown(self):
        self.tmp.cleanup()

    def plant_key(self, name="config/deploy.pem"):
        """A fake private key on line 3 of `name`; returns its first body line."""
        key = private_key()
        self.repo.write(name, "# Deployment settings\n\n" + key + "\n")
        return key.splitlines()[1]

    def test_a_clean_repository_passes(self):
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_PASS, output)
        self.assertIn("secrets-scan: PASS", output)
        self.assertIn("2 commit messages", output)

    def test_a_key_planted_in_the_tree_fails_naming_file_and_line(self):
        body = self.plant_key()
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_FAIL, output)
        self.assertIn("config/deploy.pem:3: private-key", output)
        self.assertIn("in the tree", output)
        self.assertNotIn(body, output, "the scan must never print what it found")

    def test_a_key_removed_from_the_tree_is_still_found_in_the_history(self):
        self.plant_key()
        added = self.repo.commit("Add the deployment settings")
        self.repo.git("rm", "-q", "config/deploy.pem")
        self.repo.commit("Remove them again")
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_FAIL, output)
        self.assertIn("config/deploy.pem:3: private-key", output)
        self.assertIn(f"in the history, added by commit {added[:12]}", output)
        self.assertNotIn("in the tree", output)

    def test_every_ref_is_scanned(self):
        self.repo.git("checkout", "-q", "-b", "side")
        self.plant_key("deploy/key.pem")
        added = self.repo.commit("On a side branch")
        self.repo.git("checkout", "-q", "main")
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_FAIL, output)
        self.assertIn("deploy/key.pem:3: private-key", output)
        self.assertIn(added[:12], output)

    def test_commit_messages_are_scanned(self):
        token = "gh" + "p_" + noise(36)
        commit = self.repo.commit(f"Configure CI\n\nUse {token} for now")
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_FAIL, output)
        self.assertIn(f"commit {commit[:12]}, message line 3: github-token", output)
        self.assertNotIn(token, output)

    def test_every_rule_finds_its_kind_of_secret(self):
        found = examples()
        self.assertEqual(set(found) | {"key-file"}, SCAN.RULE_IDS, "every rule needs an example here")
        for rule, secret in found.items():
            self.repo.write(f"examples/{rule}.txt", f"# {rule}\n{secret}\n")
        self.repo.write("certs/client.p12", b"\x30\x82\x00\x00binary\x00key store", mode="wb")
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_FAIL, output)
        for rule in found:
            with self.subTest(rule):
                self.assertIn(f"examples/{rule}.txt:2: {rule},", output)
                self.assertEqual(output.count(f"examples/{rule}.txt:"), 1, output)
        self.assertIn("certs/client.p12:1: key-file,", output)

    def test_near_misses_are_not_findings(self):
        self.repo.write("notes.md", "\n".join([
            "Refuse any file that starts " + "-----BEGIN " + "RSA PRIVATE" + " KEY-----" + " outright.",
            "AWS's documented example key, " + "AKIA" + "IOSFODNN7" + "EXAMPLE" + ", is not a key.",
            "password" + ' = "' + "correct-horse-battery-staple" + '"',
            "https://user" + ":" + "password" + "@example.invalid/",
            "https://git.example.invalid/project/repo",
            "ssh://git@example.invalid:22/repo.git",
            "token" + ' = "' + "short1" + '"',
        ]) + "\n")
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_PASS, output)

    def finding(self, output):
        match = re.search(r"(\S+):(\d+): (\S+), .* \[fingerprint ([0-9a-f]{16})\]", output)
        self.assertIsNotNone(match, output)
        return match.group(1), match.group(3), match.group(4)

    def allow(self, entries):
        return self.repo.write("secrets-allow.toml", textwrap.dedent(entries))

    def test_an_allow_entry_suppresses_exactly_the_finding_it_names(self):
        self.plant_key("tests/data/test-key.pem")
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_FAIL, output)
        path, rule, mark = self.finding(output)
        self.allow(f"""\
            [[allow]]
            rule = "{rule}"
            path = "{path}"
            fingerprint = "{mark}"
            reason = "A throwaway key generated for a test; it protects nothing."
            """)
        self.repo.commit("A test key, allowed")
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_PASS, output)
        self.assertIn("1 allowed finding", output)
        # The same key anywhere else is still a finding.
        with open(os.path.join(self.repo.path, path)) as handle:
            self.repo.write("deploy/prod.pem", handle.read())
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_FAIL, output)
        self.assertIn("deploy/prod.pem:3: private-key", output)
        self.assertNotIn(f"{path}:3", output)

    def test_an_allow_entry_that_matches_nothing_fails(self):
        self.allow("""\
            [[allow]]
            rule = "private-key"
            path = "tests/data/gone.pem"
            fingerprint = "0123456789abcdef"
            reason = "Was a test key."
            """)
        status, output = run(self.repo.path)
        self.assertEqual(status, SCAN.EXIT_FAIL, output)
        self.assertIn("entry 1 matches no finding: remove it", output)

    def test_an_allow_entry_must_be_exact_and_give_a_reason(self):
        bad = {
            "no reason": 'rule = "private-key"\npath = "a.pem"\nfingerprint = "0123456789abcdef"\n',
            "empty reason": 'rule = "private-key"\npath = "a.pem"\nfingerprint = "0123456789abcdef"\nreason = " "\n',
            "unknown rule": 'rule = "any"\npath = "a.pem"\nfingerprint = "0123456789abcdef"\nreason = "x"\n',
            "no location": 'rule = "private-key"\nfingerprint = "0123456789abcdef"\nreason = "x"\n',
            "a pattern": 'rule = "private-key"\npath = "a.pem"\nfingerprint = "0123*"\nreason = "x"\n',
            "short commit": 'rule = "jwt"\ncommit = "abc123"\nfingerprint = "0123456789abcdef"\nreason = "x"\n',
            "extra key": 'rule = "jwt"\npath = "a"\nfingerprint = "0123456789abcdef"\nreason = "x"\nline = 3\n',
        }
        for name, entry in bad.items():
            with self.subTest(name):
                self.allow("[[allow]]\n" + entry)
                status, output = run(self.repo.path)
                self.assertEqual(status, SCAN.EXIT_ERROR, output)

    def test_a_shallow_clone_is_refused(self):
        self.repo.commit("A third commit")
        shallow = os.path.join(self.tmp.name, "shallow")
        subprocess.run(["git", "clone", "-q", "--depth", "1", "file://" + self.repo.path, shallow],
                       capture_output=True, env={**os.environ, **GIT_ENV}, check=True)
        status, output = run(shallow)
        self.assertEqual(status, SCAN.EXIT_ERROR, output)
        self.assertIn("shallow clone", output)

    def test_only_the_top_of_a_repository(self):
        status, output = run(os.path.join(self.repo.path, "src"))
        self.assertEqual(status, SCAN.EXIT_ERROR, output)
        self.assertIn("is not the top of a git repository", output)
        status, output = run(self.tmp.name)
        self.assertEqual(status, SCAN.EXIT_ERROR, output)


class ScannerSourcesAreClean(unittest.TestCase):
    """The scanner and these tests never fail the real scan of this repository."""

    def test_no_rule_matches_the_scanner_or_its_tests(self):
        for name in ("secrets-scan", "test_secrets_scan.py"):
            with open(os.path.join(HERE, name), "rb") as handle:
                self.assertEqual(SCAN.scan(handle.read(), f"tools/{name}"), [], name)


if __name__ == "__main__":
    unittest.main()

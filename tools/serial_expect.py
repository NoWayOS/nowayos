# SPDX-License-Identifier: MIT OR Apache-2.0
"""The expect-file rules that qemu-test and check-serial share.

An expect file (systems/SYSTEM.expect) holds one expected line per line;
blank lines and lines starting with '#' are ignored. A line starting with '!'
is forbidden output instead: the rest of the line is a text that must not
appear anywhere in any output line checked (for example `!MON|ERROR`).

The rules: every expected line must appear, in order, each as a whole line,
with any other output in between; and no checked line may contain a
forbidden text. qemu-test applies them to QEMU's output as it arrives (and
times it); check-serial applies them to a serial log captured from a board.
Both report through Reporter, so a board run reads like a QEMU run.

Python 3 standard library only.
"""

import sys

TAIL_LINES = 40


class ExpectError(Exception):
    """An expect file that cannot be used; the message says why."""


def read_expectations(path):
    """Return (expected lines, forbidden texts) from an expect file.

    Raises ExpectError if the file cannot be read, lists no expected line,
    or has a '!' line with no text.
    """
    try:
        with open(path, encoding="utf-8") as handle:
            lines = [line.rstrip("\r\n") for line in handle]
    except OSError as err:
        raise ExpectError(f"cannot read {path}: {err}") from err
    wanted = [line for line in lines if line.strip() and not line.startswith("#")]
    expected = [line for line in wanted if not line.startswith("!")]
    forbidden = [line[1:] for line in wanted if line.startswith("!")]
    if not expected:
        raise ExpectError(f"{path} lists no expected lines")
    if not all(forbidden):
        raise ExpectError(f"{path} has a '!' line with no text")
    return expected, forbidden


def split_lines(data):
    """Split serial output (bytes) into lines of text.

    CR LF, LF and a lone CR each end a line, since serial consoles and
    capture tools disagree; bytes that are not UTF-8 become U+FFFD. A final
    line break does not start another line.
    """
    text = data.decode("utf-8", errors="replace").replace("\r\n", "\n").replace("\r", "\n")
    lines = text.split("\n")
    if lines and lines[-1] == "":
        lines.pop()
    return lines


def tail(path, count=TAIL_LINES):
    """Return the last `count` lines of a log file, for failure reports."""
    try:
        with open(path, "rb") as handle:
            data = handle.read()
    except OSError:
        return []
    return split_lines(data)[-count:]


class Matcher:
    """Applies the rules to output lines fed one at a time.

    Each line comes with a position (seconds since boot for qemu-test, a
    line number for check-serial), recorded for the report. Once a
    forbidden text has been seen, further lines are ignored.
    """

    def __init__(self, expected, forbidden):
        self.expected = expected
        self.forbidden = forbidden
        self.seen = []  # position of each expected line found, in order
        self.offending = None  # (position, line, forbidden text) once seen

    @property
    def complete(self):
        """Every expected line has been seen."""
        return len(self.seen) == len(self.expected)

    @property
    def missing(self):
        """The next expected line not yet seen, or None."""
        return None if self.complete else self.expected[len(self.seen)]

    def feed(self, position, text):
        """Check one line. Return True if it was the next expected line."""
        if self.offending is not None:
            return False
        hit = next((bad for bad in self.forbidden if bad in text), None)
        if hit is not None:
            self.offending = (position, text, hit)
            return False
        if text == self.missing:
            self.seen.append(position)
            return True
        return False


class Reporter:
    """Prints a tool's progress and verdict in the shared format.

    `stamp(position)` labels a found line (for example "[  0.047s]") and
    `at(position)` places an event in a sentence (for example "after
    0.047s").
    """

    def __init__(self, tool, log, stamp, at):
        self.tool = tool
        self.log = log
        self.stamp = stamp
        self.at = at

    def say(self, text):
        print(f"{self.tool}: {text}", flush=True)

    def found(self, position, text):
        self.say(f"{self.stamp(position)} found: {text}")

    def passed(self, matcher, quiet_after=""):
        """Report a pass; `quiet_after` says how long nothing forbidden came."""
        self.say(f"PASS - all {len(matcher.expected)} line(s) seen in order; "
                 f"last {self.at(matcher.seen[-1])}{quiet_after}")

    def fail(self, reason):
        """Report a failure with the end of the log."""
        print(f"{self.tool}: FAIL - {reason}", file=sys.stderr)
        print(f"{self.tool}: last lines of {self.log}:", file=sys.stderr)
        for line in tail(self.log):
            print(f"    {line}", file=sys.stderr)

    def fail_forbidden(self, matcher):
        position, text, hit = matcher.offending
        self.fail(f"forbidden output {hit!r} {self.at(position)}: {text!r}")

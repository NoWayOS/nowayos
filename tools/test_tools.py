# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for the tools in this directory: qemu-test and check-serial (and
the rules they share, serial_expect.py), check-system, board-system,
check-unsafe, check-std-crates, check-crates-absent and check-baseline. Each check must pass on good input
and fail, with its reason, on each kind of bad input; board-system also writes each image's build time
.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only.
"""

import contextlib
import fnmatch
import glob
import importlib.machinery
import importlib.util
import io
import json
import os
import re
import subprocess
import sys
import struct
import tempfile
import textwrap
import time
import tomllib
import unittest
import xml.etree.ElementTree as ET
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    """Import an extensionless script in this directory as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


def run_main(module, argv):
    """Run a tool's main() with `argv`; return (status, stdout, stderr)."""
    out, err = io.StringIO(), io.StringIO()
    saved = sys.argv
    sys.argv = [module.__name__] + argv
    try:
        with contextlib.redirect_stdout(out), contextlib.redirect_stderr(err):
            status = module.main()
    finally:
        sys.argv = saved
    return status, out.getvalue(), err.getvalue()


def write(directory, name, text):
    path = os.path.join(directory, name)
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as handle:
        handle.write(textwrap.dedent(text))
    return path


class QemuTest(unittest.TestCase):
    """tools/qemu-test, with a shell script standing in for QEMU."""

    def run_console(self, lines, expect, settle, linger=3.0, timeout=10.0, baud=None, extra=()):
        with tempfile.TemporaryDirectory() as tmp:
            expect_file = write(tmp, "x.expect", expect)
            script = "".join(f"printf '%s\\n' {json.dumps(line)}\n" for line in lines)
            fake_qemu = ["sh", "-c", script + f"sleep {linger}"]
            result = subprocess.run(
                [sys.executable, os.path.join(HERE, "qemu-test"), "--expect", expect_file,
                 "--log", os.path.join(tmp, "serial.log"), "--timeout", str(timeout),
                 "--settle", str(settle)] + ([] if baud is None else ["--baud", str(baud)])
                + list(extra) + ["--"] + fake_qemu,
                capture_output=True, text=True, timeout=60,
            )
            return result.returncode, result.stdout + result.stderr

    EXPECT = """\
        NOWAYOS: rust pd starting
        NOWAYOS: hello from rust
        !MON|ERROR
        !panicked
        """

    def test_pass_after_settling(self):
        status, output = self.run_console(
            ["boot", "NOWAYOS: rust pd starting", "noise", "NOWAYOS: hello from rust"],
            self.EXPECT, settle=0.5)
        self.assertEqual(status, 0, output)
        self.assertIn("nothing forbidden in the 0.5s after it", output)

    def test_a_fault_just_after_the_last_line_fails(self):
        # Regression (review 2026-10-01): forbidden output after the last
        # expected line used to be ignored.
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting", "NOWAYOS: hello from rust",
             "MON|ERROR: received message 0x6 badge: 0x1 tcb: 0x2",
             "hello panicked at src/main.rs:1:1"],
            self.EXPECT, settle=1.0)
        self.assertEqual(status, 1, output)
        self.assertIn("forbidden output 'MON|ERROR'", output)

    def test_without_settling_late_output_is_not_watched(self):
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting", "NOWAYOS: hello from rust", "MON|ERROR: late"],
            self.EXPECT, settle=0)
        self.assertEqual(status, 0, output)

    def test_forbidden_output_before_the_last_line_fails(self):
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting", "x panicked y", "NOWAYOS: hello from rust"],
            self.EXPECT, settle=0.5)
        self.assertEqual(status, 1, output)

    def test_a_missing_line_times_out(self):
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting"], self.EXPECT, settle=0.5, linger=5, timeout=1)
        self.assertEqual(status, 1, output)
        self.assertIn("not seen within 1s", output)

    def test_baud_takes_the_serial_at_a_uarts_line_rate(self):
        # At 1200 baud, 8N1, 120 bytes a second, so these 81 bytes take
        # at least 0.675 s, where QEMU's own serial takes none.
        lines = ["x" * 29, "NOWAYOS: rust pd starting", "NOWAYOS: hello from rust"]
        status, output = self.run_console(lines, self.EXPECT, settle=0.2, linger=2.0, baud=1200)
        self.assertEqual(status, 0, output)
        self.assertIn("serial taken at 1200 baud, 8N1", output)
        last = re.search(r"\[ *([0-9.]+)s\] found: NOWAYOS: hello from rust", output)
        self.assertGreaterEqual(float(last.group(1)), 0.6, output)
        status, output = self.run_console(lines, self.EXPECT, settle=0.2, linger=2.0)
        last = re.search(r"\[ *([0-9.]+)s\] found: NOWAYOS: hello from rust", output)
        self.assertLess(float(last.group(1)), 0.6, output)
        for bad in (0, 299, 4_000_001):
            status, output = self.run_console(lines, self.EXPECT, settle=0, baud=bad)
            self.assertEqual(status, 2, output)
            self.assertIn("--baud must be from 300 to 4000000", output)

    def test_a_serial_step_writes_to_qemus_serial_input(self):
        """QEMU's serial input is its stdin; a serial step's bytes
        reach it once the line it waits for has appeared."""
        with tempfile.TemporaryDirectory() as tmp:
            expect_file = write(tmp, "x.expect", "NOWAYOS: ready\nNOWAYOS: got abc\n")
            script = write(tmp, "x.qmp", "wait NOWAYOS: ready\nserial abc\n")
            fake_qemu = ["sh", "-c", "printf 'NOWAYOS: ready\\n'; x=$(head -c 3); "
                         "printf 'NOWAYOS: got %s\\n' \"$x\"; sleep 2"]
            result = subprocess.run(
                [sys.executable, os.path.join(HERE, "qemu-test"), "--expect", expect_file,
                 "--log", os.path.join(tmp, "serial.log"), "--timeout", "10", "--settle", "0",
                 "--qmp-script", script, "--"] + fake_qemu,
                capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)

    def test_a_run_that_ends_in_a_reset_passes_when_qemu_exits(self):
        """--ends-in-reset: the board resets, and QEMU, given
        -no-reboot, exits once every line has appeared: that passes."""
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting", "NOWAYOS: hello from rust"],
            self.EXPECT, settle=2.0, linger=0, extra=["--ends-in-reset"])
        self.assertEqual(status, 0, output)
        self.assertIn("then the board reset and QEMU exited", output)

    def test_a_run_that_must_end_in_a_reset_fails_if_qemu_runs_on(self):
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting", "NOWAYOS: hello from rust"],
            self.EXPECT, settle=0, linger=5, timeout=1.5, extra=["--ends-in-reset"])
        self.assertEqual(status, 1, output)
        self.assertIn("the board did not reset", output)

    def test_a_reset_before_the_last_line_still_fails(self):
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting"], self.EXPECT, settle=0, linger=0, extra=["--ends-in-reset"])
        self.assertEqual(status, 1, output)
        self.assertIn("QEMU exited (status 0) before it printed", output)

    def test_forbidden_output_before_the_reset_fails(self):
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting", "NOWAYOS: hello from rust", "x panicked y"],
            self.EXPECT, settle=0, linger=0, extra=["--ends-in-reset"])
        self.assertEqual(status, 1, output)
        self.assertIn("forbidden output 'panicked'", output)

    def test_qemu_exiting_while_settling_fails(self):
        status, output = self.run_console(
            ["NOWAYOS: rust pd starting", "NOWAYOS: hello from rust"],
            self.EXPECT, settle=2.0, linger=0)
        self.assertEqual(status, 1, output)
        self.assertIn("QEMU exited", output)


class QemuTestScreen(unittest.TestCase):
    """qemu-test --screen: reading QEMU's screendump (PPM) and comparing it
    with the approved image (PNG)."""

    tool = load("qemu-test")

    @staticmethod
    def ppm(width, height, pixel=b"\x10\x20\x30", header=None):
        head = header if header is not None else f"P6\n{width} {height}\n255\n".encode()
        return head + pixel * (width * height)

    def png(self, width, height, pixel=b"\x10\x20\x30"):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "x.png")
            self.tool.write_png(path, width, height, pixel * (width * height))
            with open(path, "rb") as handle:
                return handle.read()

    def files(self, tmp, screen, golden):
        paths = []
        for name, data in (("screen.ppm", screen), ("golden.png", golden)):
            path = os.path.join(tmp, name)
            if data is not None:
                with open(path, "wb") as handle:
                    handle.write(data)
            paths.append(path)
        return paths

    def compare(self, screen, golden, update=None):
        saved = os.environ.pop("NOWAYOS_UPDATE_SNAPSHOTS", None)
        if update is not None:
            os.environ["NOWAYOS_UPDATE_SNAPSHOTS"] = update
        try:
            with tempfile.TemporaryDirectory() as tmp:
                screen_path, golden_path = self.files(tmp, screen, golden)
                with contextlib.redirect_stdout(io.StringIO()):
                    problem = self.tool.compare_screen(screen_path, golden_path)
                actual = os.path.join(tmp, "golden.actual.png")
                written = None
                if os.path.exists(golden_path):
                    with open(golden_path, "rb") as handle:
                        written = handle.read()
                return problem, os.path.exists(actual), written
        finally:
            os.environ.pop("NOWAYOS_UPDATE_SNAPSHOTS", None)
            if saved is not None:
                os.environ["NOWAYOS_UPDATE_SNAPSHOTS"] = saved

    def test_the_same_screen_passes(self):
        self.assertEqual(self.compare(self.ppm(4, 3), self.png(4, 3)), (None, False, self.png(4, 3)))

    def test_a_different_pixel_fails_naming_the_first_and_keeps_the_screen(self):
        screen = bytearray(self.ppm(4, 3))
        screen[-3] ^= 1
        screen[len(b"P6\n4 3\n255\n") + 3 * 5] ^= 1
        problem, actual, _ = self.compare(bytes(screen), self.png(4, 3))
        self.assertIn("2 pixel(s) differ", problem)
        self.assertIn("the first at (1, 1)", problem)
        self.assertTrue(actual)

    def test_a_different_size_or_no_golden_image_fails(self):
        problem, _, _ = self.compare(self.ppm(4, 3), self.png(3, 4))
        self.assertIn("the screen is 4x3, the golden image 3x4", problem)
        problem, actual, _ = self.compare(self.ppm(4, 3), None)
        self.assertIn("create it with NOWAYOS_UPDATE_SNAPSHOTS=1", problem)
        self.assertFalse(actual)

    def test_update_mode_writes_the_golden_image_only_if_its_pixels_change(self):
        self.assertEqual(self.compare(self.ppm(2, 2), self.png(3, 3), update="1"), (None, False, self.png(2, 2)))
        problem, _, _ = self.compare(self.ppm(2, 2), self.png(3, 3), update="0")
        self.assertIn("the screen is 2x2, the golden image 3x3", problem)
        # The same pixels in another encoding are left as they are.
        written = self.png(2, 2)
        stored = zlib.compress(b"".join(b"\x00" + b"\x10\x20\x30" * 2 for _ in range(2)), 0)
        other = written[:written.index(b"IDAT") - 4] + self.chunk(b"IDAT", stored) + self.chunk(b"IEND", b"")
        self.assertNotEqual(other, written)
        self.assertEqual(self.compare(self.ppm(2, 2), other, update="1"), (None, False, other))

    @staticmethod
    def chunk(kind, body):
        return struct.pack(">I", len(body)) + kind + body + struct.pack(">I", zlib.crc32(kind + body))

    def test_the_png_reader_undoes_every_row_filter(self):
        # Two rows of three pixels, the second row under each filter in turn.
        top = bytes(range(10, 19))
        bottom = bytes([200, 7, 99, 3, 250, 128, 64, 1, 255])

        def filtered(kind):
            left = lambda n: bottom[n - 3] if n >= 3 else 0  # noqa: E731
            corner = lambda n: top[n - 3] if n >= 3 else 0  # noqa: E731

            def paeth(n):
                guess = left(n) + top[n] - corner(n)
                return min((abs(guess - left(n)), 0, left(n)), (abs(guess - top[n]), 1, top[n]),
                           (abs(guess - corner(n)), 2, corner(n)))[2]

            predict = {0: lambda n: 0, 1: left, 2: lambda n: top[n],
                       3: lambda n: (left(n) + top[n]) // 2, 4: paeth}[kind]
            return bytes([kind]) + bytes((bottom[n] - predict(n)) & 0xFF for n in range(9))

        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "x.png")
            for kind in range(5):
                with self.subTest(filter=kind):
                    raw = b"\x00" + top + filtered(kind)
                    with open(path, "wb") as handle:
                        handle.write(self.tool.PNG_SIGNATURE
                                     + self.chunk(b"IHDR", struct.pack(">II", 3, 2) + b"\x08\x02\x00\x00\x00")
                                     + self.chunk(b"IDAT", zlib.compress(raw)) + self.chunk(b"IEND", b""))
                    self.assertEqual(self.tool.read_png(path), (3, 2, top + bottom))

    def test_the_png_reader_refuses_other_kinds_and_damage(self):
        good = self.png(2, 1)
        rgba = good.replace(b"\x08\x02\x00\x00\x00", b"\x08\x06\x00\x00\x00", 1)
        ihdr = good.index(b"IHDR")
        rgba = rgba[:ihdr - 4] + self.chunk(b"IHDR", rgba[ihdr + 4:ihdr + 17]) + rgba[ihdr + 21:]
        flipped = bytearray(good)
        flipped[-13] ^= 1
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "x.png")
            for data, why in ((self.ppm(2, 1), "not a PNG"), (rgba, "not an 8-bit RGB"),
                              (bytes(flipped), "bad checksum"), (good[:-20], "truncated")):
                with self.subTest(why=why):
                    with open(path, "wb") as handle:
                        handle.write(data)
                    with self.assertRaisesRegex(ValueError, why):
                        self.tool.read_png(path)
            with open(path, "wb") as handle:
                handle.write(good)
            self.assertEqual(self.tool.read_png(path), (2, 1, b"\x10\x20\x30" * 2))

    def test_the_ppm_reader_takes_comments_and_refuses_other_formats(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = os.path.join(tmp, "x.ppm")
            for data, ok in ((self.ppm(2, 1, header=b"P6 # QEMU\n2 1\n255\n"), True),
                             (self.ppm(2, 1, header=b"P3\n2 1\n255\n"), False),
                             (self.ppm(2, 1, header=b"P6\n2 1\n65535\n"), False),
                             (self.ppm(2, 1)[:-1], False)):
                with self.subTest(data=data[:16]):
                    with open(path, "wb") as handle:
                        handle.write(data)
                    if ok:
                        self.assertEqual(self.tool.read_ppm(path)[:2], (2, 1))
                    else:
                        with self.assertRaises(ValueError):
                            self.tool.read_ppm(path)


class QemuTestQmpScript(unittest.TestCase):
    """qemu-test --qmp-script: reading the steps that drive QEMU."""

    tool = load("qemu-test")

    def steps(self, text):
        with tempfile.TemporaryDirectory() as tmp:
            return self.tool.read_qmp_script(write(tmp, "s.qmp", text))

    def test_wait_and_send_steps_in_order(self):
        steps = self.steps("""\
            # a comment
            wait NOWAYOS: ready
            send {"execute": "input-send-event", "arguments": {"events": []}}

            wait NOWAYOS: done
            """)
        self.assertEqual(steps, [
            ("wait", "NOWAYOS: ready"),
            ("send", {"execute": "input-send-event", "arguments": {"events": []}}),
            ("wait", "NOWAYOS: done"),
        ])

    def test_clicks_and_keys_become_paced_input_events(self):
        steps = self.steps("""\
            tablet 160 120
            click 40 50
            hold shift
            key a ret
            release shift
            pause 250
            screen ../golden/03.png
            """)
        sends = [value for verb, value in steps if verb == "send"]
        pauses = [value for verb, value in steps if verb == "pause"]
        # A click: the tablet to the pixel, scaled as the driver scales back
        # (value * 160 // 32768 == 40), then the left button down and up.
        self.assertEqual(sends[0]["arguments"]["events"], [
            {"type": "abs", "data": {"axis": "x", "value": 8192}},
            {"type": "abs", "data": {"axis": "y", "value": 13654}}])
        self.assertEqual([e["data"]["down"] for s in sends[1:3] for e in s["arguments"]["events"]],
                         [True, False])
        self.assertEqual(13654 * 120 // 32768, 50)
        # hold, key, release: each key event its own command, in order.
        keys = [(e["data"]["key"]["data"], e["data"]["down"])
                for s in sends[3:] for e in s["arguments"]["events"]]
        self.assertEqual(keys, [("shift", True), ("a", True), ("a", False), ("ret", True),
                                ("ret", False), ("shift", False)])
        # Every event is followed by the pace pause; then the script's own.
        self.assertEqual(pauses, [self.tool.PACE_SECONDS] * len(sends) + [0.25])
        verb, path = steps[-1]
        self.assertEqual(verb, "screen")
        self.assertTrue(path.endswith(os.path.join("golden", "03.png")) and os.path.isabs(path))

    def test_serial_steps_are_bursts_of_bytes(self):
        """A serial step is one burst of bytes for QEMU's serial
        port, with a terminal's escapes, then the pace pause."""
        steps = self.steps("""\
            wait NOWAYOS: ready
            serial Ada
            serial \\e[A\\eOD\\r\\n\\t\\b\\x7f\\\\\\x80\\xFF
            """)
        self.assertEqual(steps, [
            ("wait", "NOWAYOS: ready"),
            ("serial", b"Ada"), ("pause", self.tool.PACE_SECONDS),
            ("serial", b"\x1b[A\x1bOD\r\n\t\x08\x7f\\\x80\xff"), ("pause", self.tool.PACE_SECONDS),
        ])

    def test_malformed_serial_steps_are_refused(self):
        for text, reason in (("serial\n", "a step is wait"),
                             ("serial \\x01\n", "QEMU's monitor escape"),
                             ("serial a\\x01b\n", "QEMU's monitor escape"),
                             ("serial \\q\n", "an unknown escape"),
                             ("serial \\x4\n", "an unknown escape"),
                             ("serial \\\n", "an unknown escape"),
                             ("serial caf\u00e9\n", "write it as \\xNN")):
            with self.subTest(text=text):
                with self.assertRaises(ValueError) as caught:
                    self.steps(text)
                self.assertIn(reason, str(caught.exception))

    def test_the_tablet_reaches_every_pixel_and_no_further(self):
        for size in (120, 160, 480, 800):
            for pixel in range(size):
                value = self.tool.tablet_value(pixel, size)
                self.assertLessEqual(value, self.tool.ABS_MAX)
                self.assertEqual(value * size // (self.tool.ABS_MAX + 1), pixel)

    def test_malformed_steps_are_refused_with_their_line(self):
        for text, reason in (("send {not json}\n", "s.qmp:1: not JSON"),
                             ('send {"arguments": {}}\n', 'needs "execute"'),
                             ("wait\n", "a step is wait, send, pause"),
                             ("type hello\n", "a step is"),
                             ("click 1 2\n", "click before `tablet W H`"),
                             ("tablet 160 120\nclick 160 0\n", "off the 160x120 screen"),
                             ("tablet 0 120\n", "has no size"),
                             ("pause 60001\n", "at most 60000"),
                             ("pause soon\n", "whole number"),
                             ("hold shift ctrl\n", "hold takes one key name"),
                             ("key Shift\n", "key takes key names")):
            with self.subTest(text=text):
                with self.assertRaises(ValueError) as caught:
                    self.steps(text)
                self.assertIn(reason, str(caught.exception))


class SerialExpect(unittest.TestCase):
    """serial_expect.py: the expect-file rules qemu-test and check-serial share."""

    def setUp(self):
        sys.path.insert(0, HERE)
        self.addCleanup(sys.path.remove, HERE)
        import serial_expect
        self.rules = serial_expect

    def test_reads_expected_and_forbidden_lines(self):
        with tempfile.TemporaryDirectory() as tmp:
            path = write(tmp, "x.expect", """\
                # comment
                first line

                  second, indented
                !MON|ERROR
                """)
            expected, forbidden = self.rules.read_expectations(path)
        self.assertEqual(expected, ["first line", "  second, indented"])
        self.assertEqual(forbidden, ["MON|ERROR"])

    def test_refuses_unusable_expect_files(self):
        with tempfile.TemporaryDirectory() as tmp:
            for text, reason in [("# only a comment\n!panicked\n", "lists no expected lines"),
                                 ("line\n!\n", "'!' line with no text")]:
                with self.assertRaisesRegex(self.rules.ExpectError, reason):
                    self.rules.read_expectations(write(tmp, "x.expect", text))
            with self.assertRaisesRegex(self.rules.ExpectError, "cannot read"):
                self.rules.read_expectations(os.path.join(tmp, "missing.expect"))

    def test_every_line_ending_ends_a_line(self):
        split = self.rules.split_lines
        self.assertEqual(split(b"a\r\nb\nc\rd"), ["a", "b", "c", "d"])
        self.assertEqual(split(b"a\r\n"), ["a"])
        self.assertEqual(split(b"a\n\rb"), ["a", "", "b"])
        self.assertEqual(split(b""), [])
        self.assertEqual(split(b"\xffok\n"), ["\ufffdok"])

    def test_whole_lines_in_order(self):
        matcher = self.rules.Matcher(["one", "two"], ["panicked"])
        self.assertFalse(matcher.feed(1, "two"))  # out of order: not yet
        self.assertFalse(matcher.feed(2, "one more"))  # not a whole line
        self.assertTrue(matcher.feed(3, "one"))
        self.assertEqual(matcher.missing, "two")
        self.assertTrue(matcher.feed(4, "two"))
        self.assertTrue(matcher.complete)
        self.assertEqual(matcher.seen, [3, 4])
        self.assertIsNone(matcher.offending)

    def test_forbidden_text_anywhere_stops_the_match(self):
        matcher = self.rules.Matcher(["one", "two"], ["MON|ERROR"])
        matcher.feed(1, "one")
        matcher.feed(2, "x MON|ERROR: fault")
        self.assertEqual(matcher.offending, (2, "x MON|ERROR: fault", "MON|ERROR"))
        self.assertFalse(matcher.feed(3, "two"))
        self.assertFalse(matcher.complete)


class CheckSerial(unittest.TestCase):
    """tools/check-serial, on synthetic board logs."""

    # Synthetic boot output, standing in for what firmware and U-Boot print
    # before the Microkit loader. It holds forbidden texts, which are ignored.
    BOOT = [b"RPi: BOOTLOADER release", b"U-Boot: cannot find device, panicked? no",
            b"## Starting application at 0x10000000 ..."]
    LOADER = [b"LDR|INFO: loader for seL4 starting", b"MON|INFO: Microkit Monitor started!"]
    LINES = [b"NOWAYOS: rust pd starting", b"NOWAYOS: hello from rust"]

    def check(self, log, expect=QemuTest.EXPECT, *, raw=False):
        with tempfile.TemporaryDirectory() as tmp:
            expect_file = write(tmp, "x.expect", expect)
            log_file = os.path.join(tmp, "serial.log")
            with open(log_file, "wb") as handle:
                handle.write(log if raw else b"\r\n".join(log) + b"\r\n")
            result = subprocess.run(
                [sys.executable, os.path.join(HERE, "check-serial"),
                 "--expect", expect_file, "--log", log_file],
                capture_output=True, text=True, timeout=60,
            )
            return result.returncode, result.stdout + result.stderr

    def test_a_good_boot_passes_ignoring_boot_output(self):
        status, output = self.check(self.BOOT + self.LOADER + self.LINES + [b"idle"])
        self.assertEqual(status, 0, output)
        self.assertIn("skipping 3 line(s) of boot output; the loader starts at line 4", output)
        self.assertIn("[line     6] found: NOWAYOS: rust pd starting", output)
        self.assertIn("PASS - all 2 line(s) seen in order; last at line 7; "
                      "nothing forbidden in the 1 line(s) after it", output)

    def test_any_line_ending_passes(self):
        lines = self.LOADER + self.LINES
        for ending in (b"\n", b"\r", b"\n\r", b"\r\r\n"):
            status, output = self.check(ending.join(lines), raw=True)
            self.assertEqual(status, 0, f"{ending!r}: {output}")

    def test_every_system_passes_on_its_own_expected_output(self):
        systems = os.path.join(HERE, os.pardir, "systems")
        names = sorted(name[:-len(".expect")] for name in os.listdir(systems) if name.endswith(".expect"))
        self.assertIn("ui-isolation-widened", names)
        for name in names:
            path = os.path.join(systems, f"{name}.expect")
            with open(path, encoding="utf-8") as handle:
                expect = handle.read()
            lines = [line.encode() for line in expect.splitlines()
                     if line.strip() and not line.startswith(("#", "!"))]
            status, output = self.check(self.BOOT + self.LOADER + lines, expect)
            self.assertEqual(status, 0, f"{name}: {output}")

    def test_forbidden_output_after_the_loader_fails(self):
        status, output = self.check(
            self.BOOT + self.LOADER + self.LINES + [b"MON|ERROR: received message 0x6"])
        self.assertEqual(status, 1, output)
        self.assertIn("FAIL - forbidden output 'MON|ERROR' at line 8", output)
        self.assertIn("last lines of", output)

    def test_a_missing_line_fails(self):
        status, output = self.check(self.BOOT + self.LOADER + self.LINES[:1])
        self.assertEqual(status, 1, output)
        self.assertIn("FAIL - not seen in 6 line(s): 'NOWAYOS: hello from rust'", output)

    def test_lines_out_of_order_fail(self):
        status, output = self.check(self.LOADER + self.LINES[::-1])
        self.assertEqual(status, 1, output)
        self.assertIn("not seen", output)

    def test_expected_lines_in_the_boot_output_do_not_count(self):
        status, output = self.check(self.LINES + self.LOADER)
        self.assertEqual(status, 1, output)
        self.assertIn("not seen", output)

    def test_a_log_without_the_loader_fails(self):
        status, output = self.check(self.BOOT[:2] + self.LINES)
        self.assertEqual(status, 1, output)
        self.assertIn("no Microkit loader output", output)
        self.assertIn("nor U-Boot's '## Starting application at'", output)

    def test_a_release_images_log_is_checked_from_u_boots_go(self):
        # A release loader prints nothing, so the system's output
        # follows U-Boot's go line; what came before it is boot output, and
        # an expected line there does not count.
        status, output = self.check(self.BOOT + self.LINES + [b"idle"])
        self.assertEqual(status, 0, output)
        self.assertIn("no loader output (a release image's loader prints none), so checking from "
                      "line 4, after U-Boot's go at line 3", output)
        self.assertIn("[line     4] found: NOWAYOS: rust pd starting", output)
        status, output = self.check(self.LINES[:1] + self.BOOT + self.LINES[1:])
        self.assertEqual(status, 1, output)
        self.assertIn("not seen", output)
        # From the first go, as from the first loader line: a system that
        # resets the board (supervisor-panic) is checked across its boots.
        status, output = self.check(self.BOOT + self.LINES[:1] + self.BOOT[2:] + self.LINES[1:])
        self.assertEqual(status, 0, output)
        self.assertIn("checking from line 4, after U-Boot's go at line 3", output)

    def test_setup_errors_are_status_2(self):
        status, output = self.check(self.LOADER + self.LINES, "!panicked\n")
        self.assertEqual(status, 2, output)
        self.assertIn("lists no expected lines", output)
        result = subprocess.run(
            [sys.executable, os.path.join(HERE, "check-serial"), "--expect",
             os.path.join(HERE, os.pardir, "systems", "hello.expect"), "--log", "/nonexistent.log"],
            capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 2, result.stderr)
        self.assertIn("cannot read /nonexistent.log", result.stderr)

    def test_the_report_matches_qemu_test(self):
        _, board = self.check(self.LOADER + self.LINES)
        _, qemu = QemuTest().run_console(
            [line.decode() for line in self.LOADER + self.LINES], QemuTest.EXPECT,
            settle=0.2, linger=1.0)
        for output in (board, qemu):
            self.assertIn("found: NOWAYOS: hello from rust", output)
            self.assertIn("PASS - all 2 line(s) seen in order; last ", output)


SDF = textwrap.dedent("""\
    <?xml version="1.0" encoding="UTF-8"?>
    <system>
        <memory_region name="messages" size="0x1000" />
        <memory_region name="control" size="0x1000" />
        <protection_domain name="producer" priority="2" budget="5000" period="10000">
            <program_image path="producer.elf" />
            <map mr="messages" vaddr="0x4000_0000" perms="rw"
                 setvar_vaddr="nowayos_rw_region_0_vaddr" setvar_size="nowayos_rw_region_0_size" />
            <map mr="control" vaddr="0x4000_1000" perms="r"
                 setvar_vaddr="nowayos_ro_region_0_vaddr" setvar_size="nowayos_ro_region_0_size" />
            <protection_domain name="helper" id="1" priority="1">
                <program_image path="helper.elf" />
            </protection_domain>
        </protection_domain>
        <protection_domain name="consumer" priority="1">
            <program_image path="consumer.elf" />
            <map mr="messages" vaddr="0x4000_0000" perms="r"
                 setvar_vaddr="nowayos_ro_region_0_vaddr" setvar_size="nowayos_ro_region_0_size" />
            <map mr="control" vaddr="0x4000_1000"
                 setvar_vaddr="nowayos_rw_region_0_vaddr" setvar_size="nowayos_rw_region_0_size" />
        </protection_domain>
        <channel>
            <end pd="producer" id="0" />
            <end pd="consumer" id="0" />
        </channel>
    </system>
    """)

GRANTS = textwrap.dedent("""\
    # A comment.
    system demo
    role producer input
    role consumer ui
    role helper app
    pd producer priority=2 budget=5000 period=10000
    pd consumer priority=1 budget=1000 period=1000
    pd helper priority=1 budget=1000 period=1000
    child producer helper
    map producer messages rw
    map producer control r
    map consumer messages r
    map consumer control rw   # perms default to rw
    channel consumer producer
    """)


def mutated(text, old, new):
    """`text` with `old` replaced by `new`; fails if `old` is not there, so
    a mutation can never silently do nothing."""
    if old not in text:
        raise AssertionError(f"mutation target not found: {old!r}")
    return text.replace(old, new)


def capdl(rights, caps=None):
    """A minimal capDL spec in the Microkit tool's JSON shape: for each PD,
    a TCB whose page-table root reaches one frame per (region, read, write,
    executable) in rights[pd], and whose CSpace holds the caps in caps[pd]:
    (kind, object name, rights) for a Notification or Endpoint cap. The
    monitor's CNode is empty: no PD is passive."""
    objects = []

    def add(name, body):
        objects.append({"name": name, "object": body})
        return len(objects) - 1

    named = {}

    def kernel_object(name, kind):
        if name not in named:
            named[name] = add(name, kind)
        return named[name]

    for pd, frames in rights.items():
        leaf_slots = []
        for slot, (region, read, write, executable) in enumerate(frames):
            frame = add(f"frame_mr_{region}_{slot:09d}", {"Frame": {"size_bits": 12}})
            leaf_slots.append({"slot": slot, "cap": {"Frame": {
                "object": frame, "rights": {"read": read, "write": write, "grant": False, "grant_reply": False},
                "cached": True, "executable": executable}}})
        # The PD's own pages, named as the Microkit tool names them.
        for slot, (own, write, executable) in enumerate(
                ((f"frame_elf_{pd}_0", False, True), (f"frame_{pd}_stack_0", True, False),
                 (f"frame_ipcbuf_{pd}", True, False)), start=500):
            page = add(own, {"Frame": {"size_bits": 12}})
            leaf_slots.append({"slot": slot, "cap": {"Frame": {
                "object": page, "rights": {"read": True, "write": write}, "executable": executable}}})
        leaf = add(f"pt_{pd}", {"PageTable": {"level": 3, "slots": leaf_slots}})
        root = add(f"pud_{pd}", {"PageTable": {"is_root": True, "level": 1,
                                               "slots": [{"slot": 1, "cap": {"PageTable": {"object": leaf}}}]}})
        cnode = add(f"cnode_{pd}", {"CNode": {"size_bits": 9, "slots": [
            {"slot": 10 + n, "cap": {kind: {"object": kernel_object(name, kind), "badge": 1, "rights": {
                "read": "r" in cap_rights, "write": "w" in cap_rights, "grant": False, "grant_reply": False}}}}
            for n, (kind, name, cap_rights) in enumerate((caps or {}).get(pd, []))]}})
        add(f"tcb_{pd}", {"Tcb": {"slots": [{"slot": 0, "cap": {"CNode": {"object": cnode}}},
                                            {"slot": 1, "cap": {"PageTable": {"object": root}}}]}})
    add("cnode_monitor", {"CNode": {"size_bits": 9, "slots": []}})
    return {"objects": objects}


GOOD_RIGHTS = {
    "producer": [("messages", True, True, False), ("control", True, False, False)],
    "consumer": [("messages", True, False, False), ("control", True, True, False)],
    "helper": [],
}
# Each end of the producer-consumer channel can signal the other.
GOOD_CAPS = {
    "producer": [("Notification", "ntfn_producer", "r"), ("Notification", "ntfn_consumer", "w")],
    "consumer": [("Notification", "ntfn_consumer", "r"), ("Notification", "ntfn_producer", "w")],
    "helper": [("Notification", "ntfn_helper", "r"), ("Endpoint", "ep_producer", "w")],
}


class CheckSystem(unittest.TestCase):
    tool = load("check-system")

    def check(self, sdf=SDF, grants=GRANTS, rights=None):
        with tempfile.TemporaryDirectory() as tmp:
            argv = ["--sdf", write(tmp, "s.system", sdf), "--grants", write(tmp, "s.grants", grants)]
            if rights is not None:
                argv += ["--capdl", write(tmp, "capdl.json", json.dumps(capdl(rights, GOOD_CAPS)))]
            return run_main(self.tool, argv)

    def assert_fails(self, result, reason):
        status, _, err = result
        self.assertEqual(status, 1, err)
        self.assertIn(reason, err)

    def test_a_matching_system_passes(self):
        status, out, err = self.check(rights=GOOD_RIGHTS)
        self.assertEqual(status, 0, err)
        self.assertIn("description, grants and capDL agree", out)

    def test_rule_12_prefilled_configuration_is_mapped_read_only(self):
        prefilled = mutated(SDF, '<memory_region name="control" size="0x1000" />',
                            '<memory_region name="control" prefill_path="screen-1x1.bin" />')
        self.assert_fails(self.check(sdf=prefilled),
                          "rule 12: protection_domain 'consumer' map of 'control': the region is "
                          "prefilled configuration: map it read-only")
        read_only = mutated(prefilled, 'setvar_vaddr="nowayos_rw_region_0_vaddr" setvar_size="nowayos_rw_region_0_size" />\n    </protection_domain>',
                            'setvar_vaddr="nowayos_ro_region_1_vaddr" setvar_size="nowayos_ro_region_1_size" />\n    </protection_domain>')
        read_only = mutated(read_only, '<map mr="control" vaddr="0x4000_1000"\n', '<map mr="control" vaddr="0x4000_1000" perms="r"\n')
        status, _, err = self.check(sdf=read_only, grants=mutated(GRANTS, "map consumer control rw", "map consumer control r"))
        self.assertEqual(status, 0, err)

    def test_rule_12_a_test_only_stand_in_is_refused_outside_a_test_system(self):
        # A supervisor's stand-in counts, prefilled from test-counts-*.bin.
        stand_in = mutated(SDF, '<memory_region name="control" size="0x1000" />',
                           '<memory_region name="control" size="0x1000" />\n'
                           '    <memory_region name="counts" prefill_path="test-counts-1-0.bin" />')
        stand_in = mutated(stand_in, '        <protection_domain name="helper" id="1" priority="1">',
                           '        <map mr="counts" vaddr="0x5000_0000" perms="r"\n'
                           '             setvar_vaddr="nowayos_ro_region_1_vaddr" setvar_size="nowayos_ro_region_1_size" />\n'
                           '        <protection_domain name="helper" id="1" priority="1">')
        grants = GRANTS + "map producer counts r\n"
        for kind in ("demo", "product"):
            with self.subTest(kind=kind):
                self.assert_fails(self.check(sdf=stand_in, grants=mutated(grants, "system demo", f"system {kind}")),
                                  "rule 12: memory_region 'counts' is prefilled from test-counts-1-0.bin, a test-only "
                                  "stand-in: only a test system may have one")
        test_grants = mutated(grants, "system demo", "system test")
        status, _, err = self.check(sdf=stand_in, grants=test_grants)
        self.assertEqual(status, 0, err)
        # Read-only in a test system too.
        writable = mutated(stand_in, '<map mr="counts" vaddr="0x5000_0000" perms="r"\n'
                           '             setvar_vaddr="nowayos_ro_region_1_vaddr" setvar_size="nowayos_ro_region_1_size"',
                           '<map mr="counts" vaddr="0x5000_0000" perms="rw"\n'
                           '             setvar_vaddr="nowayos_rw_region_1_vaddr" setvar_size="nowayos_rw_region_1_size"')
        self.assert_fails(self.check(sdf=writable, grants=mutated(test_grants, "map producer counts r", "map producer counts rw")),
                          "rule 12: protection_domain 'producer' map of 'counts': the region is prefilled "
                          "configuration: map it read-only")

    def test_a_pd_pinned_to_a_core_is_a_fact_checked_against_capdl(self):
        # An SMP system names each PD's core; the grants say it, and
        # the capDL's thread must be bound to it. Core 0 is the default and
        # is never written.
        pinned = mutated(SDF, '<protection_domain name="consumer" priority="1">',
                         '<protection_domain name="consumer" priority="1" cpu="2">')
        grants = mutated(GRANTS, "pd consumer priority=1 budget=1000 period=1000",
                         "pd consumer priority=1 budget=1000 period=1000 cpu=2")
        self.assert_fails(self.check(sdf=pinned), "granted but not in")
        spec = capdl(GOOD_RIGHTS, GOOD_CAPS)
        for entry in spec["objects"]:
            if entry["name"] == "tcb_consumer":
                entry["object"]["Tcb"]["extra"] = {"affinity": 2}
        with tempfile.TemporaryDirectory() as tmp:
            argv = ["--sdf", write(tmp, "s.system", pinned), "--grants", write(tmp, "s.grants", grants),
                    "--capdl", write(tmp, "capdl.json", json.dumps(spec))]
            status, out, err = run_main(self.tool, argv)
        self.assertEqual(status, 0, err)
        # The capDL's thread on another core than the description's.
        self.assert_fails(self.check(sdf=pinned, grants=grants, rights=GOOD_RIGHTS),
                          "'consumer' runs on core 0, but the description puts it on core 2")
        status, _, err = self.check(grants=mutated(GRANTS, "pd consumer priority=1 budget=1000 period=1000",
                                                   "pd consumer priority=1 budget=1000 period=1000 cpu=0"))
        self.assertEqual(status, 2)
        self.assertIn("core 0 is the default: leave cpu= out", err)

    def test_a_widened_read_only_map_fails(self):
        # Regression (review 2026-10-01): this passed every CI step.
        widened = mutated(SDF, '<map mr="messages" vaddr="0x4000_0000" perms="r"',
                          '<map mr="messages" vaddr="0x4000_0000" perms="rw"')
        result = self.check(sdf=widened)
        self.assert_fails(result, "binds a read-only view, so the map must have perms=\"r\"")
        self.assert_fails(result, "granted but not in")

    def test_a_widened_grant_listed_in_both_still_fails_on_the_symbol(self):
        widened = mutated(SDF, '<map mr="messages" vaddr="0x4000_0000" perms="r"',
                          '<map mr="messages" vaddr="0x4000_0000" perms="rw"')
        grants = mutated(GRANTS, "map consumer messages r\n", "map consumer messages rw\n")
        self.assert_fails(self.check(sdf=widened, grants=grants), "binds a read-only view")

    def test_a_region_mapped_twice_into_one_pd_fails(self):
        alias = mutated(SDF, '<map mr="control" vaddr="0x4000_1000" perms="r"',
                        '<map mr="messages" vaddr="0x4000_1000" perms="r"')
        self.assert_fails(self.check(sdf=alias), "mapped into this PD more than once")

    def test_executable_and_write_only_maps_fail(self):
        for perms in ("rx", "rwx", "w"):
            sdf = mutated(SDF, 'vaddr="0x4000_1000" perms="r"', f'vaddr="0x4000_1000" perms="{perms}"')
            self.assert_fails(self.check(sdf=sdf), f"perms={perms!r}")

    def test_elements_and_attributes_it_does_not_model_fail(self):
        # Still refused now that device memory, interrupts, calls,
        # one-way channels, passive PDs, physical addresses and the
        # SMC capability are modelled: a PD on another core, virtual
        # machines, extra CSpace capabilities and channel-id symbols.
        cases = [
            ('<end pd="consumer" id="0" />', '<end pd="consumer" id="0" setvar_id="ch" />',
             "attribute(s) setvar_id"),
            ('<program_image path="consumer.elf" />',
             '<program_image path="consumer.elf" />\n<cspace><cap_tcb slot="1" pd="producer" /></cspace>',
             "<cspace>"),
            ('<program_image path="consumer.elf" />',
             '<program_image path="consumer.elf" />\n<virtual_machine name="vm" />', "<virtual_machine>"),
        ]
        for old, new, reason in cases:
            self.assert_fails(self.check(sdf=mutated(SDF, old, new)), reason)

    def test_a_second_channel_between_the_same_pds_fails(self):
        channel = '<channel>\n        <end pd="producer" id="0" />\n        <end pd="consumer" id="0" />\n    </channel>'
        twice = mutated(SDF, channel, channel + '\n    <channel><end pd="consumer" id="1" /><end pd="producer" id="1" /></channel>')
        self.assert_fails(self.check(sdf=twice), "more than one channel between consumer and producer")

    def test_grants_must_match_exactly(self):
        self.assert_fails(self.check(grants=GRANTS + "channel helper producer\n"),
                          "listed in")
        self.assert_fails(self.check(grants=mutated(GRANTS, "child producer helper\n", "")),
                          "granted but not in")
        self.assert_fails(self.check(grants=mutated(GRANTS, "budget=5000", "budget=6000")),
                          "granted but not in")

    def test_device_symbols_on_shared_memory_fail(self):
        sdf = mutated(SDF, 'setvar_vaddr="nowayos_rw_region_0_vaddr" setvar_size="nowayos_rw_region_0_size" />\n        <map mr="control"',
                      'setvar_vaddr="nowayos_device_0_vaddr" setvar_size="nowayos_device_0_size" />\n        <map mr="control"')
        self.assert_fails(self.check(sdf=sdf), "binds nowayos-hw's view of device registers, so the map "
                                               "must be of a device's register window")

    def test_symbol_pairs_must_match(self):
        sdf = mutated(SDF, 'setvar_size="nowayos_rw_region_0_size" />\n        <map mr="control"',
                      'setvar_size="nowayos_rw_region_1_size" />\n        <map mr="control"')
        self.assert_fails(self.check(sdf=sdf), "setvar_size must be 'nowayos_rw_region_0_size'")

    def test_capdl_rights_must_match_the_grants(self):
        writable = dict(GOOD_RIGHTS, consumer=[("messages", True, True, False), ("control", True, True, False)])
        self.assert_fails(self.check(rights=writable), "'consumer' maps 'messages' with read=True write=True")
        executable = dict(GOOD_RIGHTS, producer=[("messages", True, True, True), ("control", True, False, False)])
        self.assert_fails(self.check(rights=executable), "executable=True")
        reaches = dict(GOOD_RIGHTS, helper=[("control", True, False, False)])
        self.assert_fails(self.check(rights=reaches), "'helper' can reach memory region 'control'")
        missing = dict(GOOD_RIGHTS, consumer=[("messages", True, False, False)])
        self.assert_fails(self.check(rights=missing), "no page of it is in its page tables")
        no_tcb = {pd: frames for pd, frames in GOOD_RIGHTS.items() if pd != "helper"}
        self.assert_fails(self.check(rights=no_tcb), "no TCB for protection domain 'helper'")

    def test_a_pd_reaches_only_its_own_frames_and_its_regions(self):
        """Rule 6: another PD's program image, stack or IPC buffer
        page in a PD's page tables fails, naming the PD and the frame."""
        for stolen in ("frame_elf_producer_0", "frame_producer_stack_0", "frame_ipcbuf_producer"):
            with self.subTest(frame=stolen):
                spec = capdl(GOOD_RIGHTS, GOOD_CAPS)
                index = {entry["name"]: n for n, entry in enumerate(spec["objects"])}
                leaf = spec["objects"][index["pt_helper"]]["object"]["PageTable"]
                leaf["slots"].append({"slot": 600, "cap": {"Frame": {
                    "object": index[stolen], "rights": {"read": True, "write": False}, "executable": False}}})
                with tempfile.TemporaryDirectory() as tmp:
                    argv = ["--sdf", write(tmp, "s.system", SDF), "--grants", write(tmp, "s.grants", GRANTS),
                            "--capdl", write(tmp, "capdl.json", json.dumps(spec))]
                    self.assert_fails(run_main(self.tool, argv),
                                      f"'helper' reaches frame '{stolen}', which is neither its own")
        self.assertTrue(self.tool.own_frame("client_alpha", "frame_client_alpha_stack_3"))
        self.assertFalse(self.tool.own_frame("client", "frame_client_alpha_stack_3"))
        self.assertFalse(self.tool.own_frame("ui", "frame_elf_ui_2_5"))

    def test_unreadable_input_is_an_error_not_a_pass(self):
        status, _, err = self.check(sdf="<system><unclosed></system>")
        self.assertEqual(status, 2, err)
        status, _, err = self.check(grants="map only three\n")
        self.assertEqual(status, 2, err)
        broken = capdl(GOOD_RIGHTS)
        for entry in broken["objects"]:
            for slot in entry["object"].get("PageTable", {}).get("slots", []):
                if "Frame" in slot["cap"]:
                    slot["cap"]["Frame"]["object"] = -1
        with tempfile.TemporaryDirectory() as tmp:
            argv = ["--sdf", write(tmp, "s.system", SDF), "--grants", write(tmp, "s.grants", GRANTS),
                    "--capdl", write(tmp, "capdl.json", json.dumps(broken))]
            status, _, err = run_main(self.tool, argv)
        self.assertEqual(status, 2, err)
        self.assertIn("does not exist", err)


BOARD = textwrap.dedent("""\
    board = "testboard"

    [[device]]
    name = "uart"
    address = 0x0900_0000
    size = 0x1000
    irqs = [33]
    trigger = "level"
    source = "test fixture"

    [[device]]
    name = "net"
    address = 0x0a00_3000
    size = 0x1000
    irqs = [78, 79]
    trigger = "edge"
    source = "test fixture"
    """)

DEVICE_SDF = textwrap.dedent("""\
    <?xml version="1.0" encoding="UTF-8"?>
    <system>
        <memory_region name="uart_regs" size="0x1000" phys_addr="0x900_0000" />
        <memory_region name="net_regs" size="0x1000" phys_addr="0xa00_3000" />
        <protection_domain name="serial" priority="2">
            <program_image path="serial.elf" />
            <map mr="uart_regs" vaddr="0x200_0000" perms="rw" cached="false" setvar_vaddr="uart_base" />
            <irq irq="33" id="1" />
        </protection_domain>
        <protection_domain name="netdrv" priority="1">
            <program_image path="netdrv.elf" />
            <map mr="net_regs" vaddr="0x200_0000" perms="r" cached="false" />
            <irq irq="79" id="2" trigger="edge" />
        </protection_domain>
    </system>
    """)

DEVICE_GRANTS = textwrap.dedent("""\
    system demo
    role serial driver
    role netdrv driver
    pd serial priority=2 budget=1000 period=1000
    pd netdrv priority=1 budget=1000 period=1000
    device serial uart rw
    irq serial uart 33
    device netdrv net r
    irq netdrv net 79
    """)

# (region, read, write, executable, cached, paddr) per PD, as the Microkit
# tool would map them; and the interrupts whose handler caps each PD holds.
GOOD_DEVICE_FRAMES = {
    "serial": [("uart_regs", True, True, False, False, 0x0900_0000)],
    "netdrv": [("net_regs", True, False, False, False, 0x0a00_3000)],
}
GOOD_HANDLERS = {"serial": [33], "netdrv": [79]}


def device_capdl(frames, handlers, unclaimed=(), extra_frames=None, extra_caps=None, duplicate_irq=False):
    """A capDL spec in the Microkit tool's JSON shape (checked against a real
    one on 2026-10-02): device frames carry their paddr, frame caps their
    cached flag, and each PD's CSpace root CNode leads to a CNode holding an
    ArmIrqHandler cap for each of its interrupts, which the top-level "irqs"
    list ties to an interrupt number. A frame may add a seventh value, its
    size_bits (default 12). `unclaimed` adds handlers no PD holds;
    `extra_frames` adds (name, paddr) frames to a PD's page tables;
    `extra_caps` adds caps to a PD's CNode; `duplicate_irq` lists the first
    handler a second time under another interrupt."""
    objects, irqs = [], []

    def add(name, body):
        objects.append({"name": name, "object": body})
        return len(objects) - 1

    def handler(irq, owner):
        index = add(f"irq_{irq}_{owner}", {"ArmIrq": {"slots": [], "extra": {"trigger": 0, "target": 0}}})
        irqs.append({"irq": irq, "handler": index})
        return index

    for pd in sorted(set(frames) | set(handlers)):
        slots = []
        for slot, (region, read, write, executable, cached, paddr, *bits) in enumerate(frames.get(pd, [])):
            frame = add(f"frame_mr_{region}_{slot:09d}", {"Frame": {"size_bits": bits[0] if bits else 12,
                                                                   "paddr": paddr}})
            slots.append({"slot": slot, "cap": {"Frame": {
                "object": frame, "rights": {"read": read, "write": write, "grant": False, "grant_reply": False},
                "cached": cached, "executable": executable}}})
        for slot, (name, paddr) in enumerate((extra_frames or {}).get(pd, []), start=100):
            frame = add(name, {"Frame": {"size_bits": 12, "paddr": paddr}})
            slots.append({"slot": slot, "cap": {"Frame": {
                "object": frame, "rights": {"read": True, "write": True}, "cached": True, "executable": False}}})
        root = add(f"pt_{pd}", {"PageTable": {"is_root": True, "level": 0, "slots": slots}})
        cnode = add(f"cnode_{pd}", {"CNode": {"size_bits": 9, "slots": [
            {"slot": 139 + n, "cap": {"ArmIrqHandler": {"object": handler(irq, pd)}}}
            for n, irq in enumerate(handlers.get(pd, []))]
            + [{"slot": 200 + n, "cap": cap} for n, cap in enumerate((extra_caps or {}).get(pd, []))]}})
        cnode_root = add(f"cnode_{pd}_root", {"CNode": {"size_bits": 6, "slots": [
            {"slot": 0, "cap": {"CNode": {"object": cnode, "guard": 0, "guard_size": 49}}}]}})
        add(f"tcb_{pd}", {"Tcb": {"slots": [
            {"slot": 0, "cap": {"CNode": {"object": cnode_root, "guard": 0, "guard_size": 0}}},
            {"slot": 1, "cap": {"PageTable": {"object": root}}}]}})
    add("cnode_monitor", {"CNode": {"size_bits": 9, "slots": []}})
    for irq in unclaimed:
        handler(irq, "nobody")
    if duplicate_irq:
        irqs.append({"irq": 1000, "handler": irqs[0]["handler"]})
    return {"objects": objects, "irqs": irqs}


class CheckSystemDevices(unittest.TestCase):
    """Rules 6, 7 and 8: device memory and interrupts, named in a board file."""

    tool = load("check-system")

    def check(self, sdf=DEVICE_SDF, grants=DEVICE_GRANTS, board=BOARD, frames=None, handlers=None,
              board_name="testboard", **capdl_extras):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "platforms/testboard.toml", board)
            argv = ["--sdf", write(tmp, "s.system", sdf), "--grants", write(tmp, "s.grants", grants),
                    "--platforms", os.path.join(tmp, "platforms")]
            if board_name:
                argv += ["--board", board_name]
            if frames is not None:
                spec = device_capdl(frames, handlers if handlers is not None else GOOD_HANDLERS, **capdl_extras)
                argv += ["--capdl", write(tmp, "capdl.json", json.dumps(spec))]
            return run_main(self.tool, argv)

    def assert_fails(self, result, *reasons):
        status, _, err = result
        self.assertEqual(status, 1, err)
        for reason in reasons:
            self.assertIn(reason, err)

    def assert_error(self, result, reason):
        status, _, err = result
        self.assertEqual(status, 2, err)
        self.assertIn(reason, err)

    def test_devices_and_interrupts_named_in_the_grants_pass(self):
        status, out, err = self.check(frames=GOOD_DEVICE_FRAMES)
        self.assertEqual(status, 0, err)
        self.assertIn("description, grants and capDL agree (6 grant(s))", out)

    def test_rule_7_a_device_region_is_exactly_one_window_on_the_board(self):
        moved = mutated(DEVICE_SDF, 'phys_addr="0x900_0000"', 'phys_addr="0x900_1000"')
        self.assert_fails(self.check(sdf=moved), "rule 7", "phys_addr 0x9001000 with size 0x1000 is not exactly")
        widened = mutated(DEVICE_SDF, 'name="uart_regs" size="0x1000"', 'name="uart_regs" size="0x2000"')
        self.assert_fails(self.check(sdf=widened), "rule 7", "size 0x2000 is not exactly")
        twice = mutated(DEVICE_SDF, '<memory_region name="net_regs"',
                        '<memory_region name="uart_again" size="0x1000" phys_addr="0x900_0000" />\n'
                        '    <memory_region name="net_regs"')
        self.assert_fails(self.check(sdf=twice), "rule 7", "device 'uart' is already covered by memory_region 'uart_regs'")

    def test_rule_7_a_device_map_is_uncached(self):
        for cached in ('', ' cached="true"'):
            sdf = mutated(DEVICE_SDF, 'perms="rw" cached="false"', f'perms="rw"{cached}')
            self.assert_fails(self.check(sdf=sdf), "rule 7", "device 'uart' must be mapped uncached")

    def test_rule_7_a_device_is_mapped_into_one_pd(self):
        shared = mutated(DEVICE_SDF, '<map mr="net_regs" vaddr="0x200_0000" perms="r" cached="false" />',
                         '<map mr="net_regs" vaddr="0x200_0000" perms="r" cached="false" />\n'
                         '        <map mr="uart_regs" vaddr="0x200_1000" perms="r" cached="false" />')
        grants = DEVICE_GRANTS + "device netdrv uart r\n"
        self.assert_fails(self.check(sdf=shared, grants=grants), "rule 7",
                          "device 'uart' is mapped into more than one PD (netdrv, serial)")

    def test_rule_2_a_device_map_is_not_executable(self):
        sdf = mutated(DEVICE_SDF, 'perms="rw" cached="false"', 'perms="rwx" cached="false"')
        self.assert_fails(self.check(sdf=sdf), "rule 2", "perms='rwx' is executable")

    def test_rule_8_interrupts_belong_to_the_board_with_its_trigger_and_one_pd(self):
        unknown = mutated(DEVICE_SDF, '<irq irq="33" id="1" />', '<irq irq="40" id="1" />')
        self.assert_fails(self.check(sdf=unknown), "rule 8", "interrupt 40 belongs to no device on board 'testboard'")
        trigger = mutated(DEVICE_SDF, '<irq irq="33" id="1" />', '<irq irq="33" id="1" trigger="edge" />')
        self.assert_fails(self.check(sdf=trigger), "rule 8", "trigger='edge', but device 'uart' is level-triggered")
        default = mutated(DEVICE_SDF, 'trigger="edge" />', '/>')
        self.assert_fails(self.check(sdf=default), "rule 8", "trigger='level', but device 'net' is edge-triggered")
        both = mutated(DEVICE_SDF, '<irq irq="79" id="2" trigger="edge" />',
                       '<irq irq="79" id="2" trigger="edge" />\n        <irq irq="33" id="3" />')
        self.assert_fails(self.check(sdf=both), "rule 8", "interrupt 33 already goes to protection_domain 'serial'")

    def test_rule_1_devices_and_interrupts_match_the_grants_both_ways(self):
        self.assert_fails(self.check(grants=mutated(DEVICE_GRANTS, "irq serial uart 33\n", "")),
                          "rule 1", "granted but not in", "irq serial uart 33")
        self.assert_fails(self.check(grants=mutated(DEVICE_GRANTS, "device netdrv net r", "device netdrv net rw")),
                          "granted but not in", "listed in")
        self.assert_fails(self.check(grants=DEVICE_GRANTS + "irq netdrv net 78\n"),
                          "listed in", "irq netdrv net 78")
        self.assert_fails(self.check(grants=DEVICE_GRANTS + "device netdrv gpu rw\n"),
                          "rule 7", "board 'testboard' has no device 'gpu'")
        self.assert_fails(self.check(grants=DEVICE_GRANTS + "irq netdrv net 33\n"),
                          "rule 8", "33 is not an interrupt of device 'net'")

    def test_the_board_is_needed_and_checked(self):
        self.assert_error(self.check(board_name=None), "pass --board")
        self.assert_error(self.check(board_name="nosuchboard"), "cannot read board file")
        self.assert_error(self.check(board=mutated(BOARD, "address = 0x0a00_3000", "address = 0x0900_0000")),
                          "overlaps device 'uart'")
        self.assert_error(self.check(board=mutated(BOARD, "address = 0x0a00_3000", "address = 0x0a00_3200")),
                          "multiples of 0x1000")
        self.assert_error(self.check(board=mutated(BOARD, "irqs = [78, 79]", "irqs = [33, 79]")),
                          "interrupt 33 is also device 'uart'")
        self.assert_error(self.check(board=mutated(BOARD, 'trigger = "edge"\nsource = "test fixture"',
                                                   'trigger = "edge"\nsource = " "')),
                          "every device needs its source")
        self.assert_error(self.check(board=BOARD + "colour = \"blue\"\n"), "unknown ['colour']")
        self.assert_error(self.check(board="colour = \"blue\"\n" + BOARD), "unknown top-level key(s) ['colour']")
        self.assert_error(self.check(board=mutated(BOARD, 'board = "testboard"', 'board = "otherboard"')),
                          "board = 'otherboard', expected 'testboard'")
        self.assert_error(self.check(board=mutated(BOARD, 'trigger = "level"', 'trigger = ["level"]')),
                          "trigger must be one of")
        self.assert_error(self.check(board=mutated(BOARD, "address = 0x0900_0000", "address = -4096")),
                          "positive multiples")
        # A description and grants that use no device need no board.
        with tempfile.TemporaryDirectory() as tmp:
            status, _, err = run_main(self.tool, ["--sdf", write(tmp, "s.system", SDF),
                                                  "--grants", write(tmp, "s.grants", GRANTS)])
        self.assertEqual(status, 0, err)

    def test_rule_6_capdl_device_frames_are_exact_and_uncached(self):
        cached = dict(GOOD_DEVICE_FRAMES, serial=[("uart_regs", True, True, False, True, 0x0900_0000)])
        self.assert_fails(self.check(frames=cached), "rule 6", "'serial' maps device 'uart' cached")
        outside = dict(GOOD_DEVICE_FRAMES, serial=[("uart_regs", True, True, False, False, 0x0901_0000)])
        self.assert_fails(self.check(frames=outside), "rule 6", "not wholly inside device 'uart'")
        # A large page at the right address still reaches past the window.
        large = dict(GOOD_DEVICE_FRAMES, serial=[("uart_regs", True, True, False, False, 0x0900_0000, 21)])
        self.assert_fails(self.check(frames=large), "rule 6", "0x9000000 (2^21 bytes), not wholly inside")
        writable = dict(GOOD_DEVICE_FRAMES, netdrv=[("net_regs", True, True, False, False, 0x0a00_3000)])
        self.assert_fails(self.check(frames=writable), "rule 6", "'netdrv' maps 'net_regs' with read=True write=True")
        executable = dict(GOOD_DEVICE_FRAMES, netdrv=[("net_regs", True, False, True, False, 0x0a00_3000)])
        self.assert_fails(self.check(frames=executable), "rule 6", "executable=True")

    def test_rule_6_capdl_irq_handlers_only_in_the_claiming_pds(self):
        moved = {"serial": [33, 79], "netdrv": []}
        self.assert_fails(self.check(frames=GOOD_DEVICE_FRAMES, handlers=moved), "rule 6",
                          "'serial' holds the handler for interrupt 79, which it is not granted",
                          "'netdrv' is granted interrupt 79, but holds no handler for it")
        self.assert_fails(self.check(frames=GOOD_DEVICE_FRAMES, unclaimed=[78]), "rule 6",
                          "interrupt 78 has a handler that no protection domain is granted")

    def test_rule_6_capdl_no_other_physical_addresses(self):
        through_buffer = mutated(DEVICE_SDF, '<memory_region name="net_regs"',
                                 '<memory_region name="buf" size="0x1000" />\n    <memory_region name="net_regs"')
        through_buffer = mutated(through_buffer, 'setvar_vaddr="uart_base" />',
                                 'setvar_vaddr="uart_base" />\n        <map mr="buf" vaddr="0x300_0000" perms="rw" />')
        grants = DEVICE_GRANTS + "map serial buf rw\n"
        frames = dict(GOOD_DEVICE_FRAMES, serial=GOOD_DEVICE_FRAMES["serial"] + [("buf", True, True, False, True, 0x0a00_3000)])
        self.assert_fails(self.check(sdf=through_buffer, grants=grants, frames=frames), "rule 6",
                          "'serial' reaches physical address 0xa003000 through frame 'frame_mr_buf_000000001'")
        self.assert_fails(self.check(frames=GOOD_DEVICE_FRAMES,
                                     extra_frames={"serial": [("frame_elf_serial_000000003", 0x0a00_3000)]}),
                          "rule 6", "through frame 'frame_elf_serial_000000003', which is not a device or DMA region it maps")

    def test_rule_6_capdl_capabilities_are_modelled_and_lists_are_unique(self):
        self.assert_fails(self.check(frames=GOOD_DEVICE_FRAMES, duplicate_irq=True), "rule 6",
                          "names interrupt 1000 or its handler")
        self.assert_fails(self.check(frames=GOOD_DEVICE_FRAMES, extra_caps={"serial": [{"IrqControl": {}}]}),
                          "rule 6", "'serial' holds a IrqControl capability, which check-system does not model")
        # The SMC capability is modelled, but only where it is granted.
        self.assert_fails(self.check(frames=GOOD_DEVICE_FRAMES, extra_caps={"netdrv": [{"ArmSmc": {}}]}),
                          "rule 6", "'netdrv' holds an SMC capability, which it is not granted")

    def test_rule_6_capdl_thread_capabilities_only_for_children(self):
        with tempfile.TemporaryDirectory() as tmp:
            spec = capdl(GOOD_RIGHTS)
            index = {entry["name"]: n for n, entry in enumerate(spec["objects"])}
            cnode = len(spec["objects"])
            spec["objects"].append({"name": "cnode_consumer", "object": {"CNode": {"size_bits": 9, "slots": [
                {"slot": 0, "cap": {"Tcb": {"object": index["tcb_producer"]}}}]}}})
            spec["objects"][index["tcb_consumer"]]["object"]["Tcb"]["slots"].append(
                {"slot": 0, "cap": {"CNode": {"object": cnode}}})
            argv = ["--sdf", write(tmp, "s.system", SDF), "--grants", write(tmp, "s.grants", GRANTS),
                    "--capdl", write(tmp, "capdl.json", json.dumps(spec))]
            status, _, err = run_main(self.tool, argv)
        self.assertEqual(status, 1, err)
        self.assertIn("'consumer' holds the thread capability 'tcb_producer', which is not one of its children", err)

    def test_rule_4_device_symbols_bind_only_a_device_window(self):
        bound = mutated(DEVICE_SDF, 'setvar_vaddr="uart_base"',
                        'setvar_vaddr="nowayos_device_0_vaddr" setvar_size="nowayos_device_0_size"')
        status, _, err = self.check(sdf=bound)
        self.assertEqual(status, 0, err)
        self.assert_fails(self.check(sdf=mutated(bound, 'setvar_size="nowayos_device_0_size"',
                                                 'setvar_size="nowayos_device_1_size"')),
                          "setvar_size must be 'nowayos_device_0_size'")
        self.assert_fails(self.check(sdf=mutated(bound, 'setvar_vaddr="nowayos_device_0_vaddr"',
                                                 'setvar_vaddr="nowayos_device_0_size"')),
                          "is not a nowayos-region or nowayos-hw vaddr symbol")
        as_region = mutated(DEVICE_SDF, 'setvar_vaddr="uart_base"',
                            'setvar_vaddr="nowayos_rw_region_0_vaddr" setvar_size="nowayos_rw_region_0_size"')
        self.assert_fails(self.check(sdf=as_region),
                          "binds nowayos-region's view of shared memory, but the map is the register window of device 'uart'")

    def test_rule_4_dma_symbols_never_bind_a_device(self):
        sdf = mutated(DEVICE_SDF, 'setvar_vaddr="uart_base"',
                      'setvar_vaddr="nowayos_dma_0_vaddr" setvar_size="nowayos_dma_0_size"')
        self.assert_fails(self.check(sdf=sdf),
                          "binds nowayos-hw's view of DMA memory, but the map is the register window of device 'uart'")

    def test_a_device_reached_through_system_registers_has_only_interrupts(self):
        timer = '\n[[device]]\nname = "tick"\nirqs = [30]\ntrigger = "level"\nsource = "test fixture"\n'
        sdf = mutated(DEVICE_SDF, '<irq irq="33" id="1" />', '<irq irq="33" id="1" />\n<irq irq="30" id="3" />')
        grants = DEVICE_GRANTS + "irq serial tick 30\n"
        status, _, err = self.check(sdf=sdf, grants=grants, board=BOARD + timer)
        self.assertEqual(status, 0, err)
        self.assert_fails(self.check(sdf=sdf, grants=grants + "device serial tick rw\n", board=BOARD + timer),
                          "device 'tick' has no register window to map")
        for broken, reason in [
            (timer.replace("irqs = [30]", "irqs = []"), "a device with no register window needs an interrupt"),
            (timer.replace('name = "tick"', 'name = "tick"\naddress = 0x0b00_0000'),
             "a register window needs both address and size"),
        ]:
            with self.subTest(reason):
                self.assert_error(self.check(sdf=sdf, grants=grants, board=BOARD + broken), reason)

    def test_rule_10_a_device_window_is_not_dma_memory(self):
        sdf = mutated(DEVICE_SDF, '<irq irq="33" id="1" />',
                      '<irq irq="33" id="1" />\n<setvar symbol="uart_paddr" region_paddr="uart_regs" />')
        self.assert_fails(self.check(sdf=sdf),
                          "rule 10: protection_domain 'serial' setvar 'uart_paddr': region_paddr='uart_regs' "
                          "is the register window of device 'uart'")

    def test_rule_5_region_names_are_unique(self):
        twice = mutated(DEVICE_SDF, '<memory_region name="net_regs" size="0x1000" phys_addr="0xa00_3000" />',
                        '<memory_region name="net_regs" size="0x1000" phys_addr="0xa00_3000" />\n'
                        '    <memory_region name="net_regs" size="0x1000" />')
        self.assert_fails(self.check(sdf=twice), "rule 5", "more than one memory_region has this name")

    def test_the_qemu_board_file_is_valid(self):
        devices = self.tool.read_board(os.path.join(HERE, os.pardir, "platforms", "qemu_virt_aarch64.toml"))
        self.assertEqual((devices["debug_uart"].address, devices["debug_uart"].irqs), (0x0900_0000, [33]))
        self.assertEqual(devices["virtio_mmio_24_31"].irqs, list(range(72, 80)))
        # Every window a page, but the xHCI controllers' BARs: 16 KiB each,
        # placed by their PD at pci_bar0.
        self.assertTrue(all(device.size == 0x1000 for device in devices.values()
                            if device.address is not None and device.pci_bar0 is None))
        for name, address in (("xhci0", 0x1000_0000), ("xhci1", 0x1000_4000)):
            xhci = devices[name]
            self.assertEqual((xhci.address, xhci.size, xhci.pci_bar0, xhci.dma_offset, xhci.compatible),
                             (address, 0x4000, address, None, "pci1b36,d"))
        self.assertEqual((devices["phys_timer"].address, devices["phys_timer"].irqs), (None, [30]))
        # QEMU's PL011 is a whole PrimeCell, its page all registers.
        self.assertEqual((devices["debug_uart"].compatible, devices["debug_uart"].registers),
                         ("arm,pl011", 0x1000))
        self.assertEqual((devices["phys_timer"].compatible, devices["phys_timer"].registers),
                         ("arm,armv8-timer", None))

    def test_a_boards_compatible_and_registers_are_checked(self):
        uart = 'name = "uart"\n'
        for broken, reason in [
            (BOARD.replace(uart, uart + "registers = 0x2000\n"), "registers must be a positive multiple of 4"),
            (BOARD.replace(uart, uart + "registers = 0x1fe\n"), "registers must be a positive multiple of 4"),
            (BOARD.replace(uart, uart + "registers = 0\n"), "registers must be a positive multiple of 4"),
            (BOARD.replace(uart, uart + 'registers = "0x200"\n'), "registers must be a positive multiple of 4"),
            (BOARD.replace(uart, uart + 'compatible = "arm, pl011"\n'), "compatible must be a devicetree"),
            (BOARD.replace(uart, uart + 'compatible = ""\n'), "compatible must be a devicetree"),
            (BOARD.replace(uart, uart + f'compatible = "{"a" * 65}"\n'), "compatible must be a devicetree"),
            (BOARD.replace(uart, uart + "compatible = 1\n"), "compatible must be a devicetree"),
            (BOARD + textwrap.dedent("""\

                [[device]]
                name = "tick"
                registers = 0x100
                irqs = [30]
                trigger = "level"
                source = "test fixture"
                """), "registers needs a register window"),
        ]:
            with self.subTest(reason=reason, broken=broken[-60:]):
                self.assert_error(self.check(board=broken), reason)
        status, _, err = self.check(board=BOARD.replace(uart, uart + 'registers = 0x200\ncompatible = "arm,pl011"\n'))
        self.assertEqual(status, 0, err)

    def test_a_framebuffers_mode_is_checked(self):
        """A framebuffer the firmware set up carries its mode - width,
        height, stride, an r5g6b5 format and the row its window starts at -
        and is a simple-framebuffer whose registers are exactly the window's
        rows of pixels."""
        fb = textwrap.dedent("""
            [[device]]
            name = "fb"
            compatible = "simple-framebuffer"
            address = 0x3f80_f000
            size = 0x3e_6000
            registers = 0x3e_5800
            irqs = []
            trigger = "level"
            framebuffer = { width = 1920, height = 1080, stride = 3840, format = "r5g6b5", first_row = 16 }
            source = "test fixture"
            """)
        status, _, err = self.check(board=BOARD + fb)
        self.assertEqual(status, 0, err)
        status, _, err = self.check(board=BOARD + fb.replace("first_row = 16 }", "first_row = 16, reserved = 0x80_0000 }"))
        self.assertEqual(status, 0, err)
        mode = "{ width = 1920, height = 1080, stride = 3840, format = \"r5g6b5\", first_row = 16 }"
        for old, new, reason in [
            (mode, mode.replace(", first_row = 16", ""), "framebuffer needs exactly"),
            (mode, mode.replace("r5g6b5", "x8r8g8b8"), "framebuffer format must be one of"),
            (mode, mode.replace("stride = 3840", "stride = 3838"), "cannot hold 1920 pixels"),
            (mode, mode.replace("width = 1920", "width = 0"), "must be positive 32-bit integers"),
            (mode, mode.replace("first_row = 16", "first_row = 1080"), "first_row must be a row of the screen"),
            (mode, mode.replace("first_row = 16", "first_row = 15"), "registers must be the window's bytes"),
            ('compatible = "simple-framebuffer"', 'compatible = "arm,pl011"', "a framebuffer needs a window"),
            ("first_row = 16 }", "first_row = 16, reserved = 0x3f_0000 }", "reserved must cover the framebuffer"),
            ("first_row = 16 }", "first_row = 16, reserved = 0x80_0001 }", "reserved must be a positive multiple"),
            ("first_row = 16 }", "first_row = 16, colour = 1 }", "framebuffer needs exactly"),
            ("registers = 0x3e_5800\n", "", "a framebuffer needs a window"),
        ]:
            with self.subTest(reason=reason, change=new[-40:]):
                self.assert_error(self.check(board=BOARD + fb.replace(old, new)), reason)

    def test_the_pi_5s_framebuffer_is_in_its_board_file(self):
        """Firmware_fb on rpi5b_2gb - the firmware's 1920 x 1080
        r5g6b5 framebuffer at 0x3f80_0000, its window every row of it, page
        aligned and ending with the framebuffer."""
        fb = self.tool.read_board(os.path.join(HERE, os.pardir, "platforms", "rpi5b_2gb.toml"))["firmware_fb"]
        self.assertEqual(fb.framebuffer, {"width": 1920, "height": 1080, "stride": 3840, "format": "r5g6b5",
                                          "first_row": 0, "reserved": 0x80_0000})
        # The firmware's whole reservation, 0x3f80_0000 to 0x4000_0000, is
        # rule 14's to keep clear.
        self.assertEqual(fb.address + fb.framebuffer["reserved"], 0x4000_0000)
        self.assertEqual(fb.address, 0x3F80_0000)
        self.assertEqual(fb.address + fb.registers, 0x3F80_0000 + 1080 * 3840)
        self.assertEqual(fb.size, -(-fb.registers // 0x1000) * 0x1000)
        self.assertLessEqual(fb.address + fb.size, 0x3FC0_0000)

    def test_rule_12_a_devices_facts_go_only_to_the_pd_that_holds_it(self):
        facts = '<memory_region name="uart_facts" prefill_path="device-uart.bin" />'
        sdf = mutated(mutated(DEVICE_SDF, '<memory_region name="net_regs" size="0x1000" phys_addr="0xa00_3000" />',
                              '<memory_region name="net_regs" size="0x1000" phys_addr="0xa00_3000" />\n    ' + facts),
                      '<irq irq="33" id="1" />',
                      '<irq irq="33" id="1" />\n'
                      '        <map mr="uart_facts" vaddr="0x4000_0000" perms="r"\n'
                      '             setvar_vaddr="nowayos_ro_region_5_vaddr" setvar_size="nowayos_ro_region_5_size" />')
        grants = DEVICE_GRANTS + "map serial uart_facts r\n"
        status, _, err = self.check(sdf=sdf, grants=grants)
        self.assertEqual(status, 0, err)
        # Mapped writable: configuration a PD could change.
        self.assert_fails(self.check(sdf=mutated(sdf, 'vaddr="0x4000_0000" perms="r"', 'vaddr="0x4000_0000" perms="rw"'),
                                     grants=grants.replace("uart_facts r", "uart_facts rw")),
                          "rule 12: protection_domain 'serial' map of 'uart_facts': the region is prefilled "
                          "configuration")
        # Given to a PD that does not hold the UART too.
        also = mutated(sdf, '<irq irq="79" id="2" trigger="edge" />',
                       '<irq irq="79" id="2" trigger="edge" />\n'
                       '        <map mr="uart_facts" vaddr="0x4000_0000" perms="r" />')
        self.assert_fails(self.check(sdf=also, grants=grants + "map netdrv uart_facts r\n"),
                          "rule 12: memory_region 'uart_facts', the facts of device 'uart': 'netdrv' maps it, "
                          "but not 'uart''s register window: a device's facts go only to the PD that holds it")
        # The facts of a device the board does not have, or has no window for.
        for name in ("gpu", "tick"):
            with self.subTest(device=name):
                self.assert_fails(
                    self.check(sdf=sdf.replace("device-uart.bin", f"device-{name}.bin"), grants=grants,
                               board=BOARD + textwrap.dedent("""\

                                   [[device]]
                                   name = "tick"
                                   irqs = [30]
                                   trigger = "level"
                                   source = "test fixture"
                                   """)),
                    f"rule 12: memory_region 'uart_facts', the facts of device '{name}': board 'testboard' has "
                    f"no device of that name with a register window")


BASELINE_CARGO = textwrap.dedent("""\
    [workspace]
    resolver = "3"
    members = ["crates/*"]

    [workspace.lints.rust]
    unsafe_code = "forbid"

    [workspace.lints.clippy]
    unwrap_used = "deny"
    expect_used = "deny"
    panic = "deny"
    todo = "deny"
    unimplemented = "deny"
    unreachable = "deny"
    panic_in_result_fn = "deny"
    allow_attributes_without_reason = "deny"
    indexing_slicing = { level = "deny", priority = 0 }
    arithmetic_side_effects = "forbid"
    undocumented_unsafe_blocks = "deny"

    [profile.release]
    overflow-checks = true
    panic = "abort"

    [profile.dev]
    panic = "abort"
    """)

with open(os.path.join(HERE, "baseline.toml"), "rb") as _handle:
    # The crates the baseline bans, which every fixture workspace bans too.
    BASELINE_BANS = [ban["crate"] for ban in tomllib.load(_handle)["deny"]["bans"]["deny"]]


def ban_line(name):
    """One entry of a fixture deny.toml's [bans] deny."""
    return f'    {{ crate = "{name}", reason = "test fixture" }},\n'


BASELINE_DENY = textwrap.dedent("""\
    [advisories]
    unmaintained = "all"
    unsound = "all"
    yanked = "deny"

    [licenses]
    allow = ["MIT", "Apache-2.0"]
    confidence-threshold = 0.95
    include-dev = true

    [licenses.private]
    ignore = true

    [bans]
    wildcards = "deny"
    """) + "deny = [\n" + "".join(ban_line(name) for name in BASELINE_BANS) + "]\n" + textwrap.dedent("""\

    [sources]
    unknown-registry = "deny"
    unknown-git = "deny"
    """)

CRATE = textwrap.dedent("""\
    [package]
    name = "good"
    version = "0.1.0"
    publish = false

    [lints]
    workspace = true
    """)

# The fixture workspace's Cargo.lock: its crate, and one from crates.io.
LOCK = textwrap.dedent("""\
    version = 4

    [[package]]
    name = "good"
    version = "0.1.0"
    dependencies = ["serde"]

    [[package]]
    name = "serde"
    version = "1.0.228"
    source = "registry+https://github.com/rust-lang/crates.io-index"
    """)


CHANNEL_SDF = textwrap.dedent("""\
    <?xml version="1.0" encoding="UTF-8"?>
    <system>
        <memory_region name="dma" size="0x1000" />
        <memory_region name="plain" size="0x1000" />
        <protection_domain name="server" priority="100" passive="true">
            <program_image path="server.elf" />
        </protection_domain>
        <protection_domain name="client" priority="50">
            <program_image path="client.elf" />
            <map mr="plain" vaddr="0x4000_0000" perms="rw" />
        </protection_domain>
        <protection_domain name="driver" priority="60">
            <program_image path="driver.elf" />
            <map mr="dma" vaddr="0x4000_0000" perms="rw" />
            <setvar symbol="dma_paddr" region_paddr="dma" />
            <protection_domain name="helper" id="1" priority="10">
                <program_image path="helper.elf" />
            </protection_domain>
        </protection_domain>
        <channel>
            <end pd="server" id="0" />
            <end pd="client" id="0" pp="true" />
        </channel>
        <channel>
            <end pd="client" id="1" />
            <end pd="driver" id="5" notify="false" />
        </channel>
    </system>
    """)

CHANNEL_GRANTS = textwrap.dedent("""\
    system demo
    role server app
    role client app
    role driver driver
    role helper app
    pd server priority=100 budget=1000 period=1000 passive
    pd client priority=50 budget=1000 period=1000
    pd driver priority=60 budget=1000 period=1000
    pd helper priority=10 budget=1000 period=1000
    child driver helper
    map client plain rw
    map driver dma rw
    paddr driver dma
    channel client server
    call client server
    channel client driver notify=client
    """)

R, W, RW_REPLY = {"read": True}, {"write": True}, {"write": True, "grant_reply": True}

# Each PD's Notification and Endpoint caps, (kind, object name, rights), as
# the Microkit 2.3.1 tool generates them for CHANNEL_SDF (checked against its
# --capdl-json output on 2026-10-03): its own notification (or, when it is
# called or has children, its own endpoint) to receive on; the endpoint that
# takes its faults (the monitor's, or its parent's for a child; a passive PD
# holds the monitor's twice); a notification cap for each PD it may notify;
# and an endpoint cap for each PD it may call.
GOOD_CHANNEL_CAPS = {
    "server": [("Endpoint", "ep_server", R), ("Endpoint", "ep_fault_monitor", RW_REPLY),
               ("Endpoint", "ep_fault_monitor", RW_REPLY), ("Notification", "ntfn_client", W)],
    "client": [("Notification", "ntfn_client", R), ("Endpoint", "ep_fault_monitor", RW_REPLY),
               ("Notification", "ntfn_server", W), ("Notification", "ntfn_driver", W),
               ("Endpoint", "ep_server", RW_REPLY)],
    "driver": [("Endpoint", "ep_driver", R), ("Endpoint", "ep_fault_monitor", RW_REPLY)],
    "helper": [("Notification", "ntfn_helper", R), ("Endpoint", "ep_driver", RW_REPLY)],
}
# (region, write, paddr) per PD; a region_paddr region's frames carry their
# physical address, as device frames do.
GOOD_CHANNEL_FRAMES = {
    "server": [], "client": [("plain", True, None)], "driver": [("dma", True, 0x6000_0000)], "helper": [],
}


def channel_capdl(caps=None, frames=None, passive=("server",), monitor=True):
    """A capDL spec in the Microkit tool's JSON shape, for CHANNEL_SDF: each
    PD's frames and Notification and Endpoint caps, and the monitor's CNode
    holding the scheduling context of each PD in `passive`."""
    objects, named = [], {}

    def add(name, body):
        objects.append({"name": name, "object": body})
        return len(objects) - 1

    def kernel_object(name, body):
        if name not in named:
            named[name] = add(name, body)
        return named[name]

    caps = GOOD_CHANNEL_CAPS if caps is None else caps
    frames = GOOD_CHANNEL_FRAMES if frames is None else frames
    for pd in sorted(set(caps) | set(frames)):
        frame_slots = []
        for slot, (region, write, paddr) in enumerate(frames.get(pd, [])):
            frame = add(f"frame_mr_{region}_{slot:09d}", {"Frame": {"size_bits": 12, "paddr": paddr}})
            frame_slots.append({"slot": slot, "cap": {"Frame": {
                "object": frame, "rights": {"read": True, "write": write}, "cached": True,
                "executable": False}}})
        root = add(f"pud_{pd}", {"PageTable": {"is_root": True, "level": 0, "slots": frame_slots}})
        cnode = add(f"cnode_{pd}", {"CNode": {"size_bits": 9, "slots": [
            {"slot": slot, "cap": {kind: {"object": kernel_object(name, kind), "badge": 0,
                                          "rights": dict({"read": False, "write": False, "grant": False,
                                                          "grant_reply": False}, **rights)}}}
            for slot, (kind, name, rights) in enumerate(caps.get(pd, []), start=1)]}})
        add(f"tcb_{pd}", {"Tcb": {"slots": [{"slot": 0, "cap": {"CNode": {"object": cnode}}},
                                            {"slot": 1, "cap": {"PageTable": {"object": root}}}]}})
    if monitor:
        add("cnode_monitor", {"CNode": {"size_bits": 9, "slots": [
            {"slot": 140 + n, "cap": {"SchedContext": {"object": add(f"sched_context_{pd}", {
                "SchedContext": {"size_bits": 8}})}}}
            for n, pd in enumerate(passive)]}})
    return {"objects": objects}


def with_cap(pd, cap, caps=None):
    """GOOD_CHANNEL_CAPS (or `caps`) with `cap` added to `pd`'s CSpace."""
    caps = dict(GOOD_CHANNEL_CAPS if caps is None else caps)
    caps[pd] = caps[pd] + [cap]
    return caps


def without_cap(pd, name):
    """GOOD_CHANNEL_CAPS without `pd`'s caps to the object `name`."""
    return dict(GOOD_CHANNEL_CAPS, **{pd: [cap for cap in GOOD_CHANNEL_CAPS[pd] if cap[1] != name]})


class CheckSystemChannels(unittest.TestCase):
    """Calls, one-way channels, passive PDs and physical addresses:
    rules 1, 5, 6, 9 and 10."""

    tool = load("check-system")

    def check(self, sdf=CHANNEL_SDF, grants=CHANNEL_GRANTS, spec=None):
        with tempfile.TemporaryDirectory() as tmp:
            argv = ["--sdf", write(tmp, "s.system", sdf), "--grants", write(tmp, "s.grants", grants)]
            if spec is not None:
                argv += ["--capdl", write(tmp, "capdl.json", json.dumps(spec))]
            return run_main(self.tool, argv)

    def assert_fails(self, result, *reasons):
        status, _, err = result
        self.assertEqual(status, 1, err)
        for reason in reasons:
            self.assertIn(reason, err)

    def test_calls_one_way_channels_passive_pds_and_paddrs_pass(self):
        status, out, err = self.check(spec=channel_capdl())
        self.assertEqual(status, 0, err)
        self.assertIn("description, grants and capDL agree (11 grant(s))", out)

    def test_rule_1_each_new_fact_must_be_listed(self):
        for fact in ("call client server\n", "paddr driver dma\n"):
            with self.subTest(fact):
                self.assert_fails(self.check(grants=mutated(CHANNEL_GRANTS, fact, "")),
                                  "granted but not in", fact.strip())
        for old, new in [(" passive\n", "\n"), (" notify=client\n", "\n")]:
            with self.subTest(old):
                self.assert_fails(self.check(grants=mutated(CHANNEL_GRANTS, old, new)),
                                  "granted but not in", "listed in")

    def test_rule_1_a_fact_the_description_does_not_grant_fails(self):
        for extra in ("call driver server\n", "paddr client plain\n", "paddr client dma\n"):
            with self.subTest(extra):
                self.assert_fails(self.check(grants=CHANNEL_GRANTS + extra), "listed in")
        self.assert_fails(self.check(grants=mutated(CHANNEL_GRANTS, "period=1000\npd driver",
                                                    "period=1000 passive\npd driver")),
                          "listed in", "pd client priority=50 budget=1000 period=1000 passive")

    def test_one_way_and_silent_channels_are_recorded(self):
        driver_only = mutated(mutated(CHANNEL_SDF, '<end pd="client" id="1" />',
                                      '<end pd="client" id="1" notify="false" />'),
                              '<end pd="driver" id="5" notify="false" />', '<end pd="driver" id="5" />')
        self.assertEqual(self.check(sdf=driver_only, grants=mutated(
            CHANNEL_GRANTS, "notify=client", "notify=driver"))[0], 0)
        silent = mutated(CHANNEL_SDF, '<end pd="client" id="1" />', '<end pd="client" id="1" notify="false" />')
        self.assertEqual(self.check(sdf=silent, grants=mutated(
            CHANNEL_GRANTS, "notify=client", "notify=none"))[0], 0)
        self.assert_fails(self.check(sdf=silent), "granted but not in", "notify=none")

    def test_rule_5_flags_are_true_or_false(self):
        for old, new in [('pp="true"', 'pp="yes"'), ('notify="false"', 'notify="0"'),
                         ('passive="true"', 'passive="True"')]:
            with self.subTest(new):
                self.assert_fails(self.check(sdf=mutated(CHANNEL_SDF, old, new)),
                                  f"{new.split('=')[0]}={new.split('=')[1][1:-1]!r}: only")

    def test_grants_lines_for_the_new_facts_are_parsed_strictly(self):
        for old, new in [("notify=client", "notify=server"), ("notify=client", "notify=client extra"),
                         (" passive\n", " active\n"), ("call client server", "call client"),
                         ("call client server", "call client server now"),
                         ("paddr driver dma", "paddr driver"), ("paddr driver dma", "paddr driver dma rw")]:
            with self.subTest(new):
                status, _, err = self.check(grants=mutated(CHANNEL_GRANTS, old, new))
                self.assertEqual(status, 2, err)

    def test_rule_9_a_call_goes_to_a_strictly_higher_priority(self):
        for priority in ("50", "40"):
            sdf = mutated(CHANNEL_SDF, 'name="server" priority="100"', f'name="server" priority="{priority}"')
            grants = mutated(CHANNEL_GRANTS, "pd server priority=100", f"pd server priority={priority}")
            self.assert_fails(self.check(sdf=sdf, grants=grants), "rule 9: channel client server: 'client'")
        backwards = mutated(mutated(CHANNEL_SDF, '<end pd="client" id="0" pp="true" />', '<end pd="client" id="0" />'),
                            '<end pd="server" id="0" />', '<end pd="server" id="0" pp="true" />')
        self.assert_fails(self.check(sdf=backwards, grants=mutated(CHANNEL_GRANTS, "call client server",
                                                                   "call server client")),
                          "rule 9: channel client server: 'server' (priority 100) may call 'client'")

    def test_rule_10_a_setvar_names_a_region_paddr_only(self):
        setvar = '<setvar symbol="dma_paddr" region_paddr="dma" />'
        for new, reason in [('<setvar symbol="dma_paddr" />', "rule 10:"),
                            ('<setvar region_paddr="dma" />', "rule 10:"),
                            ('<setvar symbol="dma_paddr" region_paddr="nowhere" />', "no memory_region"),
                            ('<setvar symbol="dma_paddr" region_paddr="dma" vaddr="0x1" />', "attribute(s) vaddr")]:
            with self.subTest(new):
                self.assert_fails(self.check(sdf=mutated(CHANNEL_SDF, setvar, new)), reason)

    def test_rule_4_dma_symbols_name_one_region(self):
        dma_map = '<map mr="dma" vaddr="0x4000_0000" perms="rw" />'
        bound_map = ('<map mr="dma" vaddr="0x4000_0000" perms="rw" setvar_vaddr="nowayos_dma_0_vaddr" '
                     'setvar_size="nowayos_dma_0_size" />')
        setvar = '<setvar symbol="dma_paddr" region_paddr="dma" />'
        bound = mutated(mutated(CHANNEL_SDF, dma_map, bound_map), setvar,
                        '<setvar symbol="nowayos_dma_0_paddr" region_paddr="dma" />')
        status, _, err = self.check(sdf=bound)
        self.assertEqual(status, 0, err)
        for new, reason in [
            (setvar, "maps 'dma' with nowayos_dma_0_vaddr, but learns no physical address for it"),
            ('<setvar symbol="nowayos_dma_0_paddr" region_paddr="plain" />',
             "nowayos_dma_0_vaddr binds 'dma', but nowayos_dma_0_paddr is the physical address of 'plain'"),
            ('<setvar symbol="nowayos_dma_1_paddr" region_paddr="dma" />',
             "nowayos_dma_1_paddr, but maps no region with nowayos_dma_1_vaddr"),
            ('<setvar symbol="nowayos_rw_region_0_vaddr" region_paddr="dma" />',
             "'nowayos_rw_region_0_vaddr' is not a nowayos-hw physical-address symbol"),
            ('<setvar symbol="nowayos_dma_0_paddr" region_paddr="dma" />\n'
             '<setvar symbol="nowayos_dma_0_paddr" region_paddr="dma" />', "is set twice in this PD"),
        ]:
            with self.subTest(reason):
                sdf = mutated(bound, '<setvar symbol="nowayos_dma_0_paddr" region_paddr="dma" />', new)
                self.assert_fails(self.check(sdf=sdf), reason)
        self.assert_fails(self.check(sdf=mutated(bound, 'setvar_size="nowayos_dma_0_size"',
                                                 'setvar_size="nowayos_dma_0_paddr"')),
                          "is not a nowayos-region or nowayos-hw size symbol")

    def test_rule_4_io_symbols_name_a_region_the_pd_never_maps(self):
        setvar = '<setvar symbol="dma_paddr" region_paddr="dma" />'
        io = mutated(CHANNEL_SDF, setvar, setvar + '\n<setvar symbol="nowayos_io_0_paddr" region_paddr="plain" />')
        grants = CHANNEL_GRANTS + "paddr driver plain\n"
        status, _, err = self.check(sdf=io, grants=grants)
        self.assertEqual(status, 0, err)
        own = mutated(CHANNEL_SDF, setvar, '<setvar symbol="nowayos_io_0_paddr" region_paddr="dma" />')
        self.assert_fails(self.check(sdf=own),
                          "learns 'dma''s physical address as nowayos_io_0_paddr, an I/O region it hands a "
                          "device but never maps, and maps it")
        twice = mutated(io, '<setvar symbol="nowayos_io_0_paddr" region_paddr="plain" />',
                        '<setvar symbol="nowayos_io_0_paddr" region_paddr="plain" />\n'
                        '<setvar symbol="nowayos_io_0_paddr" region_paddr="plain" />')
        self.assert_fails(self.check(sdf=twice, grants=grants), "is set twice in this PD")
        for symbol in ("nowayos_io_0_vaddr", "nowayos_io_x_paddr"):
            with self.subTest(symbol):
                bad = mutated(io, "nowayos_io_0_paddr", symbol)
                self.assert_fails(self.check(sdf=bad, grants=grants),
                                  f"'{symbol}' is not a nowayos-hw physical-address symbol")

    def test_rule_6_capdl_calls_need_exactly_their_endpoints(self):
        self.assert_fails(self.check(spec=channel_capdl(caps=without_cap("client", "ep_server"))),
                          "'client' may call through 'ep_server', but holds no capability")
        self.assert_fails(self.check(spec=channel_capdl(caps=with_cap("driver", ("Endpoint", "ep_server", RW_REPLY)))),
                          "'driver' can send to endpoint 'ep_server', which is neither a call")
        self.assert_fails(self.check(spec=channel_capdl(caps=with_cap("client", ("Endpoint", "ep_driver", RW_REPLY)))),
                          "'client' can send to endpoint 'ep_driver'")
        self.assert_fails(self.check(spec=channel_capdl(caps=with_cap("client", ("Endpoint", "ep_server", R)))),
                          "'client' can receive on Endpoint 'ep_server', which is not its own")
        self.assert_fails(self.check(spec=channel_capdl(caps=with_cap("server", ("Endpoint", "ep_fault_monitor",
                                                                                 {"write": True, "grant": True})))),
                          "'server' holds Endpoint 'ep_fault_monitor' with the grant right")

    def test_rule_6_capdl_notifications_follow_the_channels(self):
        self.assert_fails(self.check(spec=channel_capdl(caps=with_cap("driver", ("Notification", "ntfn_client", W)))),
                          "'driver' can signal notification 'ntfn_client', but the grants give it no channel")
        self.assert_fails(self.check(spec=channel_capdl(caps=without_cap("client", "ntfn_driver"))),
                          "'client' may notify through 'ntfn_driver', but holds no capability")
        self.assert_fails(self.check(spec=channel_capdl(caps=with_cap("helper", ("Notification", "ntfn_driver", R)))),
                          "'helper' can receive on Notification 'ntfn_driver', which is not its own")

    def test_rule_6_capdl_passive_pds_lose_their_scheduling_context(self):
        self.assert_fails(self.check(spec=channel_capdl(passive=())),
                          "'server' is passive, but the monitor does not hold its scheduling context")
        self.assert_fails(self.check(spec=channel_capdl(passive=("server", "client"))),
                          "the monitor holds the scheduling context of 'client', making it passive")
        self.assert_fails(self.check(spec=channel_capdl(monitor=False)), "no CNode 'cnode_monitor'")

    def test_rule_6_capdl_dma_frames_only_where_the_paddr_is_learned(self):
        unlearned_sdf = mutated(CHANNEL_SDF, '<setvar symbol="dma_paddr" region_paddr="dma" />', "")
        unlearned = mutated(CHANNEL_GRANTS, "paddr driver dma\n", "")
        self.assert_fails(self.check(sdf=unlearned_sdf, grants=unlearned, spec=channel_capdl()),
                          "'driver' reaches physical address 0x60000000 through frame 'frame_mr_dma_000000000'")
        fixed_plain = dict(GOOD_CHANNEL_FRAMES, client=[("plain", True, 0x6100_0000)])
        self.assert_fails(self.check(spec=channel_capdl(frames=fixed_plain)),
                          "'client' reaches physical address 0x61000000")


ROLE_SDF = textwrap.dedent("""\
    <?xml version="1.0" encoding="UTF-8"?>
    <system>
        <memory_region name="uart_regs" size="0x1000" phys_addr="0x900_0000" />
        <memory_region name="shared" size="0x1000" />
        <protection_domain name="subject" priority="10">
            <program_image path="subject.elf" />
            <map mr="uart_regs" vaddr="0x200_0000" perms="rw" cached="false" />
            <map mr="shared" vaddr="0x300_0000" perms="rw" />
            <setvar symbol="dma_paddr" region_paddr="shared" />
            <irq irq="33" id="1" />
            <protection_domain name="helper" id="0" priority="5">
                <program_image path="helper.elf" />
            </protection_domain>
        </protection_domain>
        <protection_domain name="peer" priority="20">
            <program_image path="peer.elf" />
        </protection_domain>
        <channel>
            <end pd="subject" id="2" pp="true" />
            <end pd="peer" id="0" />
        </channel>
    </system>
    """)

ROLE_FACTS = textwrap.dedent("""\
    pd subject priority=10 budget=1000 period=1000
    pd helper priority=5 budget=1000 period=1000
    pd peer priority=20 budget=1000 period=1000
    device subject uart rw
    irq subject uart 33
    child subject helper
    channel peer subject
    call subject peer
    map subject shared rw
    paddr subject shared
    """)

# ROLE_SDF with the SMC capability for the subject, and its grants line.
ROLE_SDF_SMC = ROLE_SDF.replace('<protection_domain name="subject" priority="10">',
                                '<protection_domain name="subject" priority="10" smc="true">')


def role_grants(subject="driver", peer="app", system="demo"):
    """Grants for ROLE_SDF: the subject PD holds one grant of every kind a
    role rule can forbid (device memory, an interrupt, a child, a channel and
    a call to the peer, memory, a physical address and the SMC capability),
    and is a PD at all (`pd`). By default the subject is a driver, which no
    rule forbids any of them."""
    return f"system {system}\nrole subject {subject}\nrole helper app\nrole peer {peer}\n" + ROLE_FACTS


# For the `only_writers` rules: `reader` maps `ring` read-only and
# its own `control` read-write, as a ring's consumer does; `writer` maps
# `ring` read-write and `control` read-only, as its producer does. `other`
# maps `spare` alone, which the reader never maps.
WRITER_SDF = textwrap.dedent("""\
    <?xml version="1.0" encoding="UTF-8"?>
    <system>
        <memory_region name="ring" size="0x1000" />
        <memory_region name="control" size="0x1000" />
        <memory_region name="spare" size="0x1000" />
        <protection_domain name="reader" priority="30">
            <program_image path="reader.elf" />
            <map mr="ring" vaddr="0x300_0000" perms="r" />
            <map mr="control" vaddr="0x301_0000" perms="rw" />
        </protection_domain>
        <protection_domain name="writer" priority="20">
            <program_image path="writer.elf" />
            <map mr="ring" vaddr="0x300_0000" perms="rw" />
            <map mr="control" vaddr="0x301_0000" perms="r" />
        </protection_domain>
        <protection_domain name="other" priority="10">
            <program_image path="other.elf" />
            <map mr="spare" vaddr="0x300_0000" perms="rw" />
        </protection_domain>
    </system>
    """)

WRITER_FACTS = textwrap.dedent("""\
    pd reader priority=30 budget=1000 period=1000
    pd writer priority=20 budget=1000 period=1000
    pd other priority=10 budget=1000 period=1000
    map reader ring r
    map reader control rw
    map writer ring rw
    map writer control r
    map other spare rw
    """)


def writer_grants(reader, writer, system, other="app", sdf=None):
    """WRITER_SDF (or `sdf`) and its grants, with the reader, the writer and
    the other PD in the given roles."""
    return (sdf or WRITER_SDF,
            f"system {system}\nrole reader {reader}\nrole writer {writer}\nrole other {other}\n" + WRITER_FACTS)


def prefill_fixture(role, system, symbol, prefill=None, bind=True):
    """For the `reads_prefill` rules: a system description and its grants in
    which `reader`, in role `role`, maps `config` read-only - bound to
    `symbol` and its size symbol unless `bind` is false - and `config` is
    prefilled from `prefill`, or from nothing when it is None."""
    size = symbol[:-len("vaddr")] + "size"
    fill = f' prefill_path="{prefill}"' if prefill is not None else ""
    setvars = f' setvar_vaddr="{symbol}" setvar_size="{size}"' if bind else ""
    sdf = ('<?xml version="1.0" encoding="UTF-8"?>\n<system>\n'
           f'    <memory_region name="config" size="0x1000"{fill} />\n'
           '    <protection_domain name="reader" priority="30">\n'
           '        <program_image path="reader.elf" />\n'
           f'        <map mr="config" vaddr="0x300_0000" perms="r"{setvars} />\n'
           '    </protection_domain>\n</system>\n')
    grants = (f"system {system}\nrole reader {role}\npd reader priority=30 budget=1000 period=1000\n"
              "map reader config r\n")
    return sdf, grants


class CheckSystemRoles(unittest.TestCase):
    """Rule 11: PD roles, and the grants that must never exist
    (tools/grant-rules.toml)."""

    tool = load("check-system")

    def check(self, sdf=ROLE_SDF, grants=None, rules=None, board=BOARD):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "platforms/testboard.toml", board)
            argv = ["--sdf", write(tmp, "s.system", sdf),
                    "--grants", write(tmp, "s.grants", grants or role_grants()),
                    "--platforms", os.path.join(tmp, "platforms"), "--board", "testboard"]
            if rules is not None:
                argv += ["--rules", write(tmp, "rules.toml", rules)]
            return run_main(self.tool, argv)

    def assert_fails(self, result, *reasons):
        status, _, err = result
        self.assertEqual(status, 1, err)
        for reason in reasons:
            self.assertIn(reason, err)

    def test_roles_that_forbid_nothing_pass(self):
        status, _, err = self.check()
        self.assertEqual(status, 0, err)

    def check_rule_fixture(self, rule, peer=None, system=None):
        """check-system on the role fixture with the subject in `rule`'s
        role (and the SMC capability, for a rule about it; and, for a rule
        about one device, the fixture's device given that name and mapped
        with the rule's rights)."""
        smc = rule["grant"] == "smc"
        grants = role_grants(subject=rule["role"], peer=peer or rule.get("peer_role", "app"),
                             system=system or rule.get("systems", ["demo"])[0])
        sdf, board = ROLE_SDF_SMC if smc else ROLE_SDF, BOARD
        if "device" in rule:
            name, rights = rule["device"], rule.get("rights", "rw")
            board = mutated(BOARD, 'name = "uart"', f'name = "{name}"')
            grants = mutated(grants, "device subject uart rw", f"device subject {name} {rights}")
            grants = mutated(grants, "irq subject uart 33", f"irq subject {name} 33")
            sdf = mutated(sdf, '<map mr="uart_regs" vaddr="0x200_0000" perms="rw"',
                          f'<map mr="uart_regs" vaddr="0x200_0000" perms="{rights}"')
        return self.check(sdf=sdf, grants=grants + ("smc subject\n" if smc else ""), board=board)

    def test_every_forbid_rule_has_a_failing_fixture(self):
        with open(os.path.join(HERE, "grant-rules.toml"), "rb") as handle:
            rules = tomllib.load(handle)
        self.assertGreaterEqual(len(rules["forbid"]), 4)
        for rule in rules["forbid"]:
            with self.subTest(rule["id"]):
                held = "runs in this" if rule["grant"] == "pd" else "is granted"
                self.assert_fails(self.check_rule_fixture(rule), f"rule 11 ({rule['id']}): 'subject', "
                                  f"role {rule['role']}, {held}", rule["reason"])
                if "peer_role" in rule:
                    status, _, err = self.check_rule_fixture(rule, peer="app")
                    self.assertNotIn(rule["id"], err, "the rule binds only towards its peer role")
                if "unless_peer_role" in rule:
                    spared = rule["unless_peer_role"]
                    for peer in [spared] if isinstance(spared, str) else spared:
                        status, _, err = self.check_rule_fixture(rule, peer=peer)
                        self.assertNotIn(rule["id"], err, f"the rule spares its peer role {peer}")
                for kind in set(rules["systems"]) - set(rule.get("systems", rules["systems"])):
                    status, _, err = self.check_rule_fixture(rule, system=kind)
                    self.assertNotIn(rule["id"], err, f"the rule does not bind in a {kind} system")

    def test_an_app_pd_holds_no_device_interrupt_physical_address_or_storage_channel(self):
        """Of the fixture's
        grants, an App PD keeps its child, its channel and call to the peer
        and its memory; device memory, an interrupt and a physical address
        are refused in every kind of system, and a channel to a storage PD
        in a product."""
        def refused(peer, system):
            status, _, err = self.check(grants=role_grants(subject="app", peer=peer, system=system))
            self.assertEqual(status, 1, err)
            return set(re.findall(r"rule 11 \(([a-z-]+)\): 'subject'", err))

        everywhere = {"app-no-device-memory", "app-no-interrupt", "app-no-physical-address"}
        for system in ("test", "demo", "product"):
            with self.subTest(system=system):
                self.assertEqual(refused("app", system), everywhere)
                storage = {"app-no-channel-to-storage"} if system == "product" else set()
                self.assertEqual(refused("storage", system), everywhere | storage)

    def test_a_session_pd_holds_no_device_interrupt_physical_address_or_child(self):
        """Of the fixture's grants, a session PD keeps
        its channel, call and memory; device memory, an interrupt, a physical
        address and a child are refused in every kind of system, and a channel
        to a display PD in a demo or product system."""
        everywhere = {"session-no-device-memory", "session-no-interrupt", "session-no-physical-address",
                      "session-no-children"}
        for system in ("test", "demo", "product"):
            with self.subTest(system=system):
                self.assertEqual(self.own_rules_refusing("session", "app", system), everywhere)
                display = {"session-no-channel-to-display"} if system != "test" else set()
                self.assertEqual(self.own_rules_refusing("session", "display", system), everywhere | display)

    def test_a_decoder_reaches_only_a_session_display_or_watchdog_pd(self):
        """A decoder PD holds no device memory,
        interrupt, physical address or child; its channel and call may go to
        a session or display PD, and a channel to a watchdog PD - but no call:
        that one carries notifications only - and to no other."""
        own = {"decoder-no-device-memory", "decoder-no-interrupt", "decoder-no-physical-address",
               "decoder-no-children"}
        for system in ("test", "demo", "product"):
            with self.subTest(system=system):
                for peer in ("session", "display"):
                    self.assertEqual(self.own_rules_refusing("decoder", peer, system), own, peer)
                self.assertEqual(self.own_rules_refusing("decoder", "watchdog", system),
                                 own | {"decoder-calls-only-to-session-display"})
                self.assertEqual(self.own_rules_refusing("decoder", "app", system),
                                 own | {"decoder-channels-only-to-session-display-watchdog",
                                        "decoder-calls-only-to-session-display"})
                # A notification-only channel to the watchdog is the decoder's.
                sdf = mutated(ROLE_SDF, '<end pd="subject" id="2" pp="true" />', '<end pd="subject" id="2" />')
                grants = mutated(role_grants(subject="decoder", peer="watchdog", system=system),
                                 "call subject peer\n", "")
                status, _, err = self.check(sdf=sdf, grants=grants)
                found = {rule_id for rule_id in re.findall(r"rule 11 \(([a-z-]+)\): 'subject'", err)
                         if rule_id.startswith("decoder-")}
                self.assertEqual(found, own, err)

    def test_unless_peer_role_names_a_role_or_a_list_of_them(self):
        """A forbid rule spares the peer roles its unless_peer_role
        names - one, or a list - and a list must name known roles."""
        with open(os.path.join(HERE, "grant-rules.toml"), encoding="utf-8") as handle:
            text = handle.read()
        for bad in ('unless_peer_role = []', 'unless_peer_role = ["session", "nobody"]',
                    'unless_peer_role = [1]'):
            with self.subTest(bad=bad):
                rules = mutated(text, 'unless_peer_role = ["session", "display", "watchdog"]', bad)
                status, _, err = self.check(rules=rules)
                self.assertEqual(status, 2, err)
                self.assertIn("forbid rule", err)

    def own_rules_refusing(self, subject, peer, system):
        """The ids of the rules for `subject`'s own role that refuse the role
        fixture's subject PD, given the peer's role and the kind of system."""
        status, _, err = self.check(grants=role_grants(subject=subject, peer=peer, system=system))
        found = {rule_id for rule_id in re.findall(r"rule 11 \(([a-z-]+)\): 'subject'", err)
                 if rule_id.startswith(f"{subject}-")}
        if found:
            self.assertEqual(status, 1, err)
        return found

    def test_a_watchdog_pd_holds_no_child_call_physical_address_or_memory(self):
        """Of the fixture's grants, a watchdog PD
        keeps its device, interrupt and channel; a child, a call and a
        physical address are refused in every kind of system, and memory in
        a demo or product system, where it must also run above every PD but
        the supervisor - here the peer, at 20 to its 10."""
        everywhere = {"watchdog-no-children", "watchdog-no-call", "watchdog-no-dma"}
        for system in ("test", "demo", "product"):
            for peer in ("supervisor", "app"):
                with self.subTest(system=system, peer=peer):
                    expected = set(everywhere)
                    if system != "test":
                        expected.add("watchdog-no-memory")
                        if peer != "supervisor":
                            expected.add("watchdog-above-every-pd-it-watches")
                    self.assertEqual(self.own_rules_refusing("watchdog", peer, system), expected)

    def test_an_update_pd_reaches_no_device_and_no_screen_or_input_pd(self):
        """In every kind of system an update PD is
        refused device memory, an interrupt and a physical address, and a
        channel to a ui, input, broker or display PD, each naming its rule.
        Its child, memory and channels to other PDs are left to review."""
        everywhere = {"update-no-device-memory", "update-no-interrupt", "update-no-physical-address"}
        screen_and_input = ("ui", "input", "broker", "display")
        for system in ("test", "demo", "product"):
            for peer in screen_and_input + ("app", "network", "storage", "security"):
                with self.subTest(system=system, peer=peer):
                    channel = {f"update-no-channel-to-{peer}"} if peer in screen_and_input else set()
                    self.assertEqual(self.own_rules_refusing("update", peer, system), everywhere | channel)

    def test_a_clock_pd_holds_no_device_interrupt_or_physical_address(self):
        """In every kind of system a clock PD is
        refused device memory, an interrupt and a physical address, each
        naming its rule, whatever its peer; its child, channel, call and
        memory are left to review, but for who may write what it reads
        (below) and the floor it reads, which the role fixture's
        subject does not map, so its rule refuses it too. The SMC capability
        stays the supervisor's alone."""
        everywhere = {"clock-no-device-memory", "clock-no-interrupt", "clock-no-physical-address",
                      "clock-floor-from-build-time"}
        for system in ("test", "demo", "product"):
            for peer in ("network", "driver", "test", "app"):
                with self.subTest(system=system, peer=peer):
                    self.assertEqual(self.own_rules_refusing("clock", peer, system), everywhere)
        smc = role_grants(subject="clock", system="test") + "smc subject\n"
        self.assert_fails(self.check(sdf=ROLE_SDF_SMC, grants=smc),
                          "rule 11 (smc-only-supervisor): 'subject', role clock, is granted")

    def writers_refused(self, reader, writer, system, sdf=None, grants=None):
        """The (rule id, PD) pairs of the `only_writers` refusals on the
        writer fixture, the reader in role `reader` and the writer in role
        `writer`, in a `system` system; and check-system's stderr."""
        fixture_sdf, fixture_grants = writer_grants(reader, writer, system)
        status, _, err = self.check(sdf=sdf or fixture_sdf, grants=grants or fixture_grants)
        found = set(re.findall(r"rule 11 \(([a-z-]+)\): '([a-z_]+)', role [a-z]+, can write", err))
        if found:
            self.assertEqual(status, 1, err)
        return found, err

    def test_every_only_writers_rule_has_failing_fixtures(self):
        """For each `only_writers` rule in tools/grant-rules.toml, a
        PD of every role and in every kind of system that maps read-write a
        region a PD of the rule's role reads fails, naming the rule, the PD,
        the region and the reason - unless its role is one of the rule's
        writers, or of its test writers in a test system. The reader's own
        read-write region, which the writer maps read-only, and a region the
        reader never maps bind nothing."""
        with open(os.path.join(HERE, "grant-rules.toml"), "rb") as handle:
            rules = tomllib.load(handle)
        self.assertTrue(rules["only_writers"])
        for rule in rules["only_writers"]:
            for system in rules["systems"]:
                allowed = set(rule["writers"]) | (set(rule.get("test_writers", [])) if system == "test" else set())
                for role in rules["roles"]:
                    with self.subTest(rule=rule["id"], system=system, writer=role):
                        found, err = self.writers_refused(rule["role"], role, system)
                        if role in allowed:
                            self.assertEqual(found, set(), err)
                            continue
                        # Two PDs of the rule's role each write what the other reads.
                        both = {(rule["id"], "reader")} if role == rule["role"] else set()
                        self.assertEqual(found, {(rule["id"], "writer")} | both, err)
                        self.assertIn(f"rule 11 ({rule['id']}): 'writer', role {role}, can write 'ring', which "
                                      f"'reader', role {rule['role']}, maps ('writer' maps it read-write; only "
                                      f"a {rule['writers'][0]} PD", err)
                        self.assertIn(rule["reason"], err)

    def test_a_writer_is_a_pd_that_maps_the_region_read_write_or_learns_its_address(self):
        """A region the reader maps read-write counts as one it reads;
        a PD that learns a region's physical address can have a device write
        it, so it counts as a writer even with the region mapped read-only;
        the reader itself is never its own writer. The clock's rule here."""
        shared = mutated(WRITER_SDF, '<map mr="ring" vaddr="0x300_0000" perms="r" />',
                         '<map mr="ring" vaddr="0x300_0000" perms="rw" />')
        _, grants = writer_grants("clock", "app", "test")
        found, err = self.writers_refused("clock", "app", "test", sdf=shared,
                                          grants=mutated(grants, "map reader ring r\n", "map reader ring rw\n"))
        self.assertEqual(found, {("clock-samples-only-from-network", "writer")}, err)

        dma = mutated(WRITER_SDF, '<map mr="ring" vaddr="0x300_0000" perms="rw" />',
                      '<map mr="ring" vaddr="0x300_0000" perms="r" />\n'
                      '            <setvar symbol="ring_paddr" region_paddr="ring" />')
        for system in ("test", "demo", "product"):
            with self.subTest(system=system):
                _, grants = writer_grants("clock", "driver", system)
                found, err = self.writers_refused(
                    "clock", "driver", system, sdf=dma,
                    grants=mutated(grants, "map writer ring rw\n", "map writer ring r\npaddr writer ring\n"))
                self.assertEqual(found, {("clock-samples-only-from-network", "writer")}, err)
                self.assertIn("'writer' learns its physical address, so a device it drives can write it", err)

        # The clock's own physical address is clock-no-physical-address's,
        # not this rule's: the clock is never its own writer.
        own = mutated(WRITER_SDF, '<map mr="control" vaddr="0x301_0000" perms="rw" />',
                      '<map mr="control" vaddr="0x301_0000" perms="rw" />\n'
                      '            <setvar symbol="control_paddr" region_paddr="control" />')
        _, grants = writer_grants("clock", "network", "demo")
        found, err = self.writers_refused("clock", "network", "demo", sdf=own,
                                          grants=grants + "paddr reader control\n")
        self.assertEqual(found, set(), err)
        self.assertIn("rule 11 (clock-no-physical-address): 'reader', role clock", err)

    def test_only_a_network_pd_or_a_test_pd_in_a_test_system_sends_a_clock_samples(self):
        """A PD that can write a region a clock PD
        maps fails, naming clock-samples-only-from-network and its reason,
        unless it is a network PD, or a test PD in a test system - in a demo
        or product system a test PD fails it too. A PD that writes only
        memory the clock never maps is not refused by it, nor is a PD that
        writes what a PD of another role reads."""
        reason = "the clock PD believes any sample not earlier than the image's build time"
        for system in ("test", "demo", "product"):
            for writer in ("ui", "input", "broker", "display", "driver", "network", "security", "storage",
                           "update", "app", "supervisor", "watchdog", "clock", "test"):
                with self.subTest(system=system, writer=writer):
                    found, err = self.writers_refused("clock", writer, system)
                    if writer == "network" or (writer == "test" and system == "test"):
                        self.assertEqual(found, set(), err)
                        continue
                    # A second clock is refused too: each writes what the other reads.
                    both = {("clock-samples-only-from-network", "reader")} if writer == "clock" else set()
                    self.assertEqual(found, {("clock-samples-only-from-network", "writer")} | both, err)
                    self.assertIn(f"rule 11 (clock-samples-only-from-network): 'writer', role {writer}, can "
                                  f"write 'ring', which 'reader', role clock, maps ('writer' maps it read-write; "
                                  f"only a network PD, or a test PD in a test system, may): {reason}", err)
            with self.subTest(system=system, reader="app"):
                found, err = self.writers_refused("app", "driver", system)
                self.assertEqual(found, set(), err)
        # The other PD, an app, writes only `spare`, which the clock never maps.
        found, err = self.writers_refused("clock", "network", "product")
        self.assertEqual(found, set(), err)

    def test_clock_test_gives_the_clocks_ring_to_its_stand_in_alone(self):
        """Clock-test passes (BoardSystem checks every system), and
        with its TLS stand-in given any role but network or test - or kept a
        test PD in a demo or product system - it fails, naming the rule."""
        board = load("board-system")
        devices = self.tool.read_board(os.path.join(HERE, os.pardir, "platforms", "qemu_virt_aarch64.toml"),
                                       "qemu_virt_aarch64")
        systems = os.path.join(HERE, os.pardir, "systems")
        with open(os.path.join(systems, "clock-test.system"), encoding="utf-8") as handle:
            sdf, problems = board.resolve(handle.read(), devices, "qemu_virt_aarch64")
        self.assertEqual(problems, [])
        with open(os.path.join(systems, "clock-test.grants"), encoding="utf-8") as handle:
            grants = handle.read()

        def check(text):
            with tempfile.TemporaryDirectory() as tmp:
                return run_main(self.tool, ["--sdf", write(tmp, "s.system", sdf), "--grants",
                                            write(tmp, "s.grants", text), "--board", "qemu_virt_aarch64"])

        status, _, err = check(grants)
        self.assertEqual(status, 0, err)
        for role in ("app", "driver", "ui", "security"):
            with self.subTest(role=role):
                self.assert_fails(check(mutated(grants, "role tls_stand_in test", f"role tls_stand_in {role}")),
                                  f"rule 11 (clock-samples-only-from-network): 'tls_stand_in', role {role}, can "
                                  f"write 'clock_samples', which 'clock', role clock, maps")
        for system in ("demo", "product"):
            with self.subTest(system=system):
                self.assert_fails(check(mutated(grants, "system test", f"system {system}")),
                                  "rule 11 (clock-samples-only-from-network): 'tls_stand_in', role test, can "
                                  "write 'clock_samples'")
        status, _, err = check(mutated(grants, "role tls_stand_in test", "role tls_stand_in network"))
        self.assertEqual(status, 0, err)

    def test_every_reads_prefill_rule_has_failing_fixtures(self):
        """For each `reads_prefill` rule in tools/grant-rules.toml, in every
        kind of system it binds, a PD of its role fails when the region it
        binds the rule's symbol to is not prefilled, or is prefilled from
        another file, and when it binds the symbol to no region - each
        failure naming the rule, the PD, why and the reason - and passes
        when the region is prefilled from the rule's file. A PD of another
        role is not bound by it, nor is any PD where the rule does not bind
        (the clock's floor, clock-floor-from-build-time)."""
        with open(os.path.join(HERE, "grant-rules.toml"), "rb") as handle:
            rules = tomllib.load(handle)
        self.assertTrue(rules["reads_prefill"])
        for rule in rules["reads_prefill"]:
            role, symbol, prefill = rule["role"], rule["symbol"], rule["prefill"]
            other_file = "screen-800x480.bin" if prefill != "screen-800x480.bin" else "build-time.bin"
            head = f"rule 11 ({rule['id']}): 'reader', role {role},"
            cases = [
                ((None, True), f"binds {symbol} to 'config', which is not prefilled (no prefill_path), so not "
                               f"from {prefill}"),
                ((other_file, True), f"binds {symbol} to 'config', which is prefilled from {other_file}, not "
                                     f"from {prefill}"),
                ((prefill, False), f'maps no region with setvar_vaddr="{symbol}", so it does not read {prefill}'),
            ]
            for system in rules["systems"]:
                binds = system in rule.get("systems", rules["systems"])
                with self.subTest(rule=rule["id"], system=system):
                    status, _, err = self.check(*prefill_fixture(role, system, symbol, prefill))
                    self.assertNotIn(rule["id"], err)
                    for (fill, bind), why in cases:
                        result = self.check(*prefill_fixture(role, system, symbol, fill, bind))
                        if binds:
                            self.assert_fails(result, f"{head} {why}: {rule['reason']}")
                        else:
                            self.assertNotIn(rule["id"], result[2])
                    other_role = "app" if role != "app" else "driver"
                    status, _, err = self.check(*prefill_fixture(other_role, system, symbol))
                    self.assertNotIn(rule["id"], err)

    def test_clock_test_s_clock_reads_its_floor_from_build_time_bin(self):
        """clock-test's clock binds read-only region 6 to the region
        prefilled from build-time.bin; with that region left unfilled, filled
        from other configuration, or not mapped at all, check-system fails,
        naming clock-floor-from-build-time and the clock (a-W-A1: before
        this rule a zero-filled floor, 1970, passed)."""
        board = load("board-system")
        devices = self.tool.read_board(os.path.join(HERE, os.pardir, "platforms", "qemu_virt_aarch64.toml"),
                                       "qemu_virt_aarch64")
        systems = os.path.join(HERE, os.pardir, "systems")
        with open(os.path.join(systems, "clock-test.system"), encoding="utf-8") as handle:
            sdf, problems = board.resolve(handle.read(), devices, "qemu_virt_aarch64")
        self.assertEqual(problems, [])
        with open(os.path.join(systems, "clock-test.grants"), encoding="utf-8") as handle:
            grants = handle.read()

        def check(text, listed=grants):
            with tempfile.TemporaryDirectory() as tmp:
                return run_main(self.tool, ["--sdf", write(tmp, "s.system", text), "--grants",
                                            write(tmp, "s.grants", listed), "--board", "qemu_virt_aarch64"])

        status, _, err = check(sdf)
        self.assertEqual(status, 0, err)
        head = "rule 11 (clock-floor-from-build-time): 'clock', role clock,"
        region = '<memory_region name="build_time" size="0x1000" prefill_path="build-time.bin" />'
        self.assert_fails(check(mutated(sdf, region, '<memory_region name="build_time" size="0x1000" />')),
                          f"{head} binds nowayos_ro_region_6_vaddr to 'build_time', which is not prefilled")
        self.assert_fails(check(mutated(sdf, 'prefill_path="build-time.bin"', 'prefill_path="screen-800x480.bin"')),
                          f"{head} binds nowayos_ro_region_6_vaddr to 'build_time', which is prefilled from "
                          f"screen-800x480.bin, not from build-time.bin")
        unmapped = re.sub(r'\s*<map mr="build_time" vaddr="0x4000_2000" perms="r"\s*'
                          r'setvar_vaddr="nowayos_ro_region_6_vaddr" setvar_size="nowayos_ro_region_6_size" />',
                          "", sdf, count=1)
        self.assertNotEqual(unmapped, sdf)
        self.assert_fails(check(unmapped, mutated(grants, "map clock build_time r\n", "")),
                          f'{head} maps no region with setvar_vaddr="nowayos_ro_region_6_vaddr"')

    def test_a_test_pd_runs_only_in_a_test_system(self):
        """A PD with the role test fails a demo or product
        system, naming the PD and the reason, whatever it holds; in a test
        system the role still binds nothing."""
        for system in ("demo", "product"):
            with self.subTest(system=system):
                self.assert_fails(self.check(grants=role_grants(subject="test", system=system)),
                                  f"rule 11 (test-role-only-in-test-systems): 'subject', role test, runs in "
                                  f"this {system} system: a test PD holds whatever its test needs")
                quiet = mutated(role_grants(system=system), "role helper app", "role helper test")
                self.assert_fails(self.check(grants=quiet), "rule 11 (test-role-only-in-test-systems): "
                                  f"'helper', role test, runs in this {system} system")
        status, _, err = self.check(grants=role_grants(subject="test", system="test"))
        self.assertEqual(status, 0, err)

    def test_no_ui_or_broker_pd_learns_a_physical_address(self):
        """A ui or broker PD granted a region's physical
        address fails in every kind of system, naming the rule."""
        for role in ("ui", "broker"):
            for system in ("test", "demo", "product"):
                with self.subTest(role=role, system=system):
                    self.assertIn(f"{role}-no-physical-address", self.own_rules_refusing(role, "app", system))

    def test_every_only_rule_has_a_failing_fixture(self):
        with open(os.path.join(HERE, "grant-rules.toml"), "rb") as handle:
            rules = tomllib.load(handle)
        self.assertTrue(rules["only"])
        for rule in rules["only"]:
            if "device" in rule:
                # A rule on one device of one board: its fixture is that board's
                # (BoardSystem.test_only_a_display_pd_maps_the_firmware_framebuffer).
                continue
            with self.subTest(rule["id"]):
                other = dict(rule, role="app")
                self.assert_fails(self.check_rule_fixture(other, system="test"),
                                  f"rule 11 ({rule['id']}): 'subject', role app, is granted")
                status, _, err = self.check_rule_fixture(rule, system="test")
                self.assertNotIn(rule["id"], err, f"a {rule['role']} PD may hold it")

    def test_an_smc_grant_is_modelled_both_ways(self):
        # Granted in the description but not listed, and listed but not granted.
        self.assert_fails(self.check(sdf=ROLE_SDF_SMC, grants=role_grants(subject="supervisor", system="test")),
                          "granted but not in")
        self.assert_fails(self.check(grants=role_grants(subject="supervisor", system="test") + "smc subject\n"),
                          "smc subject")

    def test_every_pd_has_one_known_role_and_the_system_a_kind(self):
        self.assert_fails(self.check(grants=mutated(role_grants(), "role helper app\n", "")),
                          "'helper' has no role")
        self.assert_fails(self.check(grants=mutated(role_grants(), "role helper app", "role helper wizard")),
                          "'helper' has role 'wizard', which is not one of")
        self.assert_fails(self.check(grants=role_grants() + "role ghost app\n"),
                          "role ghost app names no protection domain")
        self.assert_fails(self.check(grants=mutated(role_grants(), "system demo\n", "")),
                          "the grants file has no system line")
        self.assert_fails(self.check(grants=role_grants(system="staging")),
                          "system 'staging' is not one of")
        for twice in ("system demo\n", "role helper app\n"):
            status, _, err = self.check(grants=role_grants() + twice)
            self.assertEqual(status, 2, err)

    def test_test_only_images_run_only_in_test_systems(self):
        sdf = mutated(ROLE_SDF, 'path="peer.elf"', 'path="nowayos-isolation-probe.elf"')
        self.assert_fails(self.check(sdf=sdf), "rule 11 (test-only image): 'peer' runs "
                          "nowayos-isolation-probe.elf outside a test system")
        status, _, err = self.check(sdf=sdf, grants=role_grants(system="test"))
        self.assertEqual(status, 0, err)

    def test_development_only_images_never_run_in_a_product_system(self):
        """A development-only program - the debug UART's keys - runs
        in test and demo systems, and the build says so of each; never in a
        product system."""
        sdf = mutated(ROLE_SDF, 'path="peer.elf"', 'path="nowayos-uart-input.elf"')
        for system in ("test", "demo"):
            with self.subTest(system=system):
                status, out, err = self.check(sdf=sdf, grants=role_grants(system=system))
                self.assertEqual(status, 0, err)
                self.assertIn("check-system: development only - 'peer' runs nowayos-uart-input.elf, which no "
                              f"product system may run: this is a {system} image", out)
        self.assert_fails(self.check(sdf=sdf, grants=role_grants(system="product")),
                          "rule 11 (development-only image): 'peer' runs nowayos-uart-input.elf in a product "
                          "system: a development route, never a product's")
        # A product's own program running the same PD keeps the name's
        # ending, and the rule binds it; a name that only contains it is
        # another program.
        wrapped = mutated(ROLE_SDF, 'path="peer.elf"', 'path="acme-uart-input.elf"')
        self.assert_fails(self.check(sdf=wrapped, grants=role_grants(system="product")),
                          "rule 11 (development-only image): 'peer' runs acme-uart-input.elf in a product system")
        other = mutated(ROLE_SDF, 'path="peer.elf"', 'path="nowayos-uart-input-console.elf"')
        status, out, err = self.check(sdf=other, grants=role_grants(system="demo"))
        self.assertEqual(status, 0, err)
        self.assertNotIn("development only", out)
        # A program with a stand-in for a service in it, whatever its
        # role - the reference kiosk's application PD.
        stand_in = mutated(ROLE_SDF, 'path="peer.elf"', 'path="acme-kiosk-stand-in.elf"')
        status, out, err = self.check(sdf=stand_in, grants=role_grants(system="demo"))
        self.assertEqual(status, 0, err)
        self.assertIn("check-system: development only - 'peer' runs acme-kiosk-stand-in.elf", out)
        self.assert_fails(self.check(sdf=stand_in, grants=role_grants(system="product")),
                          "rule 11 (development-only image): 'peer' runs acme-kiosk-stand-in.elf in a product "
                          "system")
        # Another program says nothing of the kind.
        status, out, err = self.check(grants=role_grants(system="demo"))
        self.assertEqual(status, 0, err)
        self.assertNotIn("development only", out)

    def test_an_input_pd_reads_the_debug_uart_only_and_never_in_a_product(self):
        """Structurally as well as by name: an input PD maps the
        debug UART read-only, and holds it in no product system, whatever
        its program; a driver may hold it read-write, and an input PD
        another device as before."""
        def check(role, rights, system, device="debug_uart"):
            board = mutated(BOARD, 'name = "uart"', f'name = "{device}"')
            grants = role_grants(subject=role, system=system)
            grants = mutated(grants, "device subject uart rw", f"device subject {device} {rights}")
            grants = mutated(grants, "irq subject uart 33", f"irq subject {device} 33")
            sdf = mutated(ROLE_SDF, '<map mr="uart_regs" vaddr="0x200_0000" perms="rw"',
                          f'<map mr="uart_regs" vaddr="0x200_0000" perms="{rights}"')
            _, _, err = self.check(sdf=sdf, grants=grants, board=board)
            return {rule for rule in re.findall(r"rule 11 \(([a-z-]+)\): 'subject'", err)
                    if "debug-uart" in rule}
        self.assertEqual(check("input", "r", "test"), set())
        self.assertEqual(check("input", "r", "demo"), set())
        self.assertEqual(check("input", "rw", "demo"), {"input-reads-the-debug-uart-only"})
        self.assertEqual(check("input", "r", "product"), {"debug-uart-no-input-in-product"})
        self.assertEqual(check("driver", "rw", "product"), set())
        self.assertEqual(check("input", "rw", "demo", device="uart"), set())

    def test_the_development_only_programs(self):
        """The rules file names the debug UART's input PD, the scripted
        input PD and any program with a stand-in for a service in it
        development-only, and no system in systems/ that runs one is a
        product."""
        rules = self.tool.read_rules(self.tool.DEFAULT_RULES)
        self.assertEqual(rules["development_only"]["images"],
                         ["*-uart-input.elf", "*-input-script.elf", "*-stand-in.elf"])
        for image in ("nowayos-uart-input.elf", "nowayos-input-script.elf", "showcase-kiosk-stand-in.elf"):
            self.assertTrue(any(fnmatch.fnmatchcase(image, p) for p in rules["development_only"]["images"]))
        for image in ("showcase-kiosk.elf", "nowayos-stand.elf", "stand-in-app.elf"):
            self.assertFalse(any(fnmatch.fnmatchcase(image, p) for p in rules["development_only"]["images"]))
        found = 0
        for path in sorted(glob.glob(os.path.join(HERE, os.pardir, "systems", "**", "*.system"), recursive=True)):
            images = {e.get("path") for e in ET.parse(path).getroot().iter("program_image")}
            if any(fnmatch.fnmatchcase(image, p) for image in images for p in rules["development_only"]["images"]):
                found += 1
                with open(path[:-len(".system")] + ".grants", encoding="utf-8") as handle:
                    kind = re.search(r"^system (\w+)", handle.read(), re.MULTILINE).group(1)
                self.assertIn(kind, ("test", "demo"), path)
        self.assertGreaterEqual(found, 3, "ui-demo, uart-input-test and its Pi variant")

    def test_a_malformed_rules_file_is_an_error(self):
        good = '[[forbid]]\nid = "x"\nrole = "ui"\ngrant = "irq"\nreason = "why"\n'
        head = 'roles = ["ui", "app"]\nsystems = ["test", "demo"]\n'
        parent = '[[parent_of_all]]\nid = "p"\nrole = "ui"\nsystems = ["demo"]\nreason = "why"\n'
        above = '[[above_all]]\nid = "a"\nrole = "ui"\nexcept_role = "app"\nreason = "why"\n'
        budget = '[[full_budget]]\nid = "b"\nrole = "ui"\nsystems = ["demo"]\nreason = "why"\n'
        writers = '[[only_writers]]\nid = "w"\nrole = "ui"\nwriters = ["app"]\ntest_writers = ["ui"]\nreason = "why"\n'
        reads = ('[[reads_prefill]]\nid = "f"\nrole = "ui"\nsymbol = "nowayos_ro_region_6_vaddr"\n'
                 'prefill = "build-time.bin"\nreason = "why"\n')
        status, _, err = self.check(rules=head + good + parent + above + budget + writers + reads)
        self.assertNotEqual(status, 2, f"a well-formed rules file is read: {err}")
        status, _, err = self.check(rules=head + above.replace('except_role = "app"', 'except_role = ["app"]'))
        self.assertNotEqual(status, 2, f"except_role may list roles: {err}")
        status, _, err = self.check(rules=head + above.replace('except_role = "app"', 'except_role = ["app", "nobody"]'))
        self.assertEqual(status, 2, "an unknown role in except_role's list is refused")
        status, _, err = self.check(rules=head + reads.replace('reason = "why"', 'reason = "why"\nsystems = ["demo"]'))
        self.assertNotEqual(status, 2, f"a well-formed rules file is read: {err}")
        status, _, err = self.check(rules=head + writers.replace('test_writers = ["ui"]\n', ""))
        self.assertNotEqual(status, 2, f"a well-formed rules file is read: {err}")
        for rules in [
            head + good.replace('grant = "irq"', 'grant = "dance"'),
            head + good.replace('role = "ui"', 'role = "wizard"'),
            head + good.replace('reason = "why"\n', ""),
            head + good + good,
            head + good.replace('grant = "irq"', 'grant = "irq"\npeer_role = "app"'),
            'roles = ["ui"]\nsystems = ["demo"]\n',
            head + '[test_only]\nimages = ["a.elf"]\n',
            head + '[[test_only]]\nimages = ["a.elf"]\nreason = "why"\n',
            head + '[development_only]\nimages = ["a.elf"]\n',
            head + '[[development_only]]\nimages = ["a.elf"]\nreason = "why"\n',
            head + '[development_only]\nimages = "a.elf"\nreason = "why"\n',
            head + '[development_only]\nimages = ["a.elf"]\nreason = "why"\nsystems = ["demo"]\n',
            # A device and its rights bind only a rule about device memory.
            head + good.replace('grant = "irq"', 'grant = "irq"\ndevice = "uart"'),
            head + good.replace('grant = "irq"', 'grant = "device"\nrights = "w"'),
            head + good.replace('grant = "irq"', 'grant = "device"\ndevice = "no such"'),
            # A kind of rule check-system does not know is refused, never ignored.
            head + good.replace("[[forbid]]", "[[forbids]]"),
            head + '[forbid]\nid = "x"\nrole = "ui"\ngrant = "irq"\nreason = "why"\n',
            # The rules on a system's shape.
            head + parent.replace('role = "ui"', 'role = "wizard"'),
            head + parent.replace('systems = ["demo"]', 'systems = ["staging"]'),
            head + parent.replace('reason = "why"', 'reason = " "'),
            head + parent + parent,
            head + good + parent.replace('id = "p"', 'id = "x"'),
            head + parent.replace('reason = "why"', 'reason = "why"\nexcept_role = "app"'),
            head + above.replace('except_role = "app"', 'except_role = "ui"'),
            head + above.replace('except_role = "app"', 'except_role = "wizard"'),
            head + above.replace('reason = "why"', 'reason = "why"\ngrant = "irq"'),
            head + above.replace('reason = "why"\n', ""),
            # A full-budget rule names a role, and no other role or grant.
            head + budget.replace('role = "ui"', 'role = "wizard"'),
            head + budget.replace('reason = "why"', 'reason = "why"\nexcept_role = "app"'),
            head + budget.replace('reason = "why"', 'reason = "why"\ngrant = "irq"'),
            head + budget.replace('systems = ["demo"]', 'systems = ["staging"]'),
            head + budget + budget,
            # A rule on the PD itself forbids; it never names a peer or an only role.
            head + good.replace('grant = "irq"', 'grant = "pd"\npeer_role = "app"'),
            head + '[[only]]\nid = "o"\nrole = "ui"\ngrant = "pd"\nreason = "why"\n',
            # An only-writers rule names its role and lists of known
            # roles, and nothing else.
            head + writers.replace('writers = ["app"]\n', ""),
            head + writers.replace('writers = ["app"]', "writers = []"),
            head + writers.replace('writers = ["app"]', 'writers = "app"'),
            head + writers.replace('writers = ["app"]', 'writers = ["wizard"]'),
            head + writers.replace('test_writers = ["ui"]', 'test_writers = ["wizard"]'),
            head + writers.replace('test_writers = ["ui"]', "test_writers = []"),
            head + writers.replace('role = "ui"', 'role = "wizard"'),
            head + writers.replace('reason = "why"', 'reason = "why"\ngrant = "map"'),
            head + writers.replace('reason = "why"', 'reason = "why"\nsystems = ["demo"]'),
            head + writers.replace('reason = "why"', 'reason = " "'),
            head + good + writers.replace('id = "w"', 'id = "x"'),
            head + writers.replace("[[only_writers]]", "[only_writers]"),
            # A reads-prefill rule names its role, a read-only region's
            # vaddr symbol and a file, and nothing else.
            head + reads.replace('symbol = "nowayos_ro_region_6_vaddr"\n', ""),
            head + reads.replace('prefill = "build-time.bin"\n', ""),
            head + reads.replace("nowayos_ro_region_6_vaddr", "nowayos_rw_region_6_vaddr"),
            head + reads.replace("nowayos_ro_region_6_vaddr", "nowayos_ro_region_6_size"),
            head + reads.replace("nowayos_ro_region_6_vaddr", "build_time_vaddr"),
            head + reads.replace('symbol = "nowayos_ro_region_6_vaddr"', "symbol = 6"),
            head + reads.replace('"build-time.bin"', '" "'),
            head + reads.replace('"build-time.bin"', '"../build-time.bin"'),
            head + reads.replace('role = "ui"', 'role = "wizard"'),
            head + reads.replace('reason = "why"', 'reason = "why"\ngrant = "map"'),
            head + reads.replace('reason = "why"', 'reason = "why"\nsystems = ["staging"]'),
            head + reads.replace('reason = "why"', 'reason = " "'),
            head + reads + reads,
            head + good + reads.replace('id = "f"', 'id = "x"'),
            head + reads.replace("[[reads_prefill]]", "[reads_prefill]"),
        ]:
            with self.subTest(rules):
                status, _, err = self.check(rules=rules)
                self.assertEqual(status, 2, err)


def shape_system(pds, system):
    """A system description and its grants file holding nothing but PDs and
    who is whose parent: `pds` is (name, role, priority, parent or None) in
    order, with an optional fifth item (budget, period, passive) - Microkit's
    default, (1000, 1000, False), when left out. For the rules on a whole
    system's shape."""
    children = {}
    scheduling = {}
    for name, _, priority, parent, *rest in pds:
        children.setdefault(parent, []).append((name, priority))
        scheduling[name] = rest[0] if rest else (1000, 1000, False)

    def element(name, priority, depth, child_id=None):
        pad = "    " * depth
        number = f' id="{child_id}"' if child_id is not None else ""
        budget, period, passive = scheduling[name]
        extra = f' budget="{budget}" period="{period}"' + (' passive="true"' if passive else "")
        inner = "".join(element(child, level, depth + 1, index)
                        for index, (child, level) in enumerate(children.get(name, [])))
        return (f'{pad}<protection_domain name="{name}"{number} priority="{priority}"{extra}>\n'
                f'{pad}    <program_image path="{name}.elf" />\n{inner}{pad}</protection_domain>\n')

    def pd_line(name, priority):
        budget, period, passive = scheduling[name]
        return f"pd {name} priority={priority} budget={budget} period={period}" + (" passive" if passive else "")

    sdf = ('<?xml version="1.0" encoding="UTF-8"?>\n<system>\n'
           + "".join(element(name, priority, 1) for name, priority in children.get(None, [])) + "</system>\n")
    grants = (f"system {system}\n" + "".join(f"role {name} {role}\n" for name, role, *_ in pds)
              + "".join(f"{pd_line(name, priority)}\n" for name, _, priority, *_ in pds)
              + "".join(f"child {parent} {name}\n" for name, _, _, parent, *_ in pds if parent))
    return sdf, grants


class CheckSystemShape(unittest.TestCase):
    """Rule 11's rules on a whole system's shape: one supervisor, the
    parent of every other PD, in a product; in a demo or product system the
    supervisor above every PD with a full budget of its own, and the
    watchdog above every PD but the supervisor. Every fixture is generated
    from tools/grant-rules.toml."""

    tool = load("check-system")

    with open(os.path.join(HERE, "grant-rules.toml"), "rb") as _handle:
        RULES = tomllib.load(_handle)

    def check(self, pds, system):
        sdf, grants = shape_system(pds, system)
        with tempfile.TemporaryDirectory() as tmp:
            return run_main(self.tool, ["--sdf", write(tmp, "s.system", sdf),
                                        "--grants", write(tmp, "s.grants", grants)])

    def assert_names(self, result, rule_id, *reasons):
        status, _, err = result
        self.assertEqual(status, 1, err)
        self.assertIn(f"rule 11 ({rule_id}): ", err)
        for reason in reasons:
            self.assertIn(reason, err)

    def assert_spares(self, result, rule_id):
        self.assertNotIn(f"({rule_id})", result[2])

    def test_every_parent_of_all_rule_has_failing_fixtures(self):
        """In a product, no supervisor, two supervisors and a
        PD outside it each fail; one supervisor, every other PD's parent,
        passes the rule; and it binds no other kind of system."""
        self.assertTrue(self.RULES["parent_of_all"])
        for rule in self.RULES["parent_of_all"]:
            role, rule_id = rule["role"], rule["id"]
            plain = next(other for other in ("app", "driver") if other != role)
            good = [("boss", role, 200, None), ("worker", plain, 100, "boss"), ("other", plain, 50, "boss")]
            cases = {
                "no PD of the role": ([("boss", plain, 200, None)] + good[1:],
                                      f"no PD has the role {role}, and exactly one must"),
                "two": (good[:1] + [("worker", role, 100, "boss")] + good[2:],
                        f"2 PDs have the role {role} ('boss', 'worker'), and exactly one must"),
                "a PD outside it": (good + [("stray", plain, 10, None)],
                                    f"'stray' is not a child of the {role} 'boss' (it has no parent)"),
                "a grandchild": (good[:2] + [("other", plain, 50, "worker")],
                                 f"'other' is not a child of the {role} 'boss' (its parent is 'worker')"),
            }
            for kind in self.RULES["systems"]:
                binds = kind in rule.get("systems", self.RULES["systems"])
                with self.subTest(rule_id, system=kind):
                    self.assert_spares(self.check(good, kind), rule_id)
                    for name, (pds, reason) in cases.items():
                        with self.subTest(name):
                            if binds:
                                self.assert_names(self.check(pds, kind), rule_id, reason, rule["reason"])
                            else:
                                self.assert_spares(self.check(pds, kind), rule_id)

    def test_every_above_all_rule_has_failing_fixtures(self):
        """A PD at the rule's role's priority, or above it,
        fails unless it has the rule's except_role; the rule binds only the
        kinds of system it names."""
        self.assertGreaterEqual(len(self.RULES["above_all"]), 2)
        for rule in self.RULES["above_all"]:
            role, rule_id = rule["role"], rule["id"]
            excepted = rule.get("except_role", [])
            excepted = [excepted] if isinstance(excepted, str) else excepted
            plain = next(other for other in ("app", "driver", "ui") if other != role and other not in excepted)
            good = [("top", role, 200, None), ("middle", plain, 100, None), ("low", plain, 50, None)]
            cases = {
                "a PD at its priority": (good + [("rival", plain, 200, None)],
                                         f"'top', role {role}, has priority 200, not above 'rival' "
                                         f"(role {plain}, priority 200)"),
                "a PD above it": (good + [("rival", plain, 254, None)],
                                  f"not above 'rival' (role {plain}, priority 254)"),
            }
            for kind in self.RULES["systems"]:
                binds = kind in rule.get("systems", self.RULES["systems"])
                with self.subTest(rule_id, system=kind):
                    self.assert_spares(self.check(good, kind), rule_id)
                    for spared in excepted:
                        self.assert_spares(self.check(good + [("spared", spared, 254, None)], kind), rule_id)
                    for name, (pds, reason) in cases.items():
                        with self.subTest(name):
                            if binds:
                                self.assert_names(self.check(pds, kind), rule_id, reason, rule["reason"])
                            else:
                                self.assert_spares(self.check(pds, kind), rule_id)

    def test_every_full_budget_rule_has_failing_fixtures(self):
        """A PD of the rule's role whose budget is below its period,
        or that is passive, fails where the rule binds; with a full budget -
        Microkit's default, or any budget equal to its period - it passes."""
        self.assertTrue(self.RULES["full_budget"])
        for rule in self.RULES["full_budget"]:
            role, rule_id = rule["role"], rule["id"]
            plain = next(other for other in ("app", "driver") if other != role)
            good = [("top", role, 200, None), ("worker", plain, 100, "top")]
            full = [("top", role, 200, None, (2000, 2000, False))] + good[1:]
            cases = {
                "a budget below its period": ([("top", role, 200, None, (500, 1000, False))] + good[1:],
                                              f"'top', role {role}, has budget 500 and period 1000, not a full "
                                              f"budget"),
                "passive": ([("top", role, 200, None, (1000, 1000, True))] + good[1:],
                            f"'top', role {role}, is passive"),
            }
            for kind in self.RULES["systems"]:
                binds = kind in rule.get("systems", self.RULES["systems"])
                with self.subTest(rule_id, system=kind):
                    self.assert_spares(self.check(good, kind), rule_id)
                    self.assert_spares(self.check(full, kind), rule_id)
                    # Another role's budget is not this rule's business.
                    self.assert_spares(self.check(good[:1] + [("worker", plain, 100, "top", (500, 1000, True))],
                                                  kind), rule_id)
                    for name, (pds, reason) in cases.items():
                        with self.subTest(name):
                            if binds:
                                self.assert_names(self.check(pds, kind), rule_id, reason, rule["reason"])
                            else:
                                self.assert_spares(self.check(pds, kind), rule_id)

    def test_a_supervised_product_passes_and_each_slip_names_its_rule(self):
        """The supervisor and watchdog rules as a product would hold them: the
        supervisor the parent of every PD and highest; the watchdog above
        every PD but the supervisor. Each slip fails, naming its rule."""
        product = [("supervisor", "supervisor", 254, None), ("watchdog", "watchdog", 250, "supervisor"),
                   ("ui", "ui", 100, "supervisor"), ("app", "app", 90, "supervisor")]
        status, _, err = self.check(product, "product")
        self.assertEqual(status, 0, err)
        for name, pds, rule_id, reason in [
            ("the supervisor not highest", product[:2] + [("ui", "ui", 254, "supervisor")] + product[3:],
             "supervisor-highest-priority", "'supervisor', role supervisor, has priority 254, not above 'ui'"),
            ("the watchdog below a PD it watches", product[:3] + [("app", "app", 250, "supervisor")],
             "watchdog-above-every-pd-it-watches", "'watchdog', role watchdog, has priority 250, not above 'app'"),
            ("a PD outside the supervisor", product + [("stray", "app", 10, None)],
             "one-supervisor-the-parent-of-every-pd", "'stray' is not a child of the supervisor 'supervisor'"),
            ("the supervisor short of budget", [("supervisor", "supervisor", 254, None, (900, 1000, False))]
             + product[1:], "supervisor-full-budget",
             "'supervisor', role supervisor, has budget 900 and period 1000"),
        ]:
            with self.subTest(name):
                self.assert_names(self.check(pds, "product"), rule_id, reason)


class WidenedSystems(unittest.TestCase):
    """A widened system - a copy of a system with one grant widened on
    purpose, whose expect file requires the isolation failure its base
    forbids - must differ from its base by exactly that grant, so it keeps
    proving that the base's isolation test can fail."""

    SYSTEMS = os.path.join(HERE, os.pardir, "systems")

    # (base system, widened system, PD, the map added to it)
    WIDENED = [
        ("ui-isolation", "ui-isolation-widened", "ui_reads_display_private",
         {"mr": "display_private", "vaddr": "0x6000_0000", "perms": "r"}),
    ]

    def read(self, name):
        with open(os.path.join(self.SYSTEMS, name), encoding="utf-8") as handle:
            return handle.read()

    @staticmethod
    def facts(grants):
        return {line.strip() for line in grants.splitlines()
                if line.strip() and not line.lstrip().startswith("#")}

    @staticmethod
    def without_map(sdf, pd, attrs):
        """`sdf`, canonical, with the map `attrs` taken out of PD `pd`."""
        root = ET.fromstring(sdf)
        owner = next(e for e in root.iter("protection_domain") if e.get("name") == pd)
        added = [m for m in owner.findall("map") if m.attrib == attrs]
        assert len(added) == 1, f"{pd} has {len(added)} maps {attrs}"
        owner.remove(added[0])
        return ET.canonicalize(ET.tostring(root, encoding="unicode"), strip_text=True)

    def test_a_widened_system_differs_from_its_base_by_one_grant(self):
        for base, widened, pd, attrs in self.WIDENED:
            with self.subTest(widened):
                base_sdf = ET.canonicalize(self.read(f"{base}.system"), strip_text=True)
                self.assertEqual(self.without_map(self.read(f"{widened}.system"), pd, attrs),
                                 base_sdf, "the descriptions differ by more than the one map")
                added = self.facts(self.read(f"{widened}.grants")) - self.facts(self.read(f"{base}.grants"))
                removed = self.facts(self.read(f"{base}.grants")) - self.facts(self.read(f"{widened}.grants"))
                self.assertEqual(added, {f"map {pd} {attrs['mr']} {attrs['perms']}"})
                self.assertEqual(removed, set())

    def test_a_widened_system_requires_the_failure_its_base_forbids(self):
        for base, widened, pd, _ in self.WIDENED:
            with self.subTest(widened):
                base_lines = self.read(f"{base}.expect").splitlines()
                widened_lines = self.read(f"{widened}.expect").splitlines()
                self.assertIn("!ISOLATION FAILURE", base_lines)
                self.assertNotIn("!ISOLATION FAILURE", widened_lines)
                self.assertTrue(any(line.startswith(f"NOWAYOS: isolation-probe: ISOLATION FAILURE: {pd} ")
                                    for line in widened_lines))


class IsolationProbesInStep(unittest.TestCase):
    """Each probe in an isolation system holds exactly the maps, stack size
    and role of the PD it stands for in the system it tests, so the negative
    tests never probe stale copies of the UI's, the display's or the input
    broker's neighbours' grants."""

    SYSTEMS = os.path.join(HERE, os.pardir, "systems")
    # (isolation system, the system it tests, {probe: the PD it stands for})
    IN_STEP = [
        ("ui-isolation", "ui-demo",
         {"display_writes_framebuffer": "display", "ui_writes_display_index": "ui",
          "ui_reads_display_private": "ui", "ui_reads_device": "ui"}),
        ("input-isolation", "input-broker-test",
         {"ui_writes_driver_ring": "ui", "ui_writes_broker_ring": "ui"}),
        # The NIC driver's client, holding what the network stack will.
        ("net-isolation", "net-test",
         {"net_writes_driver_dma": "net_client", "net_writes_rx_buffers": "net_client"}),
        # The interactive demo runs the scripted demo's UI PD unchanged.
        ("ui-interactive", "ui-demo", {"ui": "ui"}),
    ]

    def system(self, name):
        root = ET.parse(os.path.join(self.SYSTEMS, f"{name}.system")).getroot()
        return ({pd.get("name"): pd for pd in root.iter("protection_domain")},
                {mr.get("name"): mr.attrib for mr in root.iter("memory_region")})

    def roles(self, name):
        with open(os.path.join(self.SYSTEMS, f"{name}.grants"), encoding="utf-8") as handle:
            words = [line.split("#", 1)[0].split() for line in handle]
        return {w[1]: w[2] for w in words if len(w) == 3 and w[0] == "role"}

    @staticmethod
    def children(pd, tag):
        return sorted(sorted(child.attrib.items()) for child in pd.findall(tag))

    def test_each_probe_holds_what_its_pd_holds_in_the_system_it_tests(self):
        for isolation, tested, probes in self.IN_STEP:
            (demo, demo_mrs), (iso, iso_mrs) = self.system(tested), self.system(isolation)
            demo_roles, iso_roles = self.roles(tested), self.roles(isolation)
            for probe, source in probes.items():
                with self.subTest(isolation=isolation, probe=probe):
                    for tag in ("map", "irq", "setvar"):
                        self.assertEqual(self.children(iso[probe], tag), self.children(demo[source], tag), tag)
                    self.assertEqual(iso[probe].get("stack_size"), demo[source].get("stack_size"))
                    self.assertEqual(iso_roles[probe], demo_roles[source])
                    for m in demo[source].findall("map"):
                        self.assertEqual(iso_mrs[m.get("mr")], demo_mrs[m.get("mr")])


class InteractiveDemoGrants(unittest.TestCase):
    """In the interactive demo only the display PD and the input
    driver hold device capabilities - registers, interrupts or the physical
    addresses of DMA memory - and the UI holds exactly the scripted demo's
    grants (IsolationProbesInStep checks its maps)."""

    def facts(self, name):
        with open(os.path.join(HERE, os.pardir, "systems", f"{name}.grants"), encoding="utf-8") as handle:
            return [line.split("#", 1)[0].split() for line in handle if line.split("#", 1)[0].strip()]

    def test_only_the_display_and_the_driver_hold_devices(self):
        holders = {words[1] for words in self.facts("ui-interactive") if words[0] in ("device", "irq", "paddr")}
        self.assertEqual(holders, {"display", "virtio_input"})

    def test_the_ui_has_the_scripted_demos_grants(self):
        def ui_facts(name):
            return sorted(" ".join(words) for words in self.facts(name)
                          if "ui" in words[1:3] and words[0] in ("map", "role", "channel", "pd"))
        interactive = [fact.replace("input_broker", "input") for fact in ui_facts("ui-interactive")]
        self.assertEqual(interactive, ui_facts("ui-demo"))


class NetworkPath(unittest.TestCase):
    """In the network test only the NIC driver holds the card - its
    registers, interrupt and DMA memory, and the physical addresses of the
    client's buffers - and every data region is the size the driver and its
    client assume (nowayos-net's DATA_REGION_LEN). The driver cannot check
    that size itself: Microkit gives it a region's physical address, never
    its size (a <setvar> has none)."""

    ROOT = os.path.join(HERE, os.pardir)

    def facts(self, name):
        with open(os.path.join(self.ROOT, "systems", f"{name}.grants"), encoding="utf-8") as handle:
            return [line.split("#", 1)[0].split() for line in handle if line.split("#", 1)[0].strip()]

    def data_region_len(self):
        with open(os.path.join(self.ROOT, "crates", "nowayos-net", "src", "lib.rs"), encoding="utf-8") as handle:
            match = re.search(r"pub const DATA_REGION_LEN: usize = (0x[0-9a-fA-F_]+);", handle.read())
        self.assertIsNotNone(match, "nowayos-net states DATA_REGION_LEN as a hexadecimal literal")
        return int(match.group(1).replace("_", ""), 16)

    def test_only_the_driver_holds_the_card(self):
        holders = {words[1] for words in self.facts("net-test") if words[0] in ("device", "irq", "paddr")}
        self.assertEqual(holders, {"virtio_net"})
        roles = {words[1]: words[2] for words in self.facts("net-test") if words[0] == "role"}
        self.assertEqual(roles, {"virtio_net": "driver", "net_client": "network"})

    def test_every_data_region_is_the_size_the_driver_assumes(self):
        expected, checked = self.data_region_len(), []
        for path in sorted(glob.glob(os.path.join(self.ROOT, "systems", "*.system"))):
            root = ET.parse(path).getroot()
            sizes = {mr.get("name"): mr.get("size") for mr in root.iter("memory_region")}
            for pd in root.iter("protection_domain"):
                image = pd.find("program_image").get("path")
                regions = []
                if image == "nowayos-virtio-net.elf":
                    regions = [v.get("region_paddr") for v in pd.findall("setvar")
                               if v.get("symbol", "").startswith("nowayos_io_")]
                elif image in ("nowayos-net-client.elf", "nowayos-net-stack.elf") or (
                        image == "nowayos-net-stack-test.elf" and pd.get("name") == "driver_stand_in"):
                    regions = [m.get("mr") for m in pd.findall("map")
                               if m.get("setvar_vaddr") in ("nowayos_ro_region_0_vaddr", "nowayos_rw_region_4_vaddr")]
                for region in regions:
                    with self.subTest(system=os.path.basename(path), pd=pd.get("name"), region=region):
                        self.assertEqual(int(sizes[region].replace("_", ""), 0), expected)
                        checked.append(region)
        self.assertGreaterEqual(len(checked), 4, "the driver's two I/O regions and the client's two maps")
        self.assertGreaterEqual(len(checked), 16, "and the stack's and the stand-in's in its four systems")

    # The network stack PD, and with a socket client.
    STACK_SYSTEMS = ("net-stack-test", "net-stack-no-seed", "net-stack-bad-frames", "tcp-test")

    def system(self, name):
        return ET.parse(os.path.join(self.ROOT, "systems", f"{name}.system")).getroot()

    def stack_pds(self, name):
        return [pd for pd in self.system(name).iter("protection_domain")
                if pd.find("program_image").get("path") == "nowayos-net-stack.elf"]

    def test_the_stack_holds_nothing_of_the_card_and_maps_frames_read_only(self):
        for name in self.STACK_SYSTEMS:
            with self.subTest(system=name):
                facts = self.facts(name)
                stacks = self.stack_pds(name)
                self.assertEqual([pd.get("name") for pd in stacks], ["net_stack"])
                roles = {words[1]: words[2] for words in facts if words[0] == "role"}
                self.assertEqual(roles["net_stack"], "network")
                self.assertEqual([words for words in facts if words[0] in ("device", "irq", "paddr")
                                  and words[1] == "net_stack"], [])
                self.assertIn(["map", "net_stack", "rx_buffers", "r"], facts)
                self.assertIn(["map", "net_stack", "tx_buffers", "rw"], facts)

    def stack_grants(self, name):
        """The stack's maps, setvars and channels in system `name`, and its
        facts in the grants file."""
        root = self.system(name)
        pd = self.stack_pds(name)[0]
        maps = sorted(sorted(m.attrib.items()) for m in pd.findall("map"))
        ends = sorted(sorted(end.attrib.items()) for channel in root.iter("channel")
                      for end in channel.findall("end") if end.get("pd") == "net_stack")
        facts = sorted(" ".join(words) for words in self.facts(name) if "net_stack" in words[1:3])
        return maps, ends, facts

    def test_against_a_hostile_driver_the_stack_has_its_grants(self):
        maps, ends, facts = self.stack_grants("net-stack-test")
        bad_maps, bad_ends, bad_facts = self.stack_grants("net-stack-bad-frames")
        self.assertEqual(bad_maps, maps)
        self.assertEqual(bad_ends, ends)
        self.assertEqual(sorted(fact.replace("driver_stand_in", "virtio_net") for fact in bad_facts), facts)

    def test_on_the_pi_the_stack_maps_its_packet_buffers_uncached(self):
        """RP1's DMA, across PCIe, does not see the CPU's caches, so
        in gem-net-test the stack maps the buffers the MAC reads and writes
        uncached - cached, frames went out and came in stale."""
        root = ET.parse(os.path.join(self.ROOT, "systems", "rpi5b_2gb", "gem-net-test.system")).getroot()
        stack = [pd for pd in root.iter("protection_domain") if pd.get("name") == "net_stack"][0]
        maps = {m.get("mr"): m.get("cached") for m in stack.findall("map")}
        self.assertEqual((maps["rx_buffers"], maps["tx_buffers"]), ("false", "false"))

    def test_with_a_socket_client_the_stack_gains_its_rings_and_allow_list_alone(self):
        """Tcp-test is net-stack-test with a socket client - the
        stack's grants differ by the client's two rings, its channel and the
        allow list, mapped read-only and prefilled from a tcp-allow name;
        the client holds its ends of the rings and its channel, nothing
        else."""
        maps, ends, facts = self.stack_grants("net-stack-test")
        tcp_maps, tcp_ends, tcp_facts = self.stack_grants("tcp-test")
        client = {"requests", "requests_control", "reports", "reports_control", "tcp_allow"}
        self.assertEqual([m for m in tcp_maps if dict(m)["mr"] not in client], maps)
        self.assertEqual(sorted((dict(m)["mr"], dict(m)["perms"]) for m in tcp_maps if dict(m)["mr"] in client),
                         [("reports", "rw"), ("reports_control", "r"), ("requests", "r"),
                          ("requests_control", "rw"), ("tcp_allow", "r")])
        self.assertEqual([e for e in tcp_ends if dict(e)["id"] != "3"], ends)
        self.assertEqual(sorted(set(tcp_facts) - set(facts)),
                         ["channel net_stack socket_test", "map net_stack reports rw",
                          "map net_stack reports_control r", "map net_stack requests r",
                          "map net_stack requests_control rw", "map net_stack tcp_allow r"])
        self.assertEqual(set(facts) - set(tcp_facts), set())
        regions = {mr.get("name"): mr for mr in self.system("tcp-test").iter("memory_region")}
        self.assertRegex(regions["tcp_allow"].get("prefill_path"), r"^tcp-allow-.*\.bin$")
        roles = {words[1]: words[2] for words in self.facts("tcp-test") if words[0] == "role"}
        self.assertEqual(roles["socket_test"], "test")
        self.assertEqual(sorted(" ".join(words) for words in self.facts("tcp-test") if "socket_test" in words[1:3]
                                and words[0] not in ("role", "pd")),
                         ["channel net_stack socket_test", "map socket_test reports r",
                          "map socket_test reports_control rw", "map socket_test requests rw",
                          "map socket_test requests_control r"])

    def test_with_no_seed_the_system_differs_by_the_seed_alone(self):
        def without_seed(name):
            root = self.system(name)
            pds = {pd.get("name"): sorted(sorted(c.attrib.items()) for c in pd.iter() if c is not pd)
                   for pd in root.iter("protection_domain") if pd.get("name") != "seed_source"}
            for pd in root.iter("protection_domain"):
                if pd.get("name") == "net_stack":
                    pds["net_stack"] = sorted(sorted(c.attrib.items()) for c in pd.iter()
                                              if c is not pd and "seed" not in c.get("mr", ""))
            regions = sorted(sorted(mr.attrib.items()) for mr in root.iter("memory_region")
                             if "seed" not in mr.get("name"))
            channels = sorted(sorted(sorted(end.attrib.items()) for end in channel.findall("end"))
                              for channel in root.iter("channel")
                              if all(end.get("pd") != "seed_source" for end in channel.findall("end")))
            facts = sorted(words for words in self.facts(name) if not any("seed" in word for word in words))
            return pds, regions, channels, facts
        self.assertEqual(without_seed("net-stack-no-seed"), without_seed("net-stack-test"))
        no_seed = ET.tostring(self.system("net-stack-no-seed"), encoding="unicode")
        self.assertNotIn('pd="seed_source"', no_seed)
        self.assertNotIn("nowayos_rw_region_5", no_seed)
        with open(os.path.join(self.ROOT, "systems", "net-stack-test.qemu"), encoding="utf-8") as one, \
                open(os.path.join(self.ROOT, "systems", "net-stack-no-seed.qemu"), encoding="utf-8") as other:
            strip = lambda text: [line for line in text.splitlines() if line and not line.startswith("#")]
            self.assertEqual(strip(other.read()), strip(one.read()))




class SmpTest(unittest.TestCase):
    """Smp-test puts each spinner on a core of its own, above the
    heartbeat, with a full budget, and boots only in an SMP configuration
    with QEMU's four cores; its Pi 5 variant differs only in the timer."""

    SYSTEMS = os.path.join(HERE, os.pardir, "systems")

    def test_each_spinner_has_a_core_of_its_own_above_the_heartbeat(self):
        root = ET.parse(os.path.join(self.SYSTEMS, "smp-test.system")).getroot()
        pds = {pd.get("name"): pd for pd in root.iter("protection_domain")}
        heartbeat = pds["smp_heartbeat"]
        cores = set()
        for name in ("spinner_1", "spinner_2", "spinner_3"):
            spinner = pds[name]
            self.assertGreater(int(spinner.get("priority")), int(heartbeat.get("priority")))
            self.assertIsNone(spinner.get("budget"), "a spinner keeps a full budget")
            self.assertNotEqual(spinner.get("cpu"), heartbeat.get("cpu"))
            cores.add(spinner.get("cpu"))
        self.assertEqual(cores, {"1", "2", "3"})
        with open(os.path.join(self.SYSTEMS, "smp-test.configs"), encoding="utf-8") as handle:
            configs = re.sub(r"#.*", "", handle.read()).split()
        self.assertTrue(configs and all(c.startswith("smp-") for c in configs), configs)
        with open(os.path.join(self.SYSTEMS, "smp-test.qemu"), encoding="utf-8") as handle:
            self.assertIn("-smp 4", handle.read())

    def test_the_pi_variant_differs_only_in_the_timer(self):
        def without_timer(path):
            root = ET.parse(path).getroot()
            for child in list(root):
                if child.tag == "memory_region" or child.get("name") == "timer":
                    root.remove(child)
            return sorted(ET.tostring(child, encoding="unicode").split() for child in root)
        self.assertEqual(without_timer(os.path.join(self.SYSTEMS, "rpi5b_2gb", "smp-test.system")),
                         without_timer(os.path.join(self.SYSTEMS, "smp-test.system")))


class BudgetSystems(unittest.TestCase):
    """Budget-test and budget-test-widened differ by the looper's
    budget alone - in the description and in the grants - so what the
    widened one's heartbeat stopping shows is the budget's doing."""

    ROOT = os.path.join(HERE, os.pardir)

    def read(self, name, suffix):
        with open(os.path.join(self.ROOT, "systems", f"{name}{suffix}"), encoding="utf-8") as handle:
            return handle.read()

    def test_the_two_systems_differ_by_the_loopers_budget_alone(self):
        def shape(name):
            root = ET.fromstring(self.read(name, ".system"))
            return [(element.tag, sorted((key, value) for key, value in element.attrib.items()
                                         if not (element.get("name") == "looper" and key == "budget")))
                    for element in root.iter()]
        self.assertEqual(shape("budget-test-widened"), shape("budget-test"))
        budgets = {name: {pd.get("name"): (pd.get("budget"), pd.get("period"))
                          for pd in ET.fromstring(self.read(name, ".system")).iter("protection_domain")}
                   for name in ("budget-test", "budget-test-widened")}
        self.assertEqual(budgets["budget-test"]["looper"], ("2000", "10000"))
        self.assertEqual(budgets["budget-test-widened"]["looper"], ("10000", "10000"))

        def facts(name):
            return [line for line in self.read(name, ".grants").splitlines()
                    if line and not line.startswith("#")]
        narrow, widened = facts("budget-test"), facts("budget-test-widened")
        self.assertEqual([line for line in widened if not line.startswith("pd looper ")],
                         [line for line in narrow if not line.startswith("pd looper ")])
        self.assertIn("pd looper priority=150 budget=2000 period=10000", narrow)
        self.assertIn("pd looper priority=150 budget=10000 period=10000", widened)


class ClockGrants(unittest.TestCase):
    """Every PD that runs the clock PD's
    program has the role clock and maps the image's build time read-only,
    as nowayos-region's read-only region 6; in clock-test the one PD that
    writes a region the clock reads is tls_stand_in, the test PD in the TLS
    PD's place, and its channel is the only one that may notify the clock."""

    SYSTEMS = os.path.join(HERE, os.pardir, "systems")

    def facts(self, name):
        with open(os.path.join(self.SYSTEMS, f"{name}.grants"), encoding="utf-8") as handle:
            return [line.split("#", 1)[0].split() for line in handle if line.split("#", 1)[0].strip()]

    def test_every_clock_pd_has_the_role_clock_and_the_floor_read_only(self):
        found = 0
        for path in sorted(glob.glob(os.path.join(self.SYSTEMS, "*.system"))):
            name = os.path.basename(path)[:-len(".system")]
            root = ET.parse(path).getroot()
            prefills = {mr.get("name"): mr.get("prefill_path") for mr in root.iter("memory_region")}
            roles = {words[1]: words[2] for words in self.facts(name) if words[0] == "role"}
            for pd in root.iter("protection_domain"):
                if pd.find("program_image").get("path") != "nowayos-clock.elf":
                    continue
                found += 1
                with self.subTest(system=name, pd=pd.get("name")):
                    self.assertEqual(roles[pd.get("name")], "clock")
                    floor = [m for m in pd.findall("map") if prefills.get(m.get("mr")) == "build-time.bin"]
                    self.assertEqual([(m.get("perms"), m.get("setvar_vaddr")) for m in floor],
                                     [("r", "nowayos_ro_region_6_vaddr")])
        self.assertGreaterEqual(found, 1)

    def test_in_clock_test_only_the_stand_in_moves_the_time(self):
        facts = self.facts("clock-test")
        roles = {words[1]: words[2] for words in facts if words[0] == "role"}
        self.assertEqual(roles, {"timer": "driver", "clock": "clock", "tls_stand_in": "test", "reader": "test"})
        read_by_clock = {words[2] for words in facts if words[:2] == ["map", "clock"] and words[3] == "r"}
        writers = {words[1] for words in facts if words[0] == "map" and words[2] in read_by_clock
                   and words[3] == "rw"}
        self.assertEqual(writers, {"tls_stand_in"})
        to_clock = [words for words in facts if words[0] == "channel" and "clock" in words[1:3]]
        notifiers = {words[3] for words in to_clock}
        self.assertEqual(notifiers, {"notify=none", "notify=tls_stand_in"})
        self.assertIn(["channel", "reader", "clock", "notify=none"], to_clock)
        self.assertNotIn("reader", {words[1] for words in facts if words[0] == "map"})

class UsbGrants(unittest.TestCase):
    """Every PD that runs the USB host PD's program - or its test
    programs, nowayos-usb-tone and nowayos-usb-camera - on
    every board,
    maps exactly one controller's window as device 0 - on QEMU also that
    controller's PCI configuration page as device 1 - its own DMA region,
    whose physical address it learns, and its controller's facts read-only;
    no other PD maps any of them. And usb-input-test is usb-test with QEMU's
    keyboard and mouse attached: the same description and grants."""

    SYSTEMS = os.path.join(HERE, os.pardir, "systems")

    @staticmethod
    def facts(path):
        with open(path, encoding="utf-8") as handle:
            return sorted(" ".join(line.split("#", 1)[0].split()) for line in handle
                          if line.split("#", 1)[0].strip())

    def test_each_usb_host_pd_alone_maps_its_controller_and_its_memory(self):
        found = 0
        for path in sorted(glob.glob(os.path.join(self.SYSTEMS, "*.system"))
                           + glob.glob(os.path.join(self.SYSTEMS, "*", "*.system"))):
            root = ET.parse(path).getroot()
            regions = {mr.get("name"): mr for mr in root.iter("memory_region")}
            mapped_by = {}
            for pd in root.iter("protection_domain"):
                for m in pd.findall("map"):
                    mapped_by.setdefault(m.get("mr"), set()).add(pd.get("name"))
            for pd in root.iter("protection_domain"):
                if pd.find("program_image").get("path") not in ("nowayos-usb-host.elf",
                                                                  "nowayos-usb-tone.elf",
                                                                  "nowayos-usb-camera.elf"):
                    continue
                found += 1
                name = pd.get("name")
                bound = {m.get("setvar_vaddr"): m for m in pd.findall("map")}
                with self.subTest(system=os.path.relpath(path, self.SYSTEMS), pd=name):
                    window = regions[bound["nowayos_device_0_vaddr"].get("mr")]
                    self.assertRegex(window.get("phys_addr"), r"^@xhci[0-9]$")
                    controller = window.get("phys_addr")[1:]
                    if "nowayos_device_1_vaddr" in bound:
                        config = regions[bound["nowayos_device_1_vaddr"].get("mr")]
                        self.assertEqual(config.get("phys_addr"), f"@{controller}_config")
                    memory = bound["nowayos_dma_0_vaddr"]
                    self.assertEqual(memory.get("cached"), "false")
                    self.assertEqual([v.get("region_paddr") for v in pd.findall("setvar")
                                      if v.get("symbol") == "nowayos_dma_0_paddr"], [memory.get("mr")])
                    facts = bound["nowayos_ro_region_5_vaddr"]
                    self.assertEqual((regions[facts.get("mr")].get("prefill_path"), facts.get("perms")),
                                     (f"device-{controller}.bin", "r"))
                    for m in pd.findall("map"):
                        if m.get("setvar_vaddr", "").startswith(("nowayos_device_", "nowayos_dma_")):
                            self.assertEqual(mapped_by[m.get("mr")], {name}, m.get("mr"))
        self.assertGreaterEqual(found, 4)

    def test_usb_input_test_is_usb_test_with_devices_attached(self):
        def description(name):
            root = ET.parse(os.path.join(self.SYSTEMS, f"{name}.system")).getroot()
            for element in root.iter():
                element.text = element.tail = None
            return ET.tostring(root, encoding="unicode")
        self.assertEqual(description("usb-input-test"), description("usb-test"))
        self.assertEqual(self.facts(os.path.join(self.SYSTEMS, "usb-input-test.grants")),
                         self.facts(os.path.join(self.SYSTEMS, "usb-test.grants")))

class CheckBaseline(unittest.TestCase):
    """tools/check-baseline, on a fixture workspace and on nowayos itself."""

    def setUp(self):
        self.tool = load("check-baseline")

    def run_on(self, cargo=BASELINE_CARGO, deny=BASELINE_DENY, crate=CRATE, lock=LOCK, extra=None):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "repo/Cargo.toml", cargo)
            write(tmp, "repo/deny.toml", deny)
            write(tmp, "repo/crates/good/Cargo.toml", crate)
            if lock is not None:
                write(tmp, "repo/Cargo.lock", lock)
            for name, text in (extra or {}).items():
                write(tmp, f"repo/{name}", text)
            return run_main(self.tool, ["--root", tmp, "--no-metadata",
                                        os.path.join(tmp, "repo")])

    def assert_drift(self, needle, **files):
        status, out, err = self.run_on(**files)
        self.assertEqual(status, 1, out + err)
        self.assertIn(needle, err)

    def test_a_workspace_that_meets_the_baseline_passes(self):
        status, out, err = self.run_on()
        self.assertEqual(status, 0, err)
        self.assertIn("PASS - repo meets the baseline (1 crate(s)", out)

    def test_each_lint_below_the_baseline_fails(self):
        self.assert_drift("repo: Cargo.toml [workspace.lints] clippy::panic_in_result_fn is "
                          "'warn'; the baseline needs 'deny'",
                          cargo=mutated(BASELINE_CARGO, 'panic_in_result_fn = "deny"',
                                        'panic_in_result_fn = "warn"'))
        self.assert_drift("clippy::indexing_slicing is unset",
                          cargo=mutated(BASELINE_CARGO, 'indexing_slicing = { level = "deny", priority = 0 }\n', ""))
        self.assert_drift("rust::unsafe_code is 'deny'; the baseline needs 'forbid'",
                          cargo=mutated(BASELINE_CARGO, 'unsafe_code = "forbid"', 'unsafe_code = "deny"'))

    def test_profiles_must_match(self):
        self.assert_drift("[profile.release] overflow-checks is false",
                          cargo=mutated(BASELINE_CARGO, "overflow-checks = true", "overflow-checks = false"))
        self.assert_drift("[profile.dev] panic is unset",
                          cargo=mutated(BASELINE_CARGO, '[profile.dev]\npanic = "abort"\n', ""))

    def test_every_member_takes_the_workspace_lints(self):
        self.assert_drift("crate good does not take the workspace lints",
                          crate=mutated(CRATE, "[lints]\nworkspace = true\n", ""))

    def test_a_root_package_is_a_member_too(self):
        root_package = '[package]\nname = "rooted"\nversion = "0.1.0"\n\n' + BASELINE_CARGO
        self.assert_drift("crate rooted does not take the workspace lints", cargo=root_package)
        status, _, err = self.run_on(cargo=root_package.replace(
            '\n\n[workspace]', '\n\n[lints]\nworkspace = true\n\n[workspace]'))
        self.assertEqual(status, 0, err)

    def test_an_own_lint_table_may_relax_only_unsafe_code(self):
        own = CRATE.replace("[lints]\nworkspace = true\n", "")
        own_lints = ('[lints.rust]\nunsafe_code = "deny"\n\n[lints.clippy]\n' +
                     BASELINE_CARGO.split("[workspace.lints.clippy]\n")[1].split("\n\n")[0] + "\n")
        status, _, err = self.run_on(crate=own + own_lints)
        self.assertEqual(status, 0, err)
        self.assert_drift("crate good [lints] rust::unsafe_code is 'allow'",
                          crate=own + own_lints.replace('unsafe_code = "deny"', 'unsafe_code = "allow"'))
        self.assert_drift("crate good [lints] clippy::panic is unset",
                          crate=own + own_lints.replace('panic = "deny"\n', ""))

    def test_each_deny_setting_below_the_baseline_fails(self):
        for old, new, needle in [
            ('yanked = "deny"', 'yanked = "warn"', "[advisories] yanked is 'warn'; the baseline needs 'deny'"),
            ('unmaintained = "all"\n', "", "[advisories] unmaintained is unset"),
            ('unsound = "all"', 'unsound = "workspace"', "[advisories] unsound is 'workspace'"),
            ("confidence-threshold = 0.95", "confidence-threshold = 0.8",
             "confidence-threshold is 0.8; the baseline needs at least 0.93"),
            ("include-dev = true", "include-dev = false", "include-dev is false; the baseline needs true"),
            ('allow = ["MIT", "Apache-2.0"]', 'allow = ["MIT", "MPL-2.0"]',
             "[licenses] allow adds MPL-2.0, outside the house list"),
            ('wildcards = "deny"', 'wildcards = "allow"', "[bans] wildcards is 'allow'"),
            ('unknown-git = "deny"', 'unknown-git = "warn"', "[sources] unknown-git is 'warn'"),
        ]:
            with self.subTest(needle):
                self.assert_drift(needle, deny=mutated(BASELINE_DENY, old, new))

    def test_every_crate_the_baseline_bans_is_banned_in_every_workspace(self):
        self.assertGreaterEqual(len(BASELINE_BANS), 18)
        for name in BASELINE_BANS:
            with self.subTest(name):
                self.assert_drift(f"repo: deny.toml [bans] deny does not ban {name}; the baseline bans it (",
                                  deny=mutated(BASELINE_DENY, ban_line(name), ""))

    def test_a_ban_must_be_the_whole_crate(self):
        for entry, needle in [
            ('    { crate = "wry@<0.50", reason = "x" },\n', "only versions <0.50"),
            ('    { crate = "wry", wrappers = ["tauri"], reason = "x" },\n', "not through its wrappers tauri"),
            ('    { crate = "wry", deny-multiple-versions = true, reason = "x" },\n',
             "only a second version (deny-multiple-versions)"),
            ('    "wry@=0.1.0",\n', "only versions =0.1.0"),
        ]:
            with self.subTest(entry):
                self.assert_drift(f"repo: deny.toml [bans] deny bans wry only in part ({needle}); the "
                                  f"baseline bans the whole crate",
                                  deny=mutated(BASELINE_DENY, ban_line("wry"), entry))

    def test_a_ban_in_another_form_is_not_counted(self):
        """cargo-deny 0.20.2 also reads a `name` key, with a `version` that
        bans only some versions, and a crate given by its source; neither
        counts, so neither can pass for a whole-crate ban."""
        for entry in ('    { name = "wry", version = "<0.50" },\n',
                      '    "https://github.com/rust-lang/crates.io-index#wry",\n'):
            with self.subTest(entry):
                self.assert_drift("repo: deny.toml [bans] deny does not ban wry; the baseline bans it (",
                                  deny=mutated(BASELINE_DENY, ban_line("wry"), entry))

    def test_a_banned_crate_stays_in_the_graph(self):
        """Every deny.toml setting that narrows the graph cargo-deny checks
        fails, whatever it names. With each of these but exclude-unpublished
        (which drops only unpublished crates themselves), cargo-deny 0.20.2
        passed a banned crate from crates.io: an exclude naming it in any
        spelling, or a parent that brings it; dev-dependencies, other
        platforms' crates or default features left out; and the same keys
        outside [graph], where it still reads them."""
        url = "https://github.com/rust-lang/crates.io-index#wry"
        needs = "the baseline needs it unset, false or empty: with it, cargo-deny's bans never see"
        for graph, needle in [
            ('[graph]\nexclude = ["wry"]\n', f"[graph] exclude is ['wry']; {needs} the crates it names"),
            ('[graph]\nexclude = ["serde", "wry@0.37.0"]\n', "[graph] exclude is ['serde', 'wry@0.37.0']"),
            ('[graph]\nexclude = ["wry:0.37.0"]\n', "[graph] exclude is ['wry:0.37.0']"),
            (f'[graph]\nexclude = ["{url}"]\n', f"[graph] exclude is ['{url}']"),
            (f'[graph]\nexclude = ["{url}@0.37.0"]\n', f"[graph] exclude is ['{url}@0.37.0']"),
            ('[graph]\nexclude = ["tauri"]\n', "[graph] exclude is ['tauri']"),
            ('[graph]\nexclude-dev = true\n', f"[graph] exclude-dev is true; {needs} a dev-dependency"),
            ('[graph]\ntargets = ["aarch64-unknown-none"]\n',
             f"[graph] targets is ['aarch64-unknown-none']; {needs} a crate only other platforms use"),
            ('[graph]\ntargets = [{ triple = "aarch64-unknown-none" }]\n',
             "[graph] targets is [{'triple': 'aarch64-unknown-none'}]"),
            ('[graph]\nno-default-features = true\n',
             f"[graph] no-default-features is true; {needs} a crate only a default feature brings in"),
            ('[graph]\nexclude-unpublished = true\n', f"[graph] exclude-unpublished is true; {needs} an "
                                                      f"unpublished crate"),
            ('exclude = ["wry"]\n', "sets exclude outside [graph], where cargo-deny still reads it"),
            ('targets = [{ triple = "aarch64-unknown-none" }]\n', "sets targets outside [graph]"),
            ('exclude-dev = true\n', "sets exclude-dev outside [graph]"),
            ('no-default-features = true\n', "sets no-default-features outside [graph]"),
        ]:
            with self.subTest(graph):
                self.assert_drift(f"repo: deny.toml {needle}", deny=graph + "\n" + BASELINE_DENY)

    def test_settings_that_keep_the_whole_graph_pass(self):
        for graph in ('[graph]\ntargets = []\nall-features = false\nno-default-features = false\n',
                      '[graph]\nexclude = []\nexclude-dev = false\nexclude-unpublished = false\n',
                      '[graph]\nall-features = true\nfeatures = ["extra"]\n'):
            with self.subTest(graph):
                status, _, err = self.run_on(deny=graph + "\n" + BASELINE_DENY)
                self.assertEqual(status, 0, err)

    def test_no_banned_crate_is_in_the_lock_file(self):
        """Cargo.lock lists every crate a --locked build can use, for every
        feature and platform: a banned crate there fails even where
        cargo-deny never sees it, such as an optional dependency behind a
        feature that is not a default one when all-features = false. Matched
        by name in any spelling, wherever the crate comes from."""
        registry = 'source = "registry+https://github.com/rust-lang/crates.io-index"\n'
        for package, needle in [
            (f'[[package]]\nname = "winit"\nversion = "0.30.0"\n{registry}',
             "repo: Cargo.lock holds winit 0.30.0; the baseline bans it (trusted-path non-negotiable: "
             "no window system)"),
            ('[[package]]\nname = "gtk-sys"\nversion = "0.18.0"\n', "repo: Cargo.lock holds gtk-sys 0.18.0"),
            ('[[package]]\nname = "Gtk_Sys"\nversion = "0.18.0"\n', "repo: Cargo.lock holds Gtk_Sys 0.18.0"),
        ]:
            with self.subTest(needle):
                self.assert_drift(needle, lock=LOCK + "\n" + package)
        self.assert_drift("repo: has no Cargo.lock; every workspace commits one", lock=None)

    def test_a_workspace_may_ban_more_and_write_a_ban_as_a_name(self):
        deny = mutated(BASELINE_DENY, ban_line("wry"), '    "wry",\n    { crate = "raptorq", reason = "its own" },\n')
        status, _, err = self.run_on(deny=deny)
        self.assertEqual(status, 0, err)

    def test_the_baseline_bans_whole_crates_with_their_reasons(self):
        with tempfile.TemporaryDirectory() as tmp:
            with open(os.path.join(HERE, "baseline.toml"), encoding="utf-8") as handle:
                baseline = handle.read()
            for old, new in [('{ crate = "wry", reason = "trusted-path non-negotiable: no WebView" }', '"wry"'),
                             ('{ crate = "wry", reason', '{ crate = "wry@<1", reason')]:
                with self.subTest(new):
                    path = write(tmp, "baseline.toml", mutated(baseline, old, new))
                    write(tmp, "repo/Cargo.toml", BASELINE_CARGO)
                    write(tmp, "repo/deny.toml", BASELINE_DENY)
                    write(tmp, "repo/crates/good/Cargo.toml", CRATE)
                    status, _, err = run_main(self.tool, ["--baseline", path, "--root", tmp, "--no-metadata",
                                                          os.path.join(tmp, "repo")])
                    self.assertEqual(status, 2, err)
                    self.assertIn("each naming a whole crate (crate = NAME) and its reason", err)

    def test_the_local_crate_guard(self):
        with tempfile.TemporaryDirectory() as root:
            ws = os.path.join(root, "showcase")
            write(root, "showcase/Cargo.toml", '[workspace]\nmembers = ["kiosk/*"]\n')

            def package(name, manifest, source=None, publish=(), licence=None):
                return {"name": name, "version": "0.1.0", "id": f"{name}-id",
                        "source": source, "publish": None if publish is None else list(publish),
                        "license": licence, "manifest_path": manifest}

            write(root, "nowayos/pds/Cargo.toml", '[workspace]\nmembers = ["timer"]\n')
            write(root, "nowayos/pds/timer/Cargo.toml", '[package]\nname = "nowayos-timer"\n')
            write(root, "nowayos/pds/support/Cargo.toml", '[package]\nname = "not-a-member"\n')
            ours = package("flow", os.path.join(ws, "kiosk", "flow", "Cargo.toml"))
            nowayos = package("nowayos-ipc", os.path.join(root, "nowayos", "crates", "nowayos-ipc", "Cargo.toml"))
            nowayos_pd = package("nowayos-timer", os.path.join(root, "nowayos", "pds", "timer", "Cargo.toml"))
            registry = package("serde", "/cache/serde/Cargo.toml",
                               source="registry+https://github.com/rust-lang/crates.io-index", publish=None)

            def problems(*extra, members=("flow-id",)):
                metadata = {"workspace_root": ws, "workspace_members": list(members),
                            "packages": [ours, nowayos, nowayos_pd, registry, *extra]}
                return self.tool.local_crate_problems(metadata, root, house=("MIT", "Apache-2.0", "ISC"))

            self.assertEqual(problems(), [])
            # The reviewed allowance: NoWayOS's glyph atlases, OFL-1.1, where they are.
            glyphs = package("nowayos-glyphs", os.path.join(root, "nowayos", "crates", "nowayos-glyphs", "Cargo.toml"),
                             licence="OFL-1.1")
            self.assertEqual(problems(glyphs), [])
            # House-list licences: NoWayOS's own, and an open product's member.
            licensed_nowayos = package("nowayos-ui", os.path.join(root, "nowayos", "crates", "nowayos-ui", "Cargo.toml"),
                                       licence="MIT OR Apache-2.0")
            licensed_ours = package("kit", os.path.join(ws, "kiosk", "kit", "Cargo.toml"), licence="(MIT OR Apache-2.0) AND ISC")
            self.assertEqual(problems(licensed_nowayos, licensed_ours, members=("flow-id", "kit-id")), [])
            for extra, members, needle in [
                (package("nowayos-glyphs", os.path.join(root, "nowayos", "crates", "nowayos-glyphs", "Cargo.toml"),
                         licence="GPL-3.0"), ("flow-id",), "nowayos-glyphs 0.1.0: has a licence field outside the house list"),
                (package("nowayos-ui", os.path.join(root, "nowayos", "crates", "nowayos-ui", "Cargo.toml"),
                         licence="MIT OR GPL-2.0"), ("flow-id",), "nowayos-ui 0.1.0: has a licence field outside"),
                (package("excepted", os.path.join(ws, "kiosk", "excepted", "Cargo.toml"),
                         licence="MIT WITH LLVM-exception"), ("flow-id", "excepted-id"), "excepted 0.1.0: has a licence field outside"),
                (package("nowayos-glyphs", os.path.join(ws, "kiosk", "nowayos-glyphs", "Cargo.toml"),
                         licence="OFL-1.1"), ("flow-id", "nowayos-glyphs-id"), "nowayos-glyphs 0.1.0: has a licence field"),
                (package("vendored", os.path.join(ws, "third_party", "x", "Cargo.toml")),
                 ("flow-id", "vendored-id"), "is a workspace member only because it is a path dependency"),
                (package("nested", os.path.join(ws, "kiosk", "flow", "vendor", "Cargo.toml")),
                 ("flow-id", "nested-id"), "is a workspace member only because it is a path dependency"),
                (package("elsewhere", "/somewhere/else/Cargo.toml"), ("flow-id",),
                 "is neither a member of this workspace nor a NoWayOS crate"),
                (package("not-a-member", os.path.join(root, "nowayos", "pds", "support", "Cargo.toml")),
                 ("flow-id",), "is neither a member of this workspace nor a NoWayOS crate"),
                (package("sneaky", "/cache/sneaky/Cargo.toml", source="git+https://example.invalid/sneaky"),
                 ("flow-id",), "from git+https://example.invalid/sneaky is marked publish = false"),
                (package("public", os.path.join(ws, "kiosk", "public", "Cargo.toml"), publish=None),
                 ("flow-id", "public-id"), "public 0.1.0: publish = false is missing"),
                (package("licensed", os.path.join(ws, "kiosk", "licensed", "Cargo.toml"), licence="GPL-3.0"),
                 ("flow-id", "licensed-id"), "licensed 0.1.0: has a licence field"),
            ]:
                with self.subTest(needle):
                    found = problems(extra, members=members)
                    self.assertEqual(len(found), 1, found)
                    self.assertIn(needle, found[0])

    def test_unreadable_files_are_setup_errors(self):
        with tempfile.TemporaryDirectory() as tmp:
            status, _, err = run_main(self.tool, ["--root", tmp, "--no-metadata", tmp])
        self.assertEqual(status, 2)
        self.assertIn("cannot read", err)

    def test_nowayos_meets_the_baseline(self):
        nowayos = os.path.join(HERE, os.pardir)
        status, out, err = run_main(self.tool, ["--no-metadata", nowayos, os.path.join(nowayos, "pds"),
                                                os.path.join(nowayos, "fuzz"),
                                                os.path.join(nowayos, "tools", "glyph-atlas")])
        self.assertEqual(status, 0, err)
        self.assertIn("nowayos, nowayos/pds, nowayos/fuzz, nowayos/tools/glyph-atlas meet the baseline", out)


class CheckUnsafe(unittest.TestCase):
    tool = load("check-unsafe")

    def uses(self, source):
        with tempfile.TemporaryDirectory() as tmp:
            return self.tool.unsafe_uses(write(tmp, "x.rs", source))

    def test_comments_strings_and_characters_do_not_count(self):
        source = '''\
            //! Mentions `unsafe` in the docs.
            /* unsafe /* nested unsafe */ still a comment: unsafe */
            const A: &str = "unsafe { \\" unsafe }";
            const B: &str = r#"unsafe "quoted" unsafe"#;
            const C: &[u8] = b"unsafe";
            const D: char = '"';
            const E: char = '\\'';
            fn f<'a>(x: &'a str) -> &'a str { x } // unsafe
            #![forbid(unsafe_code)]
            #[allow(clippy::undocumented_unsafe_blocks, clippy::multiple_unsafe_ops_per_block)]
            fn g() {}
            '''
        self.assertEqual(self.uses(source), [])

    def test_unsafe_code_and_relaxed_lints_count(self):
        source = '''\
            fn f() { let _ = unsafe { core::ptr::read_volatile(0 as *const u8) }; }
            unsafe fn g() {}
            #[unsafe(no_mangle)]
            extern "C" fn h() {}
            #[allow(unsafe_code)]
            mod m {}
            #![cfg_attr(test, expect(dead_code, unsafe_code))]
            '''
        lines = [line for line, _ in self.uses(source)]
        self.assertEqual(lines, [1, 2, 3, 5, 7])

    def test_the_repository_passes(self):
        status, out, err = run_main(self.tool, [])
        self.assertEqual(status, 0, err)
        self.assertIn("nowayos-region", out)

    def test_a_getrandom_back_end_outside_nowayos_entropy_is_refused(self):
        # The macro, or either symbol in code or a
        # string, outside nowayos-entropy's crate; a comment does not count.
        entropy = {"name": "nowayos-entropy", "workspace": "pds",
                   "files": ["pds/entropy/src/backend.rs"], "miri": True, "why": "x"}
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "pds/entropy/src/backend.rs", "getrandom02::register_custom_getrandom!(fill);\n")
            write(tmp, "pds/stray/src/lib.rs", "//! names __getrandom_custom in its docs only\n"
                  "getrandom::register_custom_getrandom!(zeros);\n")
            write(tmp, "crates/sly/src/lib.rs", '#[export_name = "__getrandom_v03_custom"]\nfn f() {}\n')
            problems = self.tool.source_problems(tmp, [entropy])
        backends = [p for p in problems if "getrandom back end" in p]
        self.assertEqual(len(backends), 2, problems)
        self.assertTrue(any(p.startswith("pds/stray/src/lib.rs:2:") for p in backends), backends)
        self.assertTrue(any(p.startswith("crates/sly/src/lib.rs:1:") for p in backends), backends)
        # With no nowayos-entropy listed (a product repo), even its own file is refused.
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "pds/entropy/src/backend.rs", "getrandom02::register_custom_getrandom!(fill);\n")
            self.assertTrue(any("getrandom back end" in p for p in self.tool.source_problems(tmp, [])))

    def test_miri_crates_come_from_the_list(self):
        status, out, _ = run_main(self.tool, ["--miri-crates", "pds"])
        self.assertEqual((status, out.split()), (0, ["nowayos-region", "nowayos-hw", "nowayos-entropy", "nowayos-heap", "nowayos-restart"]))


def elf_object(symbol, defined=True):
    """A minimal ELF64 little-endian object whose symbol table holds one
    global function `symbol`, defined (in section 1) or only referenced."""
    strtab = b"\0" + symbol + b"\0"
    symtab = bytes(24) + struct.pack("<IBBHQQ", 1, (1 << 4) | 2, 0, 1 if defined else 0, 0, 0)
    header = bytearray(64)
    header[:7] = b"\x7fELF\x02\x01\x01"
    strtab_at = 64
    symtab_at = strtab_at + len(strtab)
    shoff = symtab_at + len(symtab)
    struct.pack_into("<Q", header, 0x28, shoff)
    struct.pack_into("<HH", header, 0x3A, 64, 3)
    sections = bytes(64)
    sections += struct.pack("<IIQQQQIIQQ", 0, 3, 0, 0, strtab_at, len(strtab), 0, 0, 1, 0)
    sections += struct.pack("<IIQQQQIIQQ", 0, 2, 0, 0, symtab_at, len(symtab), 1, 1, 8, 24)
    return bytes(header) + strtab + symtab + sections


def rlib(*objects):
    """An ar archive (an rlib) of `objects`."""
    data = b"!<arch>\n"
    for number, body in enumerate(objects):
        name = f"o{number}.o/".encode().ljust(16)
        data += name + b"0".ljust(12) + b"0".ljust(6) + b"0".ljust(6) + b"644".ljust(8)
        data += str(len(body)).encode().ljust(10) + b"`\n" + body + (b"\n" if len(body) % 2 else b"")
    return data


class CheckBackends(unittest.TestCase):
    """Getrandom's back end only from nowayos-entropy."""
    tool = load("check-backends")

    def strays(self, libraries):
        with tempfile.TemporaryDirectory() as tmp:
            for file, objects in libraries.items():
                with open(os.path.join(tmp, file), "wb") as handle:
                    handle.write(rlib(*objects))
            return self.tool.strays(tmp)

    def test_nowayos_entropys_definitions_pass(self):
        self.assertEqual(self.strays({
            "libnowayos_entropy-0123abcd.rlib": [elf_object(b"__getrandom_custom"), elf_object(b"__getrandom_v03_custom")],
            "libgetrandom-4567cdef.rlib": [elf_object(b"__getrandom_v03_custom", defined=False)],
        }), [])

    def test_a_stray_back_end_fails_with_or_without_nowayos_entropy(self):
        # The spike's stray crate: a getrandom 0.2 back end of its own, in a
        # PD that links none of nowayos-entropy - and in one that does.
        stray = {"libstray-89abcdef.rlib": [elf_object(b"other"), elf_object(b"__getrandom_custom")]}
        self.assertEqual(self.strays(stray), [("libstray-89abcdef.rlib", "__getrandom_custom")])
        with_entropy = {**stray, "libnowayos_entropy-0123abcd.rlib": [elf_object(b"__getrandom_custom")]}
        self.assertEqual(self.strays(with_entropy), [("libstray-89abcdef.rlib", "__getrandom_custom")])


class CheckStdCrates(unittest.TestCase):
    tool = load("check-std-crates")

    def check(self, rlibs, listed, licence="MIT OR Apache-2.0", lib_manifest=True):
        with tempfile.TemporaryDirectory() as tmp:
            deps = os.path.join(tmp, "deps")
            os.makedirs(deps)
            for name in rlibs:
                write(deps, f"lib{name}-0123abcd.rlib", "")
            write(deps, "libsel4-89ab.rmeta", "")
            lock = write(tmp, "Cargo.lock", '[[package]]\nname = "sel4-microkit"\nversion = "0.1.0"\n'
                         '[[package]]\nname = "rustls-webpki"\nversion = "0.103.15"\n'
                         'source = "registry+https://github.com/rust-lang/crates.io-index"\n')
            cargo_home = os.path.join(tmp, "cargo")
            if lib_manifest:
                write(cargo_home, "registry/src/index.crates.io-0123/rustls-webpki-0.103.15/Cargo.toml",
                      '[package]\nname = "rustls-webpki"\n[lib]\nname = "webpki"\n')
            library = os.path.join(tmp, "library")
            write(library, "Cargo.toml", "[workspace]\n")
            write(library, "core/Cargo.toml", f'[package]\nname = "core"\nlicense = "{licence}"\n')
            write(library, "alloc/Cargo.toml", '[package]\nname = "alloc"\nlicense = "MIT OR Apache-2.0"\n')
            write(library, "compiler-builtins/compiler-builtins/Cargo.toml",
                  '[package]\nname = "compiler_builtins"\nlicense = "MIT AND (MIT OR Apache-2.0)"\n')
            listing = write(tmp, "std-crates.txt", listed)
            return run_main(self.tool, ["--deps", deps, "--lock", lock, "--list", listing, "--library", library,
                                        "--cargo-home", cargo_home])

    LIST = """\
        # comment
        core               MIT OR Apache-2.0
        alloc              MIT OR Apache-2.0
        compiler_builtins  MIT AND (MIT OR Apache-2.0)   # flagged
        """
    BUILT = ["core", "alloc", "compiler_builtins", "sel4_microkit"]

    def test_the_reviewed_set_passes(self):
        status, out, err = self.check(self.BUILT, self.LIST)
        self.assertEqual(status, 0, err)
        self.assertIn("compiler_builtins (MIT AND (MIT OR Apache-2.0))", out)

    def test_a_new_standard_crate_fails(self):
        status, _, err = self.check(self.BUILT + ["rustc_std_workspace_core"], self.LIST)
        self.assertEqual(status, 1)
        self.assertIn("rustc_std_workspace_core: built into the PDs but not on", err)

    def test_a_changed_licence_fails(self):
        status, _, err = self.check(self.BUILT, self.LIST, licence="MIT OR Apache-2.0 OR Zlib")
        self.assertEqual(status, 1)
        self.assertIn("core: rust-src declares", err)

    def test_a_crate_no_longer_built_fails(self):
        status, _, err = self.check(["core", "alloc", "sel4_microkit"], self.LIST)
        self.assertEqual(status, 1)
        self.assertIn("compiler_builtins: on", err)

    def test_a_locked_package_built_under_its_lib_name_is_accounted_for(self):
        # rustls-webpki's library is `webpki`: its manifest says so.
        status, _, err = self.check(self.BUILT + ["webpki"], self.LIST)
        self.assertEqual(status, 0, err)
        status, _, err = self.check(self.BUILT + ["webpki"], self.LIST, lib_manifest=False)
        self.assertEqual(status, 1)
        self.assertIn("webpki: built into the PDs but not on", err)


class CheckCratesAbsent(unittest.TestCase):
    """tools/check-crates-absent: crates that must never be in a
    workspace's cargo tree, with a shell script standing in for cargo tree.
    A tree that cannot be read checks nothing, so it fails too."""

    TREE = textwrap.dedent("""\
        nowayos-ui v0.1.0 (/work/nowayos/crates/nowayos-ui)
        embedded-graphics v0.8.1
        serde_derive v1.0.229 (proc-macro)

        proptest v1.11.0
        embedded-graphics v0.8.1 (*)
        """)
    REASON = "the rasteriser belongs to the atlas tool alone"

    def check(self, script):
        result = subprocess.run(
            [sys.executable, os.path.join(HERE, "check-crates-absent"), "--what", "the cargo tree of pds",
             "--forbid", "swash", "--forbid", "skrifa", "--reason", self.REASON, "--", "sh", "-c", script],
            capture_output=True, text=True, timeout=60)
        return result.returncode, result.stdout, result.stderr

    def tree(self, text):
        """A script that prints `text` as cargo tree would, and succeeds."""
        directory = self.enterContext(tempfile.TemporaryDirectory())
        return f"cat '{write(directory, 'tree.txt', text)}'"

    def test_a_tree_without_them_passes_counting_its_crates(self):
        status, out, err = self.check(self.tree(self.TREE))
        self.assertEqual(status, 0, err)
        self.assertIn("the cargo tree of pds: 4 crates, none of swash skrifa", out)

    def test_a_forbidden_crate_fails_naming_it_and_why(self):
        status, _, err = self.check(self.tree(self.TREE + "skrifa v0.40.0\nswash v0.2.10 (*)\n"))
        self.assertEqual(status, 1, err)
        self.assertIn(f"FAIL - skrifa is in the cargo tree of pds: {self.REASON}", err)
        self.assertIn(f"FAIL - swash is in the cargo tree of pds: {self.REASON}", err)

    def test_a_cargo_tree_that_fails_fails(self):
        # Regression: with a toolchain that is not
        # installed, cargo tree printed an error and nothing else, and the
        # check passed - having looked at no crate.
        status, out, err = self.check("echo \"error: toolchain 'no-such-toolchain' is not installed\" >&2; exit 1")
        self.assertEqual(status, 2, out + err)
        self.assertIn("error: toolchain 'no-such-toolchain' is not installed", err)
        self.assertIn("FAIL - the cargo tree of pds was not checked: `sh -c", err)
        self.assertIn("failed with exit status 1", err)
        self.assertNotIn("none of", out)

    def test_a_tree_listing_nothing_fails(self):
        for script in ("true", self.tree("\n\n")):
            status, out, err = self.check(script)
            self.assertEqual(status, 2, out + err)
            self.assertIn("FAIL - the cargo tree of pds was not checked: cargo tree listed no crate", err)

    def test_a_line_that_is_not_a_crate_fails(self):
        status, _, err = self.check(self.tree(self.TREE + "warning: something else\n"))
        self.assertEqual(status, 2, err)
        self.assertIn("line 7 of cargo tree's output is not a crate and its version: 'warning: something else'", err)

    def test_no_command_is_a_usage_error(self):
        result = subprocess.run(
            [sys.executable, os.path.join(HERE, "check-crates-absent"), "--what", "x", "--forbid", "swash"],
            capture_output=True, text=True, timeout=60)
        self.assertEqual(result.returncode, 2)
        self.assertIn("give the cargo tree command after --", result.stderr)


class BoardSystem(unittest.TestCase):
    """tools/board-system: one description source for every board;
    and check-system's `irq PD DEVICE`, which takes the INTID from the board."""

    tool = load("board-system")
    check_system = load("check-system")
    PLATFORMS = os.path.join(HERE, os.pardir, "platforms")
    SYSTEMS = os.path.join(HERE, os.pardir, "systems")
    SOURCE = textwrap.dedent("""\
        <?xml version="1.0" encoding="UTF-8"?>
        <!-- a comment may say phys_addr="0x900_0000" or irq="33": never read -->
        <system>
            <memory_region name="uart_regs" size="@uart" phys_addr="@uart" />
            <memory_region name="buffer" size="0x1000" />
            <protection_domain name="serial" priority="2" stack_size="0x1000">
                <program_image path="serial.elf" />
                <map mr="uart_regs" vaddr="0x200_0000" perms="rw" cached="false" setvar_size="uart_size" />
                <irq irq="@uart" id="1" trigger="@uart" />
                <irq irq="@net[1]" id="2" trigger="@net" />
            </protection_domain>
        </system>
        """)

    def devices(self, board):
        return self.check_system.read_board(os.path.join(self.PLATFORMS, f"{board}.toml"), board)

    def resolve(self, text, board_text=BOARD):
        with tempfile.TemporaryDirectory() as tmp:
            path = write(tmp, "testboard.toml", board_text)
            devices = self.check_system.read_board(path, "testboard")
        return self.tool.resolve(text, devices, "testboard")

    def assert_refused(self, text, *reasons):
        _, problems = self.resolve(text)
        self.assertTrue(problems, "expected a refusal")
        for reason in reasons:
            self.assertTrue(any(reason in problem for problem in problems), (reason, problems))

    def test_each_placeholder_takes_the_board_fact(self):
        resolved, problems = self.resolve(self.SOURCE)
        self.assertEqual(problems, [])
        self.assertIn('<memory_region name="uart_regs" size="0x1000" phys_addr="0x9000000" />', resolved)
        self.assertIn('<memory_region name="buffer" size="0x1000" />', resolved)
        self.assertIn('setvar_size="uart_size"', resolved)
        self.assertIn('<irq irq="33" id="1" trigger="level" />', resolved)
        self.assertIn('<irq irq="79" id="2" trigger="edge" />', resolved)
        self.assertIn('may say phys_addr="0x900_0000" or irq="33"', resolved)

    def test_what_cannot_be_filled_in_is_refused_with_its_line(self):
        cases = [
            ('<memory_region name="r" size="0x1000" phys_addr="0x900_0000" />', "name the device instead"),
            ('<irq irq="33" id="1" />', 'irq="33": name the device instead'),
            ('<memory_region name="r" size="@gpu" phys_addr="@gpu" />', "has no device 'gpu'"),
            ('<irq irq="@net" id="1" />', "has 2 interrupts", "@net[N]"),
            ('<irq irq="@net[2]" id="1" />', "no index 2"),
            ('<memory_region name="r" size="@uart" phys_addr="@uart[0]" />', "only irq= takes an index"),
            ('<memory_region name="r" size="@" phys_addr="@uart" />', "write @DEVICE"),
        ]
        for line, *reasons in cases:
            with self.subTest(line=line):
                self.assert_refused(f"<system>\n    {line}\n</system>\n", "line 2: ", *reasons)

    def test_a_device_without_a_window_cannot_be_mapped(self):
        board = BOARD + textwrap.dedent("""\

            [[device]]
            name = "timer"
            irqs = [30]
            trigger = "level"
            source = "test fixture"
            """)
        _, problems = self.resolve('<memory_region name="t" size="@timer" phys_addr="@timer" />', board)
        self.assertTrue(problems and all("no register window" in problem for problem in problems), problems)
        resolved, problems = self.resolve('<irq irq="@timer" id="0" trigger="@timer" />', board)
        self.assertEqual((resolved, problems), ('<irq irq="30" id="0" trigger="level" />', []))

    def test_the_pi_board_file_is_valid(self):
        uart = self.devices("rpi5b_2gb")["debug_uart"]
        self.assertEqual((uart.address, uart.size, uart.irqs, uart.trigger),
                         (0x10_7D00_1000, 0x1000, [153], "level"))
        # BCM2712's PL011 has 0x200 bytes of registers - its
        # identification registers lie outside them - and says so.
        self.assertEqual((uart.compatible, uart.registers), ("arm,pl011-axi", 0x200))
        timer = self.devices("rpi5b_2gb")["system_timer"]
        self.assertEqual((timer.address, timer.size, timer.irqs, timer.trigger),
                         (0x10_7C00_3000, 0x1000, [96, 97, 98, 99], "level"))
        # RP1's USB controllers' first pages, behind PCIe as U-Boot
        # leaves it, seeing DRAM at physical + 0x10_0000_0000; polled.
        for name, address in (("xhci0", 0x1F_0060_0000), ("xhci1", 0x1F_0070_0000)):
            xhci = self.devices("rpi5b_2gb")[name]
            self.assertEqual((xhci.address, xhci.size, xhci.irqs, xhci.dma_offset, xhci.pci_bar0),
                             (address, 0x1000, [], 0x10_0000_0000, None))

    @staticmethod
    def without_devices(path):
        """A system description with its devices taken out - the memory
        regions with a phys_addr, those prefilled with a device's facts
        (device-NAME.bin), their maps and every <irq> - as text, so a board's
        own variant can be compared with the system it varies."""
        root = ET.parse(path).getroot()
        windows = {mr.get("name") for mr in root.findall("memory_region")
                   if mr.get("phys_addr") or re.fullmatch(r"device-\w+\.bin", mr.get("prefill_path") or "")}
        for mr in [mr for mr in root.findall("memory_region") if mr.get("name") in windows]:
            root.remove(mr)
        for pd in root.iter("protection_domain"):
            for child in [c for c in pd if c.tag == "irq" or (c.tag == "map" and c.get("mr") in windows)]:
                pd.remove(child)
        for element in root.iter():
            element.text = element.tail = None
        return ET.tostring(root, encoding="unicode")

    @staticmethod
    def grant_facts(path):
        with open(path, encoding="utf-8") as handle:
            return sorted(" ".join(line.split("#", 1)[0].split()) for line in handle
                          if line.split("#", 1)[0].strip())

    # Systems only one board can run, with no system in systems/ to vary
    # (QEMU has no firmware framebuffer). Each is still built and
    # checked like a variant, with its own grants and an expect file in
    # systems/.
    BOARD_ONLY = {("rpi5b_2gb", "fb-test"), ("rpi5b_2gb", "gem-net-test"), ("rpi5b_2gb", "pm-watchdog-test"),
                  ("rpi5b_2gb", "run-length-test"), ("rpi5b_2gb", "fan-test"),
                  ("rpi5b_2gb", "usb-stream-test"), ("rpi5b_2gb", "usb-audio-loop-pi-test"),
                  ("rpi5b_2gb", "usb-camera-test")}

    def test_a_boards_own_variant_differs_from_its_system_only_in_devices(self):
        """systems/BOARD/NAME.system: built for BOARD instead of
        NAME.system, and checked against its own NAME.grants, by the same
        rules; it keeps NAME's protection domains and channels and differs
        only in the devices (mapped cached or not), their facts and the
        interrupts it grants. Its
        log is checked against NAME.expect, or against BOARD/NAME.expect,
        which holds every line of NAME.expect (checked below). A
        board's own system (BOARD_ONLY) has no NAME.system to vary."""
        variants = sorted(glob.glob(os.path.join(self.SYSTEMS, "*", "*.system")))
        self.assertIn(os.path.join(self.SYSTEMS, "rpi5b_2gb", "timer-demo.system"), variants)
        with tempfile.TemporaryDirectory() as tmp:
            for path in variants:
                board, name = os.path.basename(os.path.dirname(path)), os.path.basename(path)[:-len(".system")]
                source = os.path.join(self.SYSTEMS, f"{name}.system")
                grants = os.path.join(self.SYSTEMS, board, f"{name}.grants")
                with self.subTest(board=board, system=name):
                    self.assertTrue(os.path.isfile(os.path.join(self.PLATFORMS, f"{board}.toml")), board)
                    self.assertEqual(os.path.isfile(source), (board, name) not in self.BOARD_ONLY)
                    self.assertTrue(os.path.isfile(grants))
                    self.assertTrue(os.path.isfile(os.path.join(self.SYSTEMS, f"{name}.expect")))
                    with open(path, encoding="utf-8") as handle:
                        resolved, problems = self.tool.resolve(handle.read(), self.devices(board), board)
                    self.assertEqual(problems, [])
                    self.assertNotIn('="@', resolved)
                    built = write(tmp, f"{board}-{name}.system", resolved)
                    status, _, err = run_main(self.check_system, ["--sdf", built, "--grants", grants,
                                                                  "--board", board])
                    self.assertEqual(status, 0, err)
                    if (board, name) in self.BOARD_ONLY:
                        continue
                    self.assertEqual(self.without_devices(path), self.without_devices(source))
                    ours, theirs = self.grant_facts(grants), self.grant_facts(
                        os.path.join(self.SYSTEMS, f"{name}.grants"))
                    facts = {mr.get("name") for mr in ET.parse(path).getroot().findall("memory_region")
                             if re.fullmatch(r"device-\w+\.bin", mr.get("prefill_path") or "")}
                    def other(fact):
                        words = fact.split()
                        return words[0] not in ("device", "cached", "irq") and not (words[0] == "map" and words[2] in facts)
                    self.assertEqual([f for f in ours if other(f)], [f for f in theirs if other(f)])

    def test_the_debug_uarts_keys_hold_only_the_uart(self):
        """uart-input-test, in QEMU and on the Pi 5: uart_input holds
        the debug UART - its window and its facts - and no interrupt (it
        polls, on the timer); only the timer holds an interrupt; the broker
        and the UI's stand-in hold no device, interrupt or physical address.
        The build names uart_input's program development-only."""
        with tempfile.TemporaryDirectory() as tmp:
            for board, folder in (("qemu_virt_aarch64", self.SYSTEMS),
                                  ("rpi5b_2gb", os.path.join(self.SYSTEMS, "rpi5b_2gb"))):
                with self.subTest(board=board):
                    grants = os.path.join(folder, "uart-input-test.grants")
                    with open(os.path.join(folder, "uart-input-test.system"), encoding="utf-8") as handle:
                        resolved, problems = self.tool.resolve(handle.read(), self.devices(board), board)
                    self.assertEqual(problems, [])
                    built = write(tmp, f"{board}.system", resolved)
                    status, out, err = run_main(self.check_system, ["--sdf", built, "--grants", grants,
                                                                    "--board", board])
                    self.assertEqual(status, 0, err)
                    self.assertIn("check-system: development only - 'uart_input' runs nowayos-uart-input.elf", out)
                    facts = self.grant_facts(grants)
                    held = {(fact.split()[0], fact.split()[1]) for fact in facts
                            if fact.split()[0] in ("device", "irq", "paddr")}
                    self.assertIn(("device", "uart_input"), held)
                    self.assertEqual({pd for kind, pd in held if kind == "irq"}, {"timer"})
                    self.assertEqual({pd for _, pd in held}, {"uart_input", "timer"})
                    self.assertIn("map uart_input debug_uart_facts r", facts)
                    self.assertIn("call uart_input timer", facts)

    def test_a_boards_own_expect_file_holds_every_line_of_the_systems(self):
        """systems/BOARD/NAME.expect: what only that board prints
        added to NAME.expect - which it holds whole, its expected lines in
        their order and every forbidden text - so a board's run is checked
        no less strictly than QEMU's."""
        # A board's folder is named for its board file; systems/update-trial/
        # holds tools/update-trial's runs' expect files instead.
        boards = {name[:-len(".toml")] for name in os.listdir(os.path.join(HERE, os.pardir, "platforms"))
                  if name.endswith(".toml")}
        found = sorted(path for path in glob.glob(os.path.join(self.SYSTEMS, "*", "*.expect"))
                       if os.path.basename(os.path.dirname(path)) in boards)
        self.assertIn(os.path.join(self.SYSTEMS, "rpi5b_2gb", "ui-demo.expect"), found)
        for path in found:
            name = os.path.basename(path)[:-len(".expect")]
            with self.subTest(expect=os.path.relpath(path, self.SYSTEMS)):
                self.assertTrue(os.path.isfile(os.path.join(os.path.dirname(path), f"{name}.system")),
                                "a board's expect file goes with its variant")
                serial_expect = load("serial_expect.py")
                ours = serial_expect.read_expectations(path)
                theirs = serial_expect.read_expectations(os.path.join(self.SYSTEMS, f"{name}.expect"))
                lines = iter(ours[0])
                self.assertTrue(all(line in lines for line in theirs[0]),
                                f"every line of {name}.expect, in order")
                self.assertTrue(set(theirs[1]) <= set(ours[1]), "every forbidden text")
                self.assertGreater(len(ours[0]), len(theirs[0]), "lines of the board's own")

    def test_the_timer_pd_is_given_one_of_the_two_timers_it_drives(self):
        """The timer PD takes the system timer's channel 1 if its
        device 0 is bound, and the CPU's timer if nothing is: every
        description that runs it grants exactly one of those - the system
        timer's page as device 0 with the second of its interrupts, or the
        CPU timer's interrupt and no device."""
        found = 0
        for path in sorted(glob.glob(os.path.join(self.SYSTEMS, "*.system"))
                           + glob.glob(os.path.join(self.SYSTEMS, "*", "*.system"))):
            root = ET.parse(path).getroot()
            windows = {mr.get("name"): mr.get("phys_addr") for mr in root.iter("memory_region")
                       if mr.get("phys_addr")}
            for pd in root.iter("protection_domain"):
                if pd.find("program_image").get("path") != "nowayos-timer.elf":
                    continue
                found += 1
                devices = [(windows[m.get("mr")], m.get("setvar_vaddr")) for m in pd.findall("map")
                           if m.get("mr") in windows]
                irqs = [(irq.get("irq"), irq.get("id")) for irq in pd.findall("irq")]
                with self.subTest(system=os.path.relpath(path, self.SYSTEMS)):
                    self.assertIn((devices, irqs), [
                        ([], [("@phys_timer", "0")]),
                        ([("@system_timer", "nowayos_device_0_vaddr")], [("@system_timer[1]", "0")]),
                    ])
        self.assertGreaterEqual(found, 2)

    def test_every_system_builds_for_qemu_and_names_no_address(self):
        devices = self.devices("qemu_virt_aarch64")
        for name in sorted(os.listdir(self.SYSTEMS)):
            if name.endswith(".system"):
                with self.subTest(system=name), open(os.path.join(self.SYSTEMS, name), encoding="utf-8") as handle:
                    resolved, problems = self.tool.resolve(handle.read(), devices, "qemu_virt_aarch64")
                    self.assertEqual(problems, [])
                    self.assertNotIn('="@', resolved)

    def test_every_system_passes_the_grants_check(self):
        """Every NoWayOS system, built for QEMU, passes check-system against
        its grants file and every rule in tools/grant-rules.toml, as `make
        image` checks it before the Microkit tool runs (the capDL check
        follows there)."""
        devices = self.devices("qemu_virt_aarch64")
        names = sorted(name[:-len(".system")] for name in os.listdir(self.SYSTEMS) if name.endswith(".system"))
        self.assertGreaterEqual(len(names), 20)
        with tempfile.TemporaryDirectory() as tmp:
            for name in names:
                with self.subTest(system=name):
                    with open(os.path.join(self.SYSTEMS, f"{name}.system"), encoding="utf-8") as handle:
                        resolved, problems = self.tool.resolve(handle.read(), devices, "qemu_virt_aarch64")
                    self.assertEqual(problems, [])
                    path = os.path.join(tmp, f"{name}.system")
                    with open(path, "w", encoding="utf-8") as handle:
                        handle.write(resolved)
                    status, _, err = run_main(self.check_system, [
                        "--sdf", path, "--grants", os.path.join(self.SYSTEMS, f"{name}.grants"),
                        "--board", "qemu_virt_aarch64"])
                    self.assertEqual(status, 0, err)

    def test_one_source_serves_both_boards(self):
        with open(os.path.join(self.SYSTEMS, "pl011-demo.system"), encoding="utf-8") as handle:
            source = handle.read()
        qemu, problems = self.tool.resolve(source, self.devices("qemu_virt_aarch64"), "qemu_virt_aarch64")
        self.assertEqual(problems, [])
        pi, problems = self.tool.resolve(source, self.devices("rpi5b_2gb"), "rpi5b_2gb")
        self.assertEqual(problems, [])
        self.assertIn('phys_addr="0x9000000"', qemu)
        self.assertIn('phys_addr="0x107d001000"', pi)
        for system, device in (("dma-demo", "fw_cfg"), ("timer-demo", "phys_timer")):
            with self.subTest(system=system), open(os.path.join(self.SYSTEMS, f"{system}.system"), encoding="utf-8") as handle:
                _, problems = self.tool.resolve(handle.read(), self.devices("rpi5b_2gb"), "rpi5b_2gb")
                self.assertTrue(problems and all(f"has no device '{device}'" in problem for problem in problems), problems)

    def test_the_command_writes_the_board_description(self):
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "platforms/testboard.toml", BOARD)
            source = write(tmp, "s.system", self.SOURCE)
            output = os.path.join(tmp, "out", "s.system")
            status, _, err = run_main(self.tool, ["--board", "testboard", "--platforms",
                                                  os.path.join(tmp, "platforms"), source, output])
            self.assertEqual(status, 0, err)
            with open(output, encoding="utf-8") as handle:
                lines = handle.read().splitlines()
            self.assertTrue(lines[0].startswith("<?xml"))
            self.assertIn("Written by tools/board-system for board testboard from s.system", lines[1])
            ET.fromstring("\n".join(lines[1:]))
            bad = write(tmp, "bad.system", '<system><irq irq="33" id="1" /></system>\n')
            status, _, err = run_main(self.tool, ["--board", "testboard", "--platforms",
                                                  os.path.join(tmp, "platforms"), bad, output + ".bad"])
            self.assertEqual(status, 1)
            self.assertIn("cannot be built for board 'testboard'", err)
            self.assertFalse(os.path.exists(output + ".bad"))
            status, _, err = run_main(self.tool, ["--board", "nosuchboard", "--platforms",
                                                  os.path.join(tmp, "platforms"), source, output])
            self.assertEqual(status, 2)
            self.assertIn("cannot read board file", err)

    def test_a_screen_prefill_is_written_beside_the_description(self):
        source = mutated(self.SOURCE, '<memory_region name="buffer" size="0x1000" />',
                         '<memory_region name="screen" prefill_path="screen-160x120.bin" />')
        with tempfile.TemporaryDirectory() as tmp:
            write(tmp, "platforms/testboard.toml", BOARD)
            output = os.path.join(tmp, "out", "s.system")
            status, _, err = run_main(self.tool, ["--board", "testboard", "--platforms",
                                                  os.path.join(tmp, "platforms"),
                                                  write(tmp, "s.system", source), output])
            self.assertEqual(status, 0, err)
            with open(os.path.join(tmp, "out", "screen-160x120.bin"), "rb") as handle:
                self.assertEqual(handle.read(), (160).to_bytes(4, "little") + (120).to_bytes(4, "little"))
        # Only the screen's size is written, by a name that is all digits.
        for name in ("../screen-1x1.bin", "screen-0x1.bin", "screen-1x1.txt", "keys.bin", "screen-1x1x1.bin",
                     "restart-../x.bin", "restart-.bin", "restart-a b.bin"):
            with self.subTest(name=name):
                files, problems = self.tool.prefills(f'<memory_region name="m" prefill_path="{name}" />')
                self.assertEqual(files, {})
                self.assertIn("the only prefills are screen-WxH.bin", problems[0])
        for name in ("device-.bin", "device-a b.bin", "device-../x.bin", "device-uart.txt"):
            with self.subTest(name=name):
                files, problems = self.tool.prefills(f'<memory_region name="m" prefill_path="{name}" />')
                self.assertEqual(files, {})
                self.assertIn("device-NAME.bin, the board's facts", problems[0])
        self.assertEqual(self.tool.prefills('<!-- prefill_path="keys.bin" -->'), ({}, []))
        # A PD's restart image is left to tools/restart-image.
        self.assertEqual(self.tool.prefills('<memory_region name="m" prefill_path="restart-worker.bin" />'),
                         ({}, []))

    def test_a_release_is_written_from_its_name(self):
        """test-release-N.bin: the release a test image says it is,
        1 to 9999 as one little-endian word, so that two releases' images
        differ."""
        for release in (1, 2, 9999):
            name = f"test-release-{release}.bin"
            with self.subTest(name=name):
                self.assertEqual(self.tool.prefills(f'<memory_region name="r" prefill_path="{name}" />'),
                                 ({name: release.to_bytes(4, "little")}, []))
        for name in ("test-release-0.bin", "test-release-01.bin", "test-release-10000.bin", "release-2.bin"):
            with self.subTest(name=name):
                files, problems = self.tool.prefills(f'<memory_region name="r" prefill_path="{name}" />')
                self.assertEqual(files, {})
                self.assertIn("test-release-N.bin, the release a test image says it is", problems[0])

    def test_a_run_length_is_written_from_its_name(self):
        """test-run-length-N.bin: a Pi test system's run length, N
        seconds (1 to a day) as one little-endian word, for the ticker."""
        for seconds in (1, 120, 86_400):
            name = f"test-run-length-{seconds}.bin"
            with self.subTest(name=name):
                self.assertEqual(self.tool.prefills(f'<memory_region name="run" prefill_path="{name}" />'),
                                 ({name: seconds.to_bytes(4, "little")}, []))
        files, problems = self.tool.prefills('<memory_region name="m" prefill_path="test-run-length-86401.bin" />')
        self.assertEqual(files, {})
        self.assertIn("a run length of 86401 s, not 1 to 86400", problems[0])
        for name in ("test-run-length-0.bin", "test-run-length-060.bin", "test-run-length-.bin",
                     "test-run-length-100000.bin", "run-length-60.bin"):
            with self.subTest(name=name):
                files, problems = self.tool.prefills(f'<memory_region name="m" prefill_path="{name}" />')
                self.assertEqual(files, {})
                self.assertIn("or test-run-length-N.bin, a run length on the Pi 5", problems[0])

    def test_a_tcp_allow_list_is_written_from_its_name(self):
        """tcp-allow-A.B.C.D-PORT[+...].bin: a network stack's
        allowed destinations, the layout nowayos_socket::AllowList::decode
        reads - version 1, the count, then each address's bytes and port."""
        def word(value):
            return value.to_bytes(4, "little")
        name = "tcp-allow-10.0.2.100-7+192.168.1.20-3389.bin"
        self.assertEqual(
            self.tool.prefills(f'<memory_region name="allow" prefill_path="{name}" />'),
            ({name: word(1) + word(2) + bytes([10, 0, 2, 100]) + word(7)
              + bytes([192, 168, 1, 20]) + word(3389)}, []))
        # Every rule the decoder keeps is kept here, so a bad list fails the
        # build rather than the PD.
        nine = "+".join(f"10.0.2.{host}-7" for host in range(1, 10))
        for name, why in (("tcp-allow-127.0.0.1-7.bin", "cannot be a TCP destination"),
                          ("tcp-allow-0.1.2.3-7.bin", "cannot be a TCP destination"),
                          ("tcp-allow-224.0.0.1-7.bin", "cannot be a TCP destination"),
                          ("tcp-allow-255.255.255.255-7.bin", "cannot be a TCP destination"),
                          ("tcp-allow-10.0.2.256-7.bin", "is not an IPv4 address"),
                          ("tcp-allow-10.0.2.100-0.bin", "is not 1 to 65535"),
                          ("tcp-allow-10.0.2.100-65536.bin", "is not 1 to 65535"),
                          ("tcp-allow-10.0.2.100-7+10.0.2.100-7.bin", "is listed twice"),
                          (f"tcp-allow-{nine}.bin", "9 destinations, not 1 to 8")):
            with self.subTest(name=name):
                files, problems = self.tool.prefills(f'<memory_region name="m" prefill_path="{name}" />')
                self.assertEqual(files, {})
                self.assertIn(why, problems[0])
        for name in ("tcp-allow-.bin", "tcp-allow-10.0.2.100.bin", "tcp-allow-10.0.2-7.bin",
                     "tcp-allow-10.0.2.100-07.bin", "tcp-allow-010.0.2.100-7.bin",
                     "tcp-allow-10.0.2.100-7+.bin", "tcp-allow-10.0.2.100-7.txt",
                     "tcp-allow-../10.0.2.100-7.bin"):
            with self.subTest(name=name):
                files, problems = self.tool.prefills(f'<memory_region name="m" prefill_path="{name}" />')
                self.assertEqual(files, {})
                self.assertIn("tcp-allow-A.B.C.D-PORT.bin, a network stack's allowed", problems[0])

    def test_a_tls_pin_is_written_from_its_name(self):
        """tls-pin-HEX.bin: a TLS client's pinned server key, the 32
        bytes its 64 lower-case hex digits spell; zeros are allowed (no pin:
        the client asks)."""
        pin = "00112233445566778899aabbccddeeff" * 2
        for value in (pin, "0" * 64):
            name = f"tls-pin-{value}.bin"
            with self.subTest(name=name):
                self.assertEqual(
                    self.tool.prefills(f'<memory_region name="pin" prefill_path="{name}" />'),
                    ({name: bytes.fromhex(value)}, []))
        for name in (f"tls-pin-{pin[:-1]}.bin", f"tls-pin-{pin}0.bin", f"tls-pin-{pin.upper()}.bin",
                     "tls-pin-.bin", f"tls-pin-{pin}.txt", f"tls-pin-../{pin[3:]}.bin"):
            with self.subTest(name=name):
                files, problems = self.tool.prefills(f'<memory_region name="m" prefill_path="{name}" />')
                self.assertEqual(files, {})
                self.assertIn("tls-pin-HEX.bin, a TLS client's pinned server key", problems[0])

    def test_a_devices_facts_are_written_from_the_board_file(self):
        """device-NAME.bin: the board's facts about its device NAME,
        for the driver that holds it - the record nowayos_region's
        device_facts() reads: version 1, the bytes of registers, the
        compatible string's length, then the string, zero-padded."""
        def record(registers, compatible):
            body = ((1).to_bytes(4, "little") + registers.to_bytes(4, "little")
                    + len(compatible).to_bytes(4, "little") + compatible)
            return body + bytes(-len(body) % 4)
        source = '<memory_region name="facts" prefill_path="device-debug_uart.bin" />'
        for board, registers, compatible in (("qemu_virt_aarch64", 0x1000, b"arm,pl011"),
                                             ("rpi5b_2gb", 0x200, b"arm,pl011-axi")):
            with self.subTest(board=board):
                self.assertEqual(self.tool.prefills(source, devices=self.devices(board), board=board),
                                 ({"device-debug_uart.bin": record(registers, compatible)}, []))
        self.assertEqual(len(record(0x200, b"arm,pl011-axi")), 28)
        board = BOARD + textwrap.dedent("""\

            [[device]]
            name = "tick"
            compatible = "arm,armv8-timer"
            irqs = [30]
            trigger = "level"
            source = "test fixture"
            """)
        with tempfile.TemporaryDirectory() as tmp:
            devices = self.check_system.read_board(write(tmp, "testboard.toml", board), "testboard")
        for name, reason in (("gpu", "board 'testboard' has no device 'gpu'"),
                             ("tick", "device 'tick' has no register window on board 'testboard'"),
                             ("uart", "device 'uart' on board 'testboard' has no compatible string")):
            with self.subTest(device=name):
                files, problems = self.tool.prefills(f'<memory_region name="f" prefill_path="device-{name}.bin" />',
                                                     devices=devices, board="testboard")
                self.assertEqual(files, {})
                self.assertEqual(len(problems), 1)
                self.assertIn(reason, problems[0])
        # The command writes it beside the description, for the board it builds.
        with tempfile.TemporaryDirectory() as tmp:
            output = os.path.join(tmp, "out", "s.system")
            status, _, err = run_main(self.tool, ["--board", "rpi5b_2gb", "--platforms", self.PLATFORMS,
                                                  write(tmp, "s.system", f"<system>{source}</system>\n"), output])
            self.assertEqual(status, 0, err)
            with open(os.path.join(tmp, "out", "device-debug_uart.bin"), "rb") as handle:
                self.assertEqual(handle.read(), record(0x200, b"arm,pl011-axi"))

    def test_a_framebuffers_facts_carry_its_mode(self):
        """device-firmware_fb.bin: version 2 - version 1's words with
        the compatible string padded to 64 bytes, then width, height, stride,
        format (1: r5g6b5) and first row - the bytes nowayos_region's tests
        read as the Pi 5's framebuffer."""
        files, problems = self.tool.prefills('<memory_region name="f" prefill_path="device-firmware_fb.bin" />',
                                             devices=self.devices("rpi5b_2gb"), board="rpi5b_2gb")
        self.assertEqual(problems, [])
        compatible = b"simple-framebuffer"
        expected = (b"".join(n.to_bytes(4, "little") for n in (2, 0x3F_4800, len(compatible)))
                    + compatible.ljust(64, b"\0")
                    + b"".join(n.to_bytes(4, "little") for n in (1920, 1080, 3840, 1, 0)))
        self.assertEqual(files, {"device-firmware_fb.bin": expected})
        self.assertEqual(len(expected), 96)

    def test_only_a_display_pd_maps_the_firmware_framebuffer(self):
        """firmware-framebuffer-only-display: fb-test passes with its
        PD in the display role, and in every other role fails naming the
        rule; the Pi's other device grants, to drivers, are untouched by it."""
        with open(os.path.join(HERE, "grant-rules.toml"), "rb") as handle:
            roles = tomllib.load(handle)["roles"]
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.system"), encoding="utf-8") as handle:
            resolved, problems = self.tool.resolve(handle.read(), self.devices("rpi5b_2gb"), "rpi5b_2gb")
        self.assertEqual(problems, [])
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.grants"), encoding="utf-8") as handle:
            grants = handle.read()
        self.assertIn("role fb_test display\n", grants)
        with tempfile.TemporaryDirectory() as tmp:
            built = write(tmp, "fb-test.system", resolved)
            for role in roles:
                with self.subTest(role=role):
                    path = write(tmp, f"{role}.grants", grants.replace("role fb_test display", f"role fb_test {role}"))
                    status, _, err = run_main(self.check_system, ["--sdf", built, "--grants", path,
                                                                  "--board", "rpi5b_2gb"])
                    if role == "display":
                        self.assertEqual(status, 0, err)
                    else:
                        self.assertEqual(status, 1, err)
                        self.assertIn(f"rule 11 (firmware-framebuffer-only-display): 'fb_test', role {role}, is "
                                      f"granted 'device fb_test firmware_fb rw', which only a display PD may hold",
                                      err)

    def test_only_the_display_maps_the_firmware_framebuffer_cached(self):
        """rule 7's one exception, cached-framebuffer-only-display:         fb-test with its framebuffer mapped cached passes as a display PD, the
        grant stated as `cached`; unstated, or in any other role, it fails;
        and any other device mapped cached still fails rule 7
        (CheckSystemDevices.test_rule_7_a_device_map_is_uncached)."""
        with open(os.path.join(HERE, "grant-rules.toml"), "rb") as handle:
            roles = tomllib.load(handle)["roles"]
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.system"), encoding="utf-8") as handle:
            source = handle.read()
        cached = mutated(source, '<map mr="firmware_fb" vaddr="0x2000_0000" perms="rw" cached="false"',
                         '<map mr="firmware_fb" vaddr="0x2000_0000" perms="rw" cached="true"')
        resolved, problems = self.tool.resolve(cached, self.devices("rpi5b_2gb"), "rpi5b_2gb")
        self.assertEqual(problems, [])
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.grants"), encoding="utf-8") as handle:
            grants = handle.read()
        stated = mutated(grants, "device fb_test firmware_fb rw\n",
                         "device fb_test firmware_fb rw\ncached fb_test firmware_fb\n")
        with tempfile.TemporaryDirectory() as tmp:
            built = write(tmp, "fb-test.system", resolved)
            def check(text):
                return run_main(self.check_system, ["--sdf", built, "--grants", write(tmp, "g.grants", text),
                                                    "--board", "rpi5b_2gb"])
            status, _, err = check(stated)
            self.assertEqual(status, 0, err)
            status, _, err = check(grants)
            self.assertEqual(status, 1, err)
            self.assertIn("granted but not in", err)
            self.assertIn("cached fb_test firmware_fb", err)
            for role in roles:
                if role == "display":
                    continue
                with self.subTest(role=role):
                    status, _, err = check(stated.replace("role fb_test display", f"role fb_test {role}"))
                    self.assertEqual(status, 1, err)
                    self.assertIn(f"rule 11 (cached-framebuffer-only-display): 'fb_test', role {role}, is "
                                  f"granted 'cached fb_test firmware_fb', which only a display PD may hold", err)

    def test_only_a_watchdog_pd_maps_the_pm_watchdog(self):
        """pm-watchdog-only-watchdog: pm-watchdog-test passes with its
        PD in the watchdog role, and in every other role fails naming the
        rule."""
        with open(os.path.join(HERE, "grant-rules.toml"), "rb") as handle:
            roles = tomllib.load(handle)["roles"]
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "pm-watchdog-test.system"), encoding="utf-8") as handle:
            resolved, problems = self.tool.resolve(handle.read(), self.devices("rpi5b_2gb"), "rpi5b_2gb")
        self.assertEqual(problems, [])
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "pm-watchdog-test.grants"), encoding="utf-8") as handle:
            grants = handle.read()
        self.assertIn("role pm_watchdog watchdog\n", grants)
        with tempfile.TemporaryDirectory() as tmp:
            built = write(tmp, "pm-watchdog-test.system", resolved)
            for role in roles:
                with self.subTest(role=role):
                    path = write(tmp, f"{role}.grants",
                                 grants.replace("role pm_watchdog watchdog", f"role pm_watchdog {role}"))
                    status, _, err = run_main(self.check_system, ["--sdf", built, "--grants", path,
                                                                  "--board", "rpi5b_2gb"])
                    if role == "watchdog":
                        self.assertEqual(status, 0, err)
                    else:
                        self.assertEqual(status, 1, err)
                        self.assertIn(f"rule 11 (pm-watchdog-only-watchdog): 'pm_watchdog', role {role}, is "
                                      f"granted 'device pm_watchdog pm_watchdog rw', which only a watchdog PD "
                                      f"may hold", err)

    def test_only_an_entropy_pd_maps_the_random_source(self):
        """rng-only-entropy: entropy-test passes with its entropy PD
        in the entropy role, and in every other role fails naming the rule:
        the page QEMU's board file names rng, virtio-rng's."""
        with open(os.path.join(HERE, "grant-rules.toml"), "rb") as handle:
            roles = tomllib.load(handle)["roles"]
        with open(os.path.join(self.SYSTEMS, "entropy-test.system"), encoding="utf-8") as handle:
            resolved, problems = self.tool.resolve(handle.read(), self.devices("qemu_virt_aarch64"),
                                                   "qemu_virt_aarch64")
        self.assertEqual(problems, [])
        with open(os.path.join(self.SYSTEMS, "entropy-test.grants"), encoding="utf-8") as handle:
            grants = handle.read()
        self.assertIn("role entropy entropy\n", grants)
        with tempfile.TemporaryDirectory() as tmp:
            built = write(tmp, "entropy-test.system", resolved)
            for role in roles:
                with self.subTest(role=role):
                    path = write(tmp, f"{role}.grants",
                                 grants.replace("role entropy entropy", f"role entropy {role}"))
                    status, _, err = run_main(self.check_system, ["--sdf", built, "--grants", path,
                                                                  "--board", "qemu_virt_aarch64"])
                    if role == "entropy":
                        self.assertEqual(status, 0, err)
                    else:
                        self.assertEqual(status, 1, err)
                        self.assertIn(f"rule 11 (rng-only-entropy): 'entropy', role {role}, is granted "
                                      f"'device entropy rng rw', which only an entropy PD may hold", err)

    def entropy_test(self):
        """entropy-test's description, resolved for QEMU, and its grants."""
        with open(os.path.join(self.SYSTEMS, "entropy-test.system"), encoding="utf-8") as handle:
            resolved, problems = self.tool.resolve(handle.read(), self.devices("qemu_virt_aarch64"),
                                                   "qemu_virt_aarch64")
        self.assertEqual(problems, [])
        with open(os.path.join(self.SYSTEMS, "entropy-test.grants"), encoding="utf-8") as handle:
            return resolved, handle.read()

    def test_a_seed_ring_has_one_reader_and_it_only_reads(self):
        """seed-ring-one-reader: a region the entropy PD maps
        read-write and gives no device - a seed ring's messages - is mapped
        by one other PD, read-only. entropy-test passes, its request queue's
        DMA region (whose physical address the entropy PD learns) not being
        a seed ring; mapped read-write into its consumer, or into a second
        consumer, it fails naming the rule."""
        resolved, grants = self.entropy_test()
        reader = '<map mr="seed_ring" vaddr="0x4000_1000" perms="r"'
        self.assertEqual(resolved.count(reader), 1)
        second = textwrap.dedent("""\
                <protection_domain name="second_consumer" priority="50" stack_size="0x10000">
                    <program_image path="nowayos-seed-consumer.elf" />
                    <map mr="seed_ring" vaddr="0x4000_1000" perms="r"
                         setvar_vaddr="nowayos_ro_region_0_vaddr" setvar_size="nowayos_ro_region_0_size" />
                </protection_domain>
            </system>
            """)
        cases = [
            (resolved, grants, None),
            (resolved.replace(reader, reader.replace('perms="r"', 'perms="rw"')),
             grants.replace("map seed_consumer seed_ring r\n", "map seed_consumer seed_ring rw\n"),
             "rule 11 (seed-ring-one-reader): 'seed_consumer', role test, maps 'seed_ring' rw, which "
             "'entropy', role entropy, writes for one reader"),
            (resolved.replace("</system>", second.lstrip()),
             grants + "role second_consumer test\npd second_consumer priority=50 budget=1000 period=1000\n"
                      "map second_consumer seed_ring r\n",
             "rule 11 (seed-ring-one-reader): 'seed_ring', which 'entropy', role entropy, writes for one "
             "reader, is mapped by 2 PDs ('second_consumer', 'seed_consumer')"),
        ]
        with tempfile.TemporaryDirectory() as tmp:
            for number, (sdf, grant_text, expected) in enumerate(cases):
                with self.subTest(case=number):
                    status, _, err = run_main(self.check_system, [
                        "--sdf", write(tmp, f"{number}.system", sdf), "--grants",
                        write(tmp, f"{number}.grants", grant_text), "--board", "qemu_virt_aarch64"])
                    if expected is None:
                        self.assertEqual(status, 0, err)
                    else:
                        self.assertEqual(status, 1, err)
                        self.assertIn(expected, err)

    def test_virtio_rng_goes_on_transport_8_only(self):
        """Rule 15: a QEMU device file may put virtio-rng on no bus
        but virtio-mmio-bus.8, transport 8 in the page the board file names
        rng; one naming another bus, or none - QEMU
        then picks the next free transport - fails, and a comment is not a
        device. Every systems/NAME.qemu passes."""
        resolved, grants = self.entropy_test()
        good = textwrap.dedent("""\
            -global virtio-mmio.force-legacy=false
            -object rng-random,id=rng0,filename=/dev/urandom
            -device virtio-rng-device,rng=rng0,bus=virtio-mmio-bus.8
            """)
        with tempfile.TemporaryDirectory() as tmp:
            built = write(tmp, "entropy-test.system", resolved)
            grants_path = write(tmp, "entropy-test.grants", grants)
            def check(text):
                return run_main(self.check_system, ["--sdf", built, "--grants", grants_path, "--board",
                                                    "qemu_virt_aarch64", "--qemu", write(tmp, "test.qemu", text)])
            for text in (good, good + "# -device virtio-rng-device,rng=rng0,bus=virtio-mmio-bus.3\n"):
                status, _, err = check(text)
                self.assertEqual(status, 0, err)
            for text, where in [(good.replace("bus.8", "bus.31"), "puts virtio-rng on virtio-mmio-bus.31"),
                                (good.replace(",bus=virtio-mmio-bus.8", ""), "puts virtio-rng on no bus named"),
                                (good.replace("bus.8", "bus.8,bus=virtio-mmio-bus.9"),
                                 "puts virtio-rng on more than one bus (virtio-mmio-bus.8, virtio-mmio-bus.9)")]:
                with self.subTest(where=where):
                    status, _, err = check(text)
                    self.assertEqual(status, 1, err)
                    self.assertIn("rule 15: ", err)
                    self.assertIn(where, err)
        for path in sorted(glob.glob(os.path.join(self.SYSTEMS, "*.qemu"))):
            with self.subTest(qemu=os.path.basename(path)):
                self.assertEqual(self.check_system.qemu_problems(path), [])

    def test_no_kernel_object_lies_in_the_framebuffer_but_its_windows_frames(self):
        """Rule 14: in the Microkit tool's report, an object inside the
        Pi 5's framebuffer - from its first row, 0x3f80_0000, through what
        the firmware reserves for it, to 0x4000_0000 - is refused unless it
        is a frame of the region that maps its window; objects outside it
        pass."""
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.system"), encoding="utf-8") as handle:
            resolved, _ = self.tool.resolve(handle.read(), self.devices("rpi5b_2gb"), "rpi5b_2gb")
        grants = os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.grants")
        report = textwrap.dedent("""\
            # Kernel Objects Details: ID, Type, Name, Physical Address
            \t0 - Page(4 KiB): 'frame_mr_firmware_fb_000000000' @ 0x00003f80f000
            \t1 - Page(4 KiB): 'frame_mr_firmware_fb_000000997' @ 0x00003fbf4000
            \t2 - CNode: 'cnode_fb_test' @ 0x000000080000
            \t3 - Page(4 KiB): 'frame_fb_test_stack_000000000' @ 0x00003f7ff000
            """)
        with tempfile.TemporaryDirectory() as tmp:
            built = write(tmp, "fb-test.system", resolved)
            def check(text):
                return run_main(self.check_system, ["--sdf", built, "--grants", grants, "--board", "rpi5b_2gb",
                                                    "--report", write(tmp, "report.txt", text)])
            status, _, err = check(report)
            self.assertEqual(status, 0, err)
            for line, where in [("\t4 - Tcb: 'tcb_fb_test' @ 0x00003f900000", "0x3f900000"),
                                ("\t4 - Page(4 KiB): 'frame_fb_test_stack_000000001' @ 0x00003f800000",
                                 "0x3f800000"),
                                ("\t4 - Page(4 KiB): 'frame_mr_firmware_fb_facts_000000000' @ 0x00003fbf4000",
                                 "0x3fbf4000")]:
                with self.subTest(line=line):
                    status, _, err = check(report + line + "\n")
                    self.assertEqual(status, 1, err)
                    self.assertIn(f"rule 14: ", err)
                    self.assertIn(f"lies at {where}, inside board 'rpi5b_2gb''s framebuffer 'firmware_fb' "
                                  f"(0x3f800000 to 0x40000000)", err)
            # Past the framebuffer's last byte but inside what the firmware
            # reserves for it: refused too.
            status, _, err = check(report + "\t4 - Tcb: 'tcb_fb_test' @ 0x00003fbf5000\n")
            self.assertEqual(status, 1, err)
            # Past the reservation: not the framebuffer's.
            status, _, err = check(report + "\t4 - Page(4 KiB): 'frame_x_000000000' @ 0x000040000000\n")
            self.assertEqual(status, 0, err)
            status, _, err = check("no objects here\n")
            self.assertEqual(status, 2, err)
            self.assertIn("no kernel objects listed", err)

    def test_the_sdks_ram_covers_no_framebuffer(self):
        """Rule 14: the RAM the Microkit SDK's kernel hands out for
        the board (its platform_gen.json) may end at or below the Pi 5's
        framebuffer, whose reservation runs from 0x3f80_0000 to 0x4000_0000,
        but never cover any of it: Microkit 2.3.1's release, whose RAM ran
        to 0x3fc0_0000, is refused; something that is not a memory
        description is an input error."""
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.system"), encoding="utf-8") as handle:
            resolved, _ = self.tool.resolve(handle.read(), self.devices("rpi5b_2gb"), "rpi5b_2gb")
        grants = os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.grants")
        with tempfile.TemporaryDirectory() as tmp:
            built = write(tmp, "fb-test.system", resolved)
            def check(text):
                return run_main(self.check_system, ["--sdf", built, "--grants", grants, "--board", "rpi5b_2gb",
                                                    "--sdk-memory", write(tmp, "platform_gen.json", text)])
            def memory(*ranges):
                return json.dumps({"devices": [{"start": 0, "end": 0x8_0000}],
                                   "memory": [{"start": low, "end": high} for low, high in ranges]})
            for ranges in [[(0x8_0000, 0x3f40_0000)], [(0x8_0000, 0x3f80_0000)],
                           [(0x8_0000, 0x3f40_0000), (0x4000_0000, 0x1_0000_0000)]]:
                with self.subTest(ranges=ranges):
                    status, _, err = check(memory(*ranges))
                    self.assertEqual(status, 0, err)
            for ranges, (low, high) in [([(0x8_0000, 0x3fc0_0000)], (0x8_0000, 0x3fc0_0000)),
                                        ([(0x8_0000, 0x3f40_0000), (0x3fff_f000, 0x4000_1000)],
                                         (0x3fff_f000, 0x4000_1000))]:
                with self.subTest(ranges=ranges):
                    status, _, err = check(memory(*ranges))
                    self.assertEqual(status, 1, err)
                    self.assertIn(f"rule 14: the Microkit SDK gives seL4 RAM from {low:#x} to {high:#x}", err)
                    self.assertIn("over board 'rpi5b_2gb''s framebuffer 'firmware_fb' (0x3f800000 to 0x40000000)",
                                  err)
            for text in ["{}", memory(), "[1, 2]", "not json"]:
                with self.subTest(text=text):
                    status, _, err = check(text)
                    self.assertEqual(status, 2, err)
                    self.assertIn("platform_gen.json", err)

    @unittest.skipUnless(os.path.isdir(os.path.join(os.environ.get("MICROKIT_SDK", "/nonexistent"), "board",
                                                    "rpi5b_2gb")), "needs the dev image's Microkit SDK")
    def test_the_dev_images_sdk_gives_sel4_no_framebuffer_on_the_pi(self):
        """In every rpi5b_2gb configuration of the dev
        image's Microkit SDK, seL4's RAM ends below the Pi 5's framebuffer
        (tools/container/Containerfile builds that board's kernel)."""
        sdk = os.path.join(os.environ["MICROKIT_SDK"], "board", "rpi5b_2gb")
        with open(os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.system"), encoding="utf-8") as handle:
            resolved, _ = self.tool.resolve(handle.read(), self.devices("rpi5b_2gb"), "rpi5b_2gb")
        grants = os.path.join(self.SYSTEMS, "rpi5b_2gb", "fb-test.grants")
        configs = sorted(os.listdir(sdk))
        self.assertIn("debug", configs)
        with tempfile.TemporaryDirectory() as tmp:
            built = write(tmp, "fb-test.system", resolved)
            for config in configs:
                with self.subTest(config=config):
                    status, _, err = run_main(self.check_system, [
                        "--sdf", built, "--grants", grants, "--board", "rpi5b_2gb",
                        "--sdk-memory", os.path.join(sdk, config, "platform_gen.json")])
                    self.assertEqual(status, 0, err)

    def test_a_device_on_a_bus_has_version_3_facts(self):
        """device-NAME.bin for a device on a bus: version 3, the
        compatible string padded to 64 bytes, then the PCI bus address its
        driver places BAR 0 at (QEMU's xHCI controllers: pci_bar0) and the
        bus address of physical 0 (RP1's, on the Pi 5: dma_offset), 64 bits
        each, 0 for either not given."""
        def record(registers, compatible, bar, dma):
            body = ((3).to_bytes(4, "little") + registers.to_bytes(4, "little")
                    + len(compatible).to_bytes(4, "little") + compatible)
            return body + bytes(64 - len(compatible)) + bar.to_bytes(8, "little") + dma.to_bytes(8, "little")
        for board, name, expected in (
                ("qemu_virt_aarch64", "xhci0", record(0x4000, b"pci1b36,d", 0x1000_0000, 0)),
                ("qemu_virt_aarch64", "xhci1", record(0x4000, b"pci1b36,d", 0x1000_4000, 0)),
                ("rpi5b_2gb", "xhci0", record(0x1000, b"snps,dwc3", 0, 0x10_0000_0000)),
                ("rpi5b_2gb", "xhci1", record(0x1000, b"snps,dwc3", 0, 0x10_0000_0000))):
            with self.subTest(board=board, device=name):
                source = f'<memory_region name="f" prefill_path="device-{name}.bin" />'
                files, problems = self.tool.prefills(source, devices=self.devices(board), board=board)
                self.assertEqual((files, problems), ({f"device-{name}.bin": expected}, []))
                self.assertEqual(len(expected), 92)
        for key, value, reason in (
                ("pci_bar0", "0x1000_0800", "pci_bar0 must be a bus address above 0, aligned to the window"),
                ("pci_bar0", "0", "pci_bar0 must be"),
                ("pci_bar0", "0x0900_0000", "device 'bus': pci_bar0 0x9000000 puts its window over device 'uart'"),
                ("dma_offset", "0x10_0000_0800", "dma_offset must be a page-aligned bus address above 0"),
                ("dma_offset", '"x"', "dma_offset must be")):
            with self.subTest(key=key, value=value):
                board = BOARD + textwrap.dedent(f"""\

                    [[device]]
                    name = "bus"
                    compatible = "test,bus"
                    address = 0x2000_0000
                    size = 0x1000
                    {key} = {value}
                    irqs = []
                    trigger = "level"
                    source = "test fixture"
                    """)
                with tempfile.TemporaryDirectory() as tmp:
                    with self.assertRaises(self.check_system.InputError) as caught:
                        self.check_system.read_board(write(tmp, "testboard.toml", board), "testboard")
                self.assertIn(reason, str(caught.exception))

    def test_the_pl011_and_console_pds_are_given_their_uarts_facts(self):
        """Every description that runs the PL011 driver or the console
        gives it the debug UART's window as device 0 and the board's facts
        about it, device-debug_uart.bin, as read-only region 5 - what
        nowayos_region::device_facts() binds - so the driver reads no
        register the board does not have."""
        found = 0
        for path in sorted(glob.glob(os.path.join(self.SYSTEMS, "*.system"))
                           + glob.glob(os.path.join(self.SYSTEMS, "*", "*.system"))):
            root = ET.parse(path).getroot()
            regions = {mr.get("name"): mr for mr in root.iter("memory_region")}
            for pd in root.iter("protection_domain"):
                if pd.find("program_image").get("path") not in ("nowayos-pl011.elf", "nowayos-console.elf"):
                    continue
                found += 1
                bound = {m.get("setvar_vaddr"): regions[m.get("mr")] for m in pd.findall("map")}
                with self.subTest(system=os.path.relpath(path, self.SYSTEMS), pd=pd.get("name")):
                    self.assertEqual(bound["nowayos_device_0_vaddr"].get("phys_addr"), "@debug_uart")
                    self.assertEqual(bound["nowayos_ro_region_5_vaddr"].get("prefill_path"), "device-debug_uart.bin")
        self.assertGreaterEqual(found, 3)

    def test_a_supervisors_stand_in_counts_are_written_beside_the_description(self):
        # What a read of the counts' store would return - the bytes read, as a 32-bit
        # little-endian count, then the record - the bytes nowayos_fault_policy::stand_in's own
        # tests read for test-counts-3-1.bin.
        def stand_in(record):
            return len(record).to_bytes(4, "little") + bytes(record)
        cases = {
            "test-counts-3-1.bin": stand_in([1, 3, 1, 0x70, 0xD0, 0xA9, 0xA2]),
            "test-counts-0-0.bin": stand_in([1, 0, 0, 0x25, 0xB3, 0x83, 0xFE]),
            "test-counts-255-255.bin": stand_in(self.tool.counts_record(255, 255)),
            "test-counts-missing.bin": stand_in([]),
            "test-counts-cut-short.bin": stand_in([1, 0, 0, 0x25]),
            "test-counts-unknown-version.bin": stand_in([2, 0, 0, 0x7C, 0x0D, 0xC5, 0xFC]),
            "test-counts-bad-check.bin": stand_in([1, 0, 0, 0x25, 0xB3, 0x83, 0x01]),
        }
        for name, contents in cases.items():
            with self.subTest(name=name):
                self.assertEqual(self.tool.prefills(f'<memory_region name="counts" prefill_path="{name}" />'),
                                 ({name: contents}, []))
        for name in ("test-counts-256-0.bin", "test-counts-0-1000.bin", "test-counts-01-0.bin", "test-counts-1.bin",
                     "test-counts-1-0-0.bin", "test-counts-.bin", "test-counts-lost.bin", "counts-1-0.bin"):
            with self.subTest(name=name):
                files, problems = self.tool.prefills(f'<memory_region name="counts" prefill_path="{name}" />')
                self.assertEqual(files, {})
                self.assertIn("test-counts-R-S.bin", problems[0])

    def test_the_update_tests_prefills_come_from_the_bundles_folder(self):
        # The run's root keys, counted, and the provisioned state, from what
        # tools/update-bundles wrote; refused with no folder, no file, or a wrong size.
        anchors = '<memory_region name="anchors" prefill_path="test-trust-anchors.bin" />'
        state = '<memory_region name="state" prefill_path="test-update-state.bin" />'
        with tempfile.TemporaryDirectory() as folder:
            with open(os.path.join(folder, "anchors.bin"), "wb") as handle:
                handle.write(bytes(range(64)) * 2)
            with open(os.path.join(folder, "state-initial.bin"), "wb") as handle:
                handle.write(bytes(860))
            self.assertEqual(self.tool.prefills(anchors, update_bundles=folder),
                             ({"test-trust-anchors.bin": (2).to_bytes(4, "little") + bytes(range(64)) * 2}, []))
            self.assertEqual(self.tool.prefills(state, update_bundles=folder),
                             ({"test-update-state.bin": bytes(860)}, []))
            with open(os.path.join(folder, "anchors.bin"), "wb") as handle:
                handle.write(bytes(65))
            with open(os.path.join(folder, "state-initial.bin"), "wb") as handle:
                handle.write(bytes(861))
            for text, reason in ((anchors, "not 1 to 4 keys"), (state, "not one state copy's 860")):
                files, problems = self.tool.prefills(text, update_bundles=folder)
                self.assertEqual(files, {})
                self.assertIn(reason, problems[0])
        for text in (anchors, state):
            files, problems = self.tool.prefills(text)
            self.assertEqual(files, {})
            self.assertIn("no --update-bundles folder", problems[0])
            files, problems = self.tool.prefills(text, update_bundles="/nonexistent-update-bundles")
            self.assertIn("run make update-bundles", problems[0])

    def test_a_grants_file_may_leave_a_single_interrupt_to_the_board(self):
        check = CheckSystemDevices()
        sdf = DEVICE_SDF.replace('<irq irq="79" id="2" trigger="edge" />', "")
        grants = DEVICE_GRANTS.replace("irq serial uart 33", "irq serial uart").replace("irq netdrv net 79\n", "")
        status, _, err = check.check(sdf=sdf, grants=grants)
        self.assertEqual(status, 0, err)
        status, _, err = check.check(grants=DEVICE_GRANTS.replace("irq netdrv net 79", "irq netdrv net"))
        self.assertEqual(status, 1)
        self.assertIn("device 'net' has 2 interrupts on board 'testboard': name the one", err)

    def test_no_code_tests_for_a_board(self):
        """Board facts live in platforms/: no crate or PD branches on the board."""
        pattern = __import__("re").compile(r"cfg!?\s*\([^)]*\b(qemu\w*|rpi\w*|bcm\w*|board\w*|virt)\b")
        offenders = []
        for top in ("crates", "pds"):
            for root, dirs, files in os.walk(os.path.join(HERE, os.pardir, top)):
                dirs[:] = [d for d in dirs if d != "target"]
                for name in files:
                    if name.endswith(".rs"):
                        path = os.path.join(root, name)
                        with open(path, encoding="utf-8") as handle:
                            for number, line in enumerate(handle, start=1):
                                if pattern.search(line):
                                    offenders.append(f"{os.path.relpath(path, HERE)}:{number}: {line.strip()}")
        self.assertEqual(offenders, [])



RESTART_SDF = mutated(mutated(mutated(SDF,
    '<memory_region name="control" size="0x1000" />',
    '<memory_region name="control" size="0x1000" />\n'
    '    <memory_region name="helper_restart" size="0x1000" prefill_path="restart-helper.bin" />'),
    '            <program_image path="helper.elf" />',
    '            <program_image path="helper.elf" />\n'
    '            <map mr="helper_restart" vaddr="0x5000_0000" perms="r"\n'
    '                 setvar_vaddr="nowayos_restart_image_vaddr" setvar_size="nowayos_restart_image_size" />'),
    '        <protection_domain name="helper" id="1" priority="1">',
    '        <map mr="helper_restart" vaddr="0x5000_0000" perms="r"\n'
    '             setvar_vaddr="nowayos_ro_region_1_vaddr" setvar_size="nowayos_ro_region_1_size" />\n'
    '        <protection_domain name="helper" id="1" priority="1">')

RESTART_GRANTS = GRANTS + textwrap.dedent("""\
    map helper helper_restart r
    map producer helper_restart r
    restart producer helper
    """)


class CheckSystemRestart(unittest.TestCase):
    """Rule 13: who maps a restart image, and where one may be."""

    tool = load("check-system")

    def check(self, sdf=RESTART_SDF, grants=RESTART_GRANTS):
        with tempfile.TemporaryDirectory() as tmp:
            return run_main(self.tool, ["--sdf", write(tmp, "s.system", sdf),
                                        "--grants", write(tmp, "s.grants", grants)])

    def assert_fails(self, result, reason):
        status, _, err = result
        self.assertEqual(status, 1, err)
        self.assertIn(reason, err)

    def test_a_restart_image_mapped_by_its_pd_and_its_parent_passes(self):
        status, out, err = self.check()
        self.assertEqual(status, 0, err)
        self.assertIn("description and grants agree", out)

    def test_the_parent_that_maps_it_is_listed_as_restarting_the_pd(self):
        self.assert_fails(self.check(grants=RESTART_GRANTS.replace("restart producer helper\n", "")),
                          "rule 1: granted but not in")
        self.assert_fails(self.check(grants=RESTART_GRANTS + "restart producer consumer\n"),
                          "rule 1: listed in")

    def test_no_product_system_restarts_a_pd(self):
        self.assert_fails(self.check(grants=RESTART_GRANTS.replace("system demo", "system product")),
                          "rule 13: a product system restarts no PD on its own")

    def test_only_the_pd_and_its_parent_may_map_it(self):
        sdf = mutated(RESTART_SDF, '<program_image path="consumer.elf" />',
                      '<program_image path="consumer.elf" />\n'
                      '        <map mr="helper_restart" vaddr="0x5000_0000" perms="r" />')
        self.assert_fails(self.check(sdf=sdf, grants=RESTART_GRANTS + "map consumer helper_restart r\n"),
                          "'consumer' maps it, but only 'helper' and its parent may")

    def test_the_pd_binds_its_own_image_with_the_restart_symbols(self):
        unbound = mutated(RESTART_SDF, 'setvar_vaddr="nowayos_restart_image_vaddr" setvar_size="nowayos_restart_image_size"',
                          'setvar_vaddr="nowayos_ro_region_0_vaddr" setvar_size="nowayos_ro_region_0_size"')
        self.assert_fails(self.check(sdf=unbound), "'helper' must map it, with "
                                                   "setvar_vaddr=\"nowayos_restart_image_vaddr\"")
        half = mutated(RESTART_SDF, 'setvar_size="nowayos_restart_image_size"', 'setvar_size="nowayos_ro_region_0_size"')
        self.assert_fails(self.check(sdf=half), "a restart image binds setvar_vaddr=\"nowayos_restart_image_vaddr\" "
                                                "with setvar_size=\"nowayos_restart_image_size\"")
        # The parent cannot bind the child's image as its own.
        stolen = mutated(RESTART_SDF, 'setvar_vaddr="nowayos_ro_region_1_vaddr" setvar_size="nowayos_ro_region_1_size"',
                         'setvar_vaddr="nowayos_restart_image_vaddr" setvar_size="nowayos_restart_image_size"')
        self.assert_fails(self.check(sdf=stolen), "protection_domain 'producer' binds nowayos_restart_image_vaddr "
                                                  "to 'helper_restart', which is not its restart image")

    def test_a_restart_image_is_read_only_and_names_a_pd(self):
        writable = mutated(RESTART_SDF, '<map mr="helper_restart" vaddr="0x5000_0000" perms="r"\n                 setvar_vaddr="nowayos_restart_image_vaddr"',
                           '<map mr="helper_restart" vaddr="0x5000_0000" perms="rw"\n                 setvar_vaddr="nowayos_restart_image_vaddr"')
        result = self.check(sdf=writable, grants=RESTART_GRANTS.replace("map helper helper_restart r", "map helper helper_restart rw"))
        self.assert_fails(result, "rule 12:")
        self.assert_fails(result, "binds a read-only view of memory, so the map must have perms=\"r\"")
        orphan = mutated(RESTART_SDF, 'prefill_path="restart-helper.bin"', 'prefill_path="restart-nobody.bin"')
        self.assert_fails(self.check(sdf=orphan), "the restart image of 'nobody': no protection_domain has that name")


HEAP_SDF = mutated(mutated(SDF,
    '<memory_region name="control" size="0x1000" />',
    '<memory_region name="control" size="0x1000" />\n'
    '    <memory_region name="consumer_heap" size="0x200000" page_size="0x200000" />'),
    '        <program_image path="consumer.elf" />',
    '        <program_image path="consumer.elf" />\n'
    '        <map mr="consumer_heap" vaddr="0x6000_0000" perms="rw"\n'
    '             setvar_vaddr="nowayos_heap_region_vaddr" setvar_size="nowayos_heap_region_size" />')

HEAP_GRANTS = GRANTS + "map consumer consumer_heap rw\n"


class CheckSystemHeap(unittest.TestCase):
    """Rule 16: a heap in a memory region is its PD's alone."""

    tool = load("check-system")

    def check(self, sdf=HEAP_SDF, grants=HEAP_GRANTS):
        with tempfile.TemporaryDirectory() as tmp:
            return run_main(self.tool, ["--sdf", write(tmp, "s.system", sdf),
                                        "--grants", write(tmp, "s.grants", grants)])

    def assert_fails(self, result, reason):
        status, _, err = result
        self.assertEqual(status, 1, err)
        self.assertIn(reason, err)

    def test_a_heap_region_mapped_by_its_pd_alone_passes(self):
        status, out, err = self.check()
        self.assertEqual(status, 0, err)
        self.assertIn("description and grants agree", out)

    def test_no_other_pd_maps_a_heap(self):
        shared = mutated(HEAP_SDF, '<program_image path="producer.elf" />',
                         '<program_image path="producer.elf" />\n'
                         '        <map mr="consumer_heap" vaddr="0x6000_0000" perms="r" />')
        self.assert_fails(self.check(sdf=shared, grants=HEAP_GRANTS + "map producer consumer_heap r\n"),
                          "rule 16: memory_region 'consumer_heap', the heap of 'consumer': 'producer' maps it too")

    def test_a_heap_is_read_write_and_binds_both_symbols(self):
        read_only = mutated(HEAP_SDF, '<map mr="consumer_heap" vaddr="0x6000_0000" perms="rw"',
                            '<map mr="consumer_heap" vaddr="0x6000_0000" perms="r"')
        self.assert_fails(self.check(sdf=read_only, grants=HEAP_GRANTS.replace("consumer_heap rw", "consumer_heap r")),
                          "nowayos_heap_region_vaddr binds a heap's arena, so the map must have perms=\"rw\"")
        half = mutated(HEAP_SDF, 'setvar_size="nowayos_heap_region_size"', 'setvar_size="nowayos_rw_region_1_size"')
        self.assert_fails(self.check(sdf=half), "a heap region binds setvar_vaddr=\"nowayos_heap_region_vaddr\" "
                                                "with setvar_size=\"nowayos_heap_region_size\"")

    def test_a_heap_starts_empty(self):
        prefilled = mutated(HEAP_SDF, '<memory_region name="consumer_heap" size="0x200000" page_size="0x200000" />',
                            '<memory_region name="consumer_heap" prefill_path="heap.bin" />')
        result = self.check(sdf=prefilled, grants=HEAP_GRANTS.replace("consumer_heap rw", "consumer_heap r"))
        self.assert_fails(result, "the heap of 'consumer': a heap's arena starts empty, so it is not prefilled")


class SupervisorCountsSystems(unittest.TestCase):
    """The supervisor's stand-in counts, in NoWayOS's own systems."""

    tool = load("check-system")
    SYSTEMS = os.path.join(HERE, os.pardir, "systems")
    COUNTS_LINE = re.compile(r"NOWAYOS: supervisor: (counts read from the test stand-in|the counts cannot be read"
                             r"|restart count \d+ of \d+, sensitive count \d+ of \d+: )")

    def read(self, name):
        with open(os.path.join(self.SYSTEMS, name), encoding="utf-8") as handle:
            return handle.read()

    def test_a_demo_system_with_the_stand_in_fails(self):
        # The fail-safe test as a demo system: the stand-in is refused (rule 12), and so is any
        # memory for the supervisor (rule 11), which a test system alone may give it.
        sdf = os.path.join(self.SYSTEMS, "failsafe-restart-limit.system")
        grants = self.read("failsafe-restart-limit.grants")
        with tempfile.TemporaryDirectory() as tmp:
            status, _, err = run_main(self.tool, ["--sdf", sdf, "--grants",
                                                  write(tmp, "demo.grants", mutated(grants, "system test", "system demo"))])
        self.assertEqual(status, 1, err)
        self.assertIn("rule 12: memory_region 'counts' is prefilled from test-counts-4-0.bin, a test-only stand-in: "
                      "only a test system may have one", err)
        self.assertIn("rule 11 (supervisor-no-memory): 'failsafe_supervisor', role supervisor, is granted", err)
        status, _, err = run_main(self.tool, ["--sdf", sdf, "--grants",
                                              os.path.join(self.SYSTEMS, "failsafe-restart-limit.grants")])
        self.assertEqual(status, 0, err)

    def test_the_failsafe_systems_differ_only_in_their_stand_in(self):
        # Six systems, one per way into the fail-safe state at start-up: the same PDs and grants,
        # each with its own stand-in counts, and expect files that differ only in the two lines on
        # the counts.
        names = sorted(name[:-len(".system")] for name in os.listdir(self.SYSTEMS)
                       if name.startswith("failsafe-") and name.endswith(".system"))
        self.assertEqual(len(names), 6, names)

        def system(name):
            text = re.sub(r"<!--.*?-->", "", self.read(f"{name}.system"), flags=re.DOTALL)
            return re.sub(r'prefill_path="test-counts-[^"]*"', 'prefill_path=""', text), \
                re.findall(r'prefill_path="([^"]*)"', text)

        def expect(name):
            lines = [line for line in self.read(f"{name}.expect").splitlines() if not line.startswith("#")]
            return [line for line in lines if not self.COUNTS_LINE.match(line)], \
                [line for line in lines if self.COUNTS_LINE.match(line)]

        base_system, _ = system(names[0])
        base_expect, _ = expect(names[0])
        prefills = set()
        for name in names:
            with self.subTest(system=name):
                text, prefill = system(name)
                self.assertEqual(text, base_system)
                self.assertEqual(len(prefill), 1)
                self.assertTrue(prefill[0].startswith("test-counts-"), prefill)
                prefills.update(prefill)
                self.assertEqual(self.read(f"{name}.grants"), self.read(f"{names[0]}.grants"))
                rest, counts_lines = expect(name)
                self.assertEqual(rest, base_expect)
                self.assertEqual(len(counts_lines), 2, counts_lines)
                self.assertTrue(counts_lines[1].endswith(": enter the fail-safe state"), counts_lines)
        self.assertEqual(len(prefills), 6, prefills)


def elf_file(entry, segments, symbols):
    """A minimal 64-bit little-endian ELF executable, as restart-image reads
    one: `segments` [(vaddr, flags, file bytes, memsz)], `symbols`
    {name: (value, size)}."""
    import struct
    names = b"\0" + b"".join(name.encode() + b"\0" for name in symbols)
    header_len, phdr_len, shdr_len, sym_len = 64, 56, 64, 24
    phoff = header_len
    data_start = phoff + phdr_len * len(segments)
    blobs, phdrs, offset = b"", b"", data_start
    for vaddr, flags, data, memsz in segments:
        phdrs += struct.pack("<IIQQQQQQ", 1, flags, offset, vaddr, vaddr, len(data), memsz, 0x1000)
        blobs += data
        offset += len(data)
    symtab = bytes(sym_len)
    position = 1
    for name, (value, size) in symbols.items():
        symtab += struct.pack("<IBBHQQ", position, 0x11, 0, 1, value, size)
        position += len(name) + 1
    symtab_off = offset
    strtab_off = symtab_off + len(symtab)
    shoff = strtab_off + len(names)
    shdrs = bytes(shdr_len)
    shdrs += struct.pack("<IIQQQQIIQQ", 0, 2, 0, 0, symtab_off, len(symtab), 2, 1, 8, sym_len)
    shdrs += struct.pack("<IIQQQQIIQQ", 0, 3, 0, 0, strtab_off, len(names), 0, 0, 1, 0)
    ident = b"\x7fELF" + bytes([2, 1, 1]) + bytes(9)
    header = ident + struct.pack("<HHIQQQIHHHHHH", 2, 0xB7, 1, entry, phoff, shoff, 0, header_len,
                                 phdr_len, len(segments), shdr_len, 3, 0)
    return header + phdrs + blobs + symtab + names + shdrs


RESTART_TOOL_SDF = textwrap.dedent("""\
    <system>
        <memory_region name="worker_restart" size="0x1000" prefill_path="restart-worker.bin" />
        <memory_region name="ring" size="0x1000" />
        <protection_domain name="worker" priority="1">
            <program_image path="worker.elf" />
            <map mr="worker_restart" vaddr="0x5000_0000" perms="r"
                 setvar_vaddr="nowayos_restart_image_vaddr" setvar_size="nowayos_restart_image_size" />
            <map mr="ring" vaddr="0x4000_0000" perms="rw"
                 setvar_vaddr="nowayos_rw_region_0_vaddr" setvar_size="nowayos_rw_region_0_size" />
        </protection_domain>
    </system>
    """)

# The worker's program: code at 0x20_0000 (its entry), and one writable
# segment of 0x40 file bytes and 0x60 in memory, holding the patched words
# (the restart image's two, then microkit_name, then the ring's two).
RESTART_DATA = bytes(range(0x40))
RESTART_SYMBOLS = {
    "nowayos_restart_entry": (0x20_0010, 0),
    "nowayos_restart_image_size": (0x30_0000, 8),
    "nowayos_restart_image_vaddr": (0x30_0008, 8),
    "microkit_name": (0x30_0010, 16),
    "nowayos_rw_region_0_size": (0x30_0028, 8),
    "nowayos_rw_region_0_vaddr": (0x30_0030, 8),
    "unrelated": (0x30_0038, 8),
}


class RestartImage(unittest.TestCase):
    """tools/restart-image, against nowayos-restart's format."""

    tool = load("restart-image")

    def build(self, tmp, symbols=RESTART_SYMBOLS, entry=0x20_0010, sdf=RESTART_TOOL_SDF, extra_segment=None):
        segments = [(0x20_0000, 5, bytes(0x20), 0x20), (0x30_0000, 6, RESTART_DATA, 0x60)]
        if extra_segment:
            segments.append(extra_segment)
        with open(os.path.join(tmp, "worker.elf"), "wb") as handle:
            handle.write(elf_file(entry, segments, symbols))
        return write(tmp, "s.system", sdf)

    def test_an_image_holds_the_entry_the_stack_the_segment_and_the_kept_words(self):
        import struct
        with tempfile.TemporaryDirectory() as tmp:
            status, out, err = run_main(self.tool, ["--sdf", self.build(tmp), "--dir", tmp])
            self.assertEqual(status, 0, err)
            self.assertIn("1 restart image(s) written: worker", out)
            with open(os.path.join(tmp, "restart-worker.bin"), "rb") as handle:
                image = handle.read()
        self.assertEqual(len(image), 0x1000, "padded to its region's size")
        magic, version, segments, keeps, reserved, entry, stack = struct.unpack_from("<8sIIIIQQ", image)
        self.assertEqual((magic, version, segments, keeps, reserved), (b"BRESTART", 1, 1, 2, 0))
        self.assertEqual((entry, stack), (0x20_0010, 0xFF_FFFF_E000))
        vaddr, filesz, memsz, offset = struct.unpack_from("<QQQQ", image, 40)
        self.assertEqual((vaddr, filesz, memsz), (0x30_0000, 0x40, 0x60))
        self.assertEqual(offset % 8, 0)
        self.assertEqual(image[offset:offset + filesz], RESTART_DATA)
        # The patched words, adjacent ones joined: the image's two and
        # microkit_name; then the ring's two. `unrelated` is restored.
        self.assertEqual([struct.unpack_from("<QQ", image, 72 + 16 * i) for i in range(2)],
                         [(0x30_0000, 0x20), (0x30_0028, 0x10)])

    def test_a_program_that_does_not_start_at_the_restart_entry_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            status, _, err = run_main(self.tool, ["--sdf", self.build(tmp, entry=0x20_0000), "--dir", tmp])
        self.assertEqual(status, 1)
        self.assertIn("the program does not start at nowayos_restart_entry", err)

    def test_a_patched_symbol_the_program_lacks_or_keeps_in_bss_is_refused(self):
        lacking = {name: value for name, value in RESTART_SYMBOLS.items() if name != "nowayos_rw_region_0_size"}
        with tempfile.TemporaryDirectory() as tmp:
            status, _, err = run_main(self.tool, ["--sdf", self.build(tmp, symbols=lacking), "--dir", tmp])
        self.assertEqual(status, 1)
        self.assertIn("sets 'nowayos_rw_region_0_size', which the program does not define", err)
        in_bss = dict(RESTART_SYMBOLS, nowayos_rw_region_0_size=(0x30_0048, 8))
        with tempfile.TemporaryDirectory() as tmp:
            status, _, err = run_main(self.tool, ["--sdf", self.build(tmp, symbols=in_bss), "--dir", tmp])
        self.assertEqual(status, 1)
        self.assertIn("lies in a writable segment but not in its file bytes", err)

    def test_an_image_larger_than_its_region_is_refused(self):
        big = (0x40_0000, 6, bytes(0x1000), 0x1000)
        with tempfile.TemporaryDirectory() as tmp:
            status, _, err = run_main(self.tool, ["--sdf", self.build(tmp, extra_segment=big), "--dir", tmp])
        self.assertEqual(status, 1)
        self.assertIn("but memory_region 'worker_restart' is 0x1000: make the region larger", err)

    def test_the_capdl_check_wants_the_threads_start_as_the_image_says(self):
        with tempfile.TemporaryDirectory() as tmp:
            sdf = self.build(tmp)
            self.assertEqual(run_main(self.tool, ["--sdf", sdf, "--dir", tmp])[0], 0)

            def spec(ip, sp):
                tcb = {"name": "tcb_worker", "object": {"Tcb": {"slots": [], "extra": {"ip": ip, "sp": sp}}}}
                return write(tmp, "capdl.json", json.dumps({"objects": [tcb]}))

            status, out, err = run_main(self.tool, ["--sdf", sdf, "--dir", tmp, "--capdl", spec(0x20_0010, 0xFF_FFFF_E000)])
            self.assertEqual(status, 0, err)
            self.assertIn("checked against the capDL spec", out)
            status, _, err = run_main(self.tool, ["--sdf", sdf, "--dir", tmp, "--capdl", spec(0x20_0010, 0xFF_FFFF_F000)])
            self.assertEqual(status, 1)
            self.assertIn("a restart would not start it as the Microkit tool did", err)

    def test_the_restart_test_is_the_only_system_with_a_restart_image(self):
        """Every restartable PD in NoWayOS's systems is in a test system."""
        found = []
        for path in sorted(glob.glob(os.path.join(HERE, os.pardir, "systems", "*.system"))):
            with open(path, encoding="utf-8") as handle:
                if "prefill_path=\"restart-" in handle.read():
                    found.append(os.path.basename(path))
        self.assertEqual(found, ["restart-test.system"])


class BoardSystemBuildTime(unittest.TestCase):
    """Tools/board-system writes build-time.bin - the image's build
    time, the floor for wall-clock time - from
    SOURCE_DATE_EPOCH, or else the latest commit time of the clones it is
    given; the same commit gives the same bytes; a time later than this
    machine's clock is refused, naming both."""

    tool = load("board-system")
    GIT_ENV = {
        "GIT_CONFIG_NOSYSTEM": "1",
        "GIT_CONFIG_GLOBAL": os.devnull,
        "GIT_AUTHOR_NAME": "Test",
        "GIT_AUTHOR_EMAIL": "test@example.invalid",
        "GIT_COMMITTER_NAME": "Test",
        "GIT_COMMITTER_EMAIL": "test@example.invalid",
    }
    SOURCE = textwrap.dedent("""\
        <?xml version="1.0" encoding="UTF-8"?>
        <system>
            <memory_region name="build_time" prefill_path="build-time.bin" />
            <protection_domain name="clock" priority="2">
                <program_image path="clock.elf" />
                <map mr="build_time" vaddr="0x4000_0000" perms="r"
                     setvar_vaddr="nowayos_ro_region_6_vaddr" setvar_size="nowayos_ro_region_6_size" />
            </protection_domain>
        </system>
        """)

    def repo(self, tmp, name, when):
        """A clone whose one commit was made at `when`, seconds since 1970."""
        path = os.path.join(tmp, name)
        stamp = f"@{when} +0000"
        env = {**os.environ, **self.GIT_ENV, "GIT_AUTHOR_DATE": stamp, "GIT_COMMITTER_DATE": stamp}
        for args in (["init", "-q", "-b", "main", path],
                     ["-C", path, "-c", "commit.gpgsign=false", "commit", "-q", "--allow-empty", "-m", "change"]):
            subprocess.run(["git", *args], env=env, check=True, capture_output=True)
        return path

    def build(self, tmp, *args, source=None):
        """board-system on SOURCE with `args`: (status, stderr, the bytes of
        build-time.bin or None)."""
        write(tmp, "platforms/testboard.toml", BOARD)
        out = os.path.join(tmp, "out")
        written = os.path.join(out, "build-time.bin")
        if os.path.exists(written):
            os.remove(written)
        status, _, err = run_main(self.tool, ["--board", "testboard", "--platforms", os.path.join(tmp, "platforms"),
                                              *args, write(tmp, "s.system", source or self.SOURCE),
                                              os.path.join(out, "s.system")])
        data = None
        if os.path.exists(written):
            with open(written, "rb") as handle:
                data = handle.read()
        return status, err, data

    def test_the_build_time_is_the_later_commit_time(self):
        with tempfile.TemporaryDirectory() as tmp:
            product = self.repo(tmp, "product", 1_700_000_000)
            nowayos = self.repo(tmp, "nowayos", 1_700_000_500)
            for repos in ([product, nowayos], [nowayos, product]):
                args = [arg for repo in repos for arg in ("--commit-repo", repo)]
                status, err, data = self.build(tmp, *args)
                self.assertEqual(status, 0, err)
                self.assertEqual(data, (1_700_000_500).to_bytes(8, "little"))
            # One clone: its own commit time; the same clone twice, the same.
            self.assertEqual(self.build(tmp, "--commit-repo", product)[2], (1_700_000_000).to_bytes(8, "little"))
            self.assertEqual(self.build(tmp, "--commit-repo", product, "--commit-repo", product)[2],
                             (1_700_000_000).to_bytes(8, "little"))
            # Building the same commits again gives the same bytes.
            first = self.build(tmp, "--commit-repo", product, "--commit-repo", nowayos)[2]
            self.assertEqual(self.build(tmp, "--commit-repo", product, "--commit-repo", nowayos)[2], first)

    def test_source_date_epoch_wins_when_set(self):
        with tempfile.TemporaryDirectory() as tmp:
            product = self.repo(tmp, "product", 1_700_000_000)
            status, err, data = self.build(tmp, "--source-date-epoch", "1600000000", "--commit-repo", product)
            self.assertEqual(status, 0, err)
            self.assertEqual(data, (1_600_000_000).to_bytes(8, "little"))
            # Empty means unset: the commit time.
            status, err, data = self.build(tmp, "--source-date-epoch", "", "--commit-repo", product)
            self.assertEqual((status, data), (0, (1_700_000_000).to_bytes(8, "little")), err)

    def test_a_build_time_later_than_the_clock_is_refused_naming_both_times(self):
        with tempfile.TemporaryDirectory() as tmp:
            future = int(time.time()) + 86_400
            for args, source in ((["--source-date-epoch", str(future)], "SOURCE_DATE_EPOCH"),
                                 (["--commit-repo", self.repo(tmp, "ahead", future)], "the commit time of")):
                with self.subTest(source):
                    status, err, data = self.build(tmp, *args)
                    self.assertEqual((status, data), (1, None), err)
                    self.assertIn(f"the build time, {future} s, ", err)
                    self.assertIn(source, err)
                    clock = re.search(r"later than this machine's clock, (\d+) s, (\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ)", err)
                    self.assertIsNotNone(clock, err)
                    self.assertLessEqual(abs(int(clock.group(1)) - time.time()), 60)

    def test_a_missing_or_unreadable_build_time_is_refused(self):
        with tempfile.TemporaryDirectory() as tmp:
            not_a_clone = os.path.join(tmp, "plain")
            os.makedirs(not_a_clone)
            cases = [
                ([], "no build time: set SOURCE_DATE_EPOCH"),
                (["--source-date-epoch", "soon"], "is not a count of seconds since 1970"),
                (["--source-date-epoch", "-5"], "is not a count of seconds since 1970"),
                (["--source-date-epoch", "0"], "is no floor: 0 is what a region nothing was written to holds"),
                (["--source-date-epoch", str(2**64)], "past what a device can hold"),
                (["--commit-repo", not_a_clone], f"cannot read the commit time of {not_a_clone} ("),
            ]
            for args, reason in cases:
                with self.subTest(args=args):
                    status, err, data = self.build(tmp, *args)
                    self.assertEqual((status, data), (1, None), err)
                    self.assertIn('prefill_path="build-time.bin": ', err)
                    self.assertIn(reason, err)
                    if "--commit-repo" in args:
                        self.assertIn("set SOURCE_DATE_EPOCH instead", err)
            # A system that prefills no build time needs none.
            other = mutated(self.SOURCE, 'prefill_path="build-time.bin"', 'prefill_path="screen-8x8.bin"')
            status, err, data = self.build(tmp, source=other)
            self.assertEqual((status, data), (0, None), err)
        # Without a way to find it, prefills() says so rather than guessing.
        files, problems = self.tool.prefills('<memory_region name="t" prefill_path="build-time.bin" />')
        self.assertEqual(files, {})
        self.assertIn("no build time was given", problems[0])
        files, problems = self.tool.prefills('<memory_region name="t" prefill_path="build-time.bin" />',
                                             lambda: 1_700_000_000)
        self.assertEqual((files, problems), ({"build-time.bin": (1_700_000_000).to_bytes(8, "little")}, []))

    def test_the_build_time_region_is_configuration_mapped_read_only(self):
        check = CheckSystem()
        sdf = mutated(SDF, '<memory_region name="control" size="0x1000" />',
                      '<memory_region name="control" prefill_path="build-time.bin" />')
        check.assert_fails(check.check(sdf=sdf), "rule 12: protection_domain 'consumer' map of 'control': the "
                           "region is prefilled configuration: map it read-only")


if __name__ == "__main__":
    unittest.main()

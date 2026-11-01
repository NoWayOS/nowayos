# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/serial-frame: a frame dumped as
nowayos-camera-test dumps it - an invented test pattern framed as a JPEG,
never an image of anything - saved whole, a broken line taken from the other
copy, and a frame that cannot be made whole refused."""

import base64
import importlib.machinery
import importlib.util
import os
import tempfile
import unittest
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))


def load(name):
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


TOOL = load("serial-frame")
FRAME = b"\xff\xd8\xff\xe0\x00\x04" + bytes((n * 7) & 0x7F for n in range(1000)) + b"\xff\xd9"


def dump(frame, sequence=42, copies=2, broken=()):
    """The lines nowayos-camera-test prints; `broken` names (copy, offset)
    pairs whose line another PD's output cut into."""
    lines = [f"NOWAYOS: camera-test: frame {sequence}: {len(frame)} bytes, ..."]
    for copy in range(copies):
        for offset in range(0, len(frame), 48):
            part = frame[offset:offset + 48]
            line = (f"NOWAYOS: camera-test: jpeg {sequence} {offset:06x} "
                    f"{base64.b64encode(part).decode()} {zlib.crc32(part):08x}")
            if (copy, offset) in broken:
                line = line[:40] + "NOWAYOS: usb0: port 1: camera: 10 s: 300 frames" + line[40:]
            lines.append(line)
        lines.append(f"NOWAYOS: camera-test: jpeg {sequence} end {len(frame)} {zlib.crc32(frame):08x}")
    return "\r\n".join(lines).encode("latin-1")


class SerialFrame(unittest.TestCase):

    def test_a_frame_is_saved_whole(self):
        self.assertEqual(TOOL.frame(dump(FRAME)), (42, FRAME))

    def test_a_broken_line_is_taken_from_the_other_copy(self):
        self.assertEqual(TOOL.frame(dump(FRAME, broken={(0, 96), (1, 480)}))[1], FRAME)

    def test_a_line_broken_in_both_copies_refuses_the_frame(self):
        with self.assertRaisesRegex(TOOL.FrameError, "1 lines missing or broken in both copies: 000060"):
            TOOL.frame(dump(FRAME, broken={(0, 96), (1, 96)}))

    def test_a_wrong_crc_and_no_dump_are_refused(self):
        bad = dump(FRAME, copies=1).replace(f"{zlib.crc32(FRAME):08x}".encode(), b"00000000")
        with self.assertRaisesRegex(TOOL.FrameError, "not 1008 bytes and 00000000"):
            TOOL.frame(bad)
        with self.assertRaisesRegex(TOOL.FrameError, "no frame dumped"):
            TOOL.frame(b"NOWAYOS: usb0: ready\n")
        with self.assertRaisesRegex(TOOL.FrameError, "not a JPEG"):
            TOOL.frame(dump(b"\x00" * 100))

    def test_main_writes_only_a_whole_frame(self):
        with tempfile.TemporaryDirectory() as tmp:
            log, out = os.path.join(tmp, "run.log"), os.path.join(tmp, "frame.jpg")
            with open(log, "wb") as handle:
                handle.write(b"U-Boot noise\n" + dump(FRAME))
            self.assertEqual(TOOL.main(["--log", log, "--out", out]), 0)
            with open(out, "rb") as handle:
                self.assertEqual(handle.read(), FRAME)
            os.remove(out)
            with open(log, "wb") as handle:
                handle.write(dump(FRAME, broken={(0, 0), (1, 0)}))
            self.assertEqual(TOOL.main(["--log", log, "--out", out]), 1)
            self.assertFalse(os.path.exists(out))


if __name__ == "__main__":
    unittest.main()

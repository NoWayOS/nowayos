# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/update-trial: the harness's bootloader, its copy
of the device's storage, and what it takes from a run's serial log. The
runs themselves are `make update-trial-test`'s."""
import importlib.machinery
import importlib.util
import os
import struct
import unittest
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
_loader = importlib.machinery.SourceFileLoader("update_trial", os.path.join(HERE, "update-trial"))
_spec = importlib.util.spec_from_loader("update_trial", _loader)
trial = importlib.util.module_from_spec(_spec)
_loader.exec_module(trial)
ub = trial.ub

LIST = bytes(trial.LIST_RECORD_LEN)


def copy(generation, boots, trial_slot=None, rollback=3, security=0):
    """One copy of the update state, as nowayos-update lays it out."""
    codes = {"A": 1, "B": 2}
    record = ub.header(ub.STATE_KIND, ub.STATE_PAYLOAD_LEN) + struct.pack(
        "<BBBBIII", codes[boots], codes.get(trial_slot, 0), 1, 0, rollback,
        security if trial_slot else 0, 0) + bytes(ub.KEY_ID_LEN)
    body = struct.pack("<Q", generation) + record + LIST
    return body + struct.pack("<I", zlib.crc32(body))


def report(which, data, per_line=64):
    """The stand-in's lines for a copy written."""
    pieces = [data[at:at + per_line] for at in range(0, len(data), per_line)]
    return [f"NOWAYOS: update-storage: copy {which} written: bytes {index}/{len(pieces)}: {piece.hex()}"
            for index, piece in enumerate(pieces, start=1)]


class StateTest(unittest.TestCase):
    def test_a_copy_is_as_long_as_the_stand_ins(self):
        # 8 + the state record (56) + the trust list's record (792) + 4.
        self.assertEqual(trial.COPY_LEN, 860)
        self.assertEqual(len(copy(1, "A")), trial.COPY_LEN)

    def test_a_copy_decodes_to_its_generation_and_slots(self):
        self.assertEqual(trial.decode_copy(copy(3, "A", "B", security=5)), (3, "A", "B"))
        self.assertEqual(trial.decode_copy(copy(4, "B")), (4, "B", None))

    def test_empty_torn_and_foreign_copies_are_none(self):
        good = copy(2, "A")
        self.assertIsNone(trial.decode_copy(bytes(trial.COPY_LEN)))
        self.assertIsNone(trial.decode_copy(good[:-1]))
        torn = bytearray(good)
        torn[20] ^= 1
        self.assertIsNone(trial.decode_copy(bytes(torn)))
        zero = struct.pack("<Q", 0) + good[8:-4]
        self.assertIsNone(trial.decode_copy(zero + struct.pack("<I", zlib.crc32(zero))))

    def test_the_higher_generation_counts(self):
        self.assertEqual(trial.current([copy(3, "A", "B", security=5), copy(4, "B")]), (4, "B", None))
        self.assertEqual(trial.current([copy(5, "A"), bytes(trial.COPY_LEN)]), (5, "A", None))
        with self.assertRaises(trial.HarnessError):
            trial.current([bytes(trial.COPY_LEN), bytes(trial.COPY_LEN)])


class BootloaderTest(unittest.TestCase):
    def device(self, copy_a, copy_b=None):
        device = trial.Device(copy_a, ("release-1.img", "aa"))
        if copy_b is not None:
            device.copies[1] = copy_b
        return device

    def test_the_slot_that_boots_boots(self):
        self.assertEqual(self.device(copy(1, "A")).choose(), ("A", False))
        self.assertEqual(self.device(copy(1, "A"), copy(2, "B")).choose(), ("B", False))

    def test_a_trial_boots_once(self):
        device = self.device(copy(3, "A", "B", security=5))
        self.assertEqual(device.choose(), ("B", True))
        # The same state again - the trial run ended in a reset, writing
        # nothing: the bootloader has booted the trial, so it falls back.
        self.assertEqual(device.choose(), ("A", False))
        self.assertEqual(device.choose(), ("A", False))

    def test_a_new_trial_boots_once_too(self):
        device = self.device(copy(3, "A", "B", security=5))
        self.assertEqual(device.choose(), ("B", True))
        device.copies[1] = copy(4, "A", "B", security=6)
        self.assertEqual(device.choose(), ("B", True))
        self.assertEqual(device.choose(), ("A", False))

    def test_a_trial_marked_good_boots_as_the_default(self):
        device = self.device(copy(3, "A", "B", security=5))
        self.assertEqual(device.choose(), ("B", True))
        device.copies[1] = copy(4, "B", rollback=5)
        self.assertEqual(device.choose(), ("B", False))

    def test_the_disk_is_the_stand_ins_layout(self):
        device = self.device(copy(3, "A", "B", security=5), copy(2, "A"))
        disk = device.disk("B")
        self.assertEqual(len(disk), 16 + 2 * trial.COPY_LEN)
        self.assertEqual(disk[:16], b"BDSK" + struct.pack("<III", 1, 2, 0))
        self.assertEqual(disk[16:16 + trial.COPY_LEN], device.copies[0])
        self.assertEqual(disk[16 + trial.COPY_LEN:], device.copies[1])


class SerialReportTest(unittest.TestCase):
    def test_the_last_whole_copy_written_is_taken(self):
        device = trial.Device(copy(1, "A"), ("release-1.img", "aa"))
        older, newer = copy(2, "A"), copy(4, "B", rollback=5)
        log = "\n".join(["noise"] + report("B", older) + ["more noise"] + report("B", newer)) + "\n"
        self.assertEqual(device.take(log, {}), ["B"])
        self.assertEqual(device.copies[1], newer)
        self.assertEqual(device.copies[0], copy(1, "A"))

    def test_a_copy_cut_short_is_not_taken(self):
        device = trial.Device(copy(1, "A"), ("release-1.img", "aa"))
        lines = report("A", copy(3, "A", "B", security=5))
        self.assertEqual(device.take("\n".join(lines[:-1]) + "\n", {}), [])
        self.assertEqual(device.copies[0], copy(1, "A"))

    def test_a_copy_of_the_wrong_length_fails(self):
        device = trial.Device(copy(1, "A"), ("release-1.img", "aa"))
        with self.assertRaises(trial.HarnessError):
            device.take("\n".join(report("A", bytes(10))) + "\n", {})

    def test_a_slot_written_is_matched_to_the_image_with_its_sha256(self):
        device = trial.Device(copy(1, "A"), ("release-1.img", "aa"))
        digest = "ab" * 32
        line = f"NOWAYOS: update-storage: slot B holds 3141060 bytes written this run, SHA-256 {digest}\r\n"
        device.take(line, {"release-1.img": "aa", "release-2.img": digest})
        self.assertEqual(device.slots["B"], ("release-2.img", digest))
        other = "cd" * 32
        device.take(line.replace(digest, other), {"release-2.img": digest})
        self.assertEqual(device.slots["B"], (None, other))
        self.assertEqual(device.slots["A"], ("release-1.img", "aa"))

    def test_a_copy_of_the_device_is_its_own(self):
        device = trial.Device(copy(3, "A", "B", security=5), ("release-1.img", "aa"))
        twin = device.copy()
        self.assertEqual(twin.choose(), ("B", True))
        self.assertEqual(device.choose(), ("B", True))
        twin.copies[1] = copy(4, "B")
        self.assertIsNone(trial.decode_copy(device.copies[1]))


if __name__ == "__main__":
    unittest.main()

# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/icon-masks: its path reader, its strokes and its
refusals. `make atlases-check` checks what it draws from the pinned icons.

Python 3.11 or later, standard library only.
"""

import importlib.machinery
import importlib.util
import os
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


icon_masks = load("icon-masks")

HEAD = ('<svg xmlns="http://www.w3.org/2000/svg" width="24" height="24" viewBox="0 0 24 24" '
        'fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" '
        'stroke-linejoin="round">')


def svg(body):
    return f"{HEAD}{body}</svg>"


class PathTest(unittest.TestCase):
    def test_relative_and_absolute_commands_reach_the_same_points(self):
        absolute = icon_masks.path_lines("M2 2L10 2H12V6Z")
        relative = icon_masks.path_lines("m2 2l8 0h2v4z")
        self.assertEqual(absolute, relative)
        self.assertEqual(absolute[0], [(2, 2), (10, 2), (12, 2), (12, 6), (2, 2)])

    def test_implicit_repeats_and_packed_numbers(self):
        # After M, numbers are lines; ".5.5" is two numbers; arc flags pack.
        lines = icon_masks.path_lines("M1 1 2 2 3 3l.5.5")
        self.assertEqual(lines[0], [(1, 1), (2, 2), (3, 3), (3.5, 3.5)])
        arc = icon_masks.path_lines("M2 12a10 10 0 0120 0")[0]
        self.assertEqual(arc[-1], (22.0, 12.0))
        # A half circle of radius 10 rises to y = 2 at its middle.
        self.assertAlmostEqual(min(y for _, y in arc), 2.0, places=1)

    def test_a_curve_ends_where_it_says(self):
        line = icon_masks.path_lines("M0 0C0 10 10 10 10 0")[0]
        self.assertEqual(line[-1], (10.0, 0.0))
        self.assertTrue(all(0 <= y <= 7.5 for _, y in line))

    def test_commands_it_does_not_draw_are_refused(self):
        with self.assertRaises(icon_masks.InputError):
            icon_masks.path_lines("M0 0X1 1")
        with self.assertRaises(icon_masks.InputError):
            icon_masks.path_lines("1 1")


class MaskTest(unittest.TestCase):
    def test_a_dot_is_a_round_cap_two_units_across(self):
        segments = icon_masks.svg_segments(svg('<path d="M12 12h.01" />'))
        mask = icon_masks.mask(segments, 4)
        self.assertEqual(len(mask), 24 * 24)
        # The pixels around (12, 12) are inked; far ones are not.
        self.assertGreater(mask[12 * 24 + 12], 200)
        self.assertEqual(mask[0], 0)
        inked = sum(mask) / 255
        # A disc of radius 1: about pi pixels of ink.
        self.assertAlmostEqual(inked, 3.14, delta=0.3)

    def test_a_circle_and_a_rounded_rect_draw(self):
        for body in ('<circle cx="12" cy="12" r="8" />', '<rect x="4" y="4" width="16" height="16" rx="2" />'):
            mask = icon_masks.mask(icon_masks.svg_segments(svg(body)), 8)
            self.assertEqual(len(mask), 48 * 48)
            self.assertEqual(mask[24 * 48 + 24], 0, body)
            self.assertGreater(sum(mask), 0, body)

    def test_every_quarter_scale_is_a_whole_number_of_pixels(self):
        segments = icon_masks.svg_segments(svg('<circle cx="12" cy="12" r="8" />'))
        for quarters, side in [(5, 30), (6, 36), (10, 60), (16, 96)]:
            mask = icon_masks.mask(segments, quarters)
            self.assertEqual(len(mask), side * side, quarters)
            # The ring's ink grows with the square of the scale.
            self.assertAlmostEqual(sum(mask) / 255 / side ** 2,
                                   sum(icon_masks.mask(segments, 4)) / 255 / 24 ** 2, delta=0.02)

    def test_what_it_does_not_draw_is_refused(self):
        for body in ('<path d="M0 0L1 1" fill="red" />', '<path d="M0 0L1 1" transform="scale(2)" />',
                     '<text>x</text>', '<path d="M0 0L1 1" stroke-width="3" />'):
            with self.assertRaises(icon_masks.InputError, msg=body):
                icon_masks.svg_segments(svg(body))
        with self.assertRaises(icon_masks.InputError):
            icon_masks.svg_segments(HEAD.replace('stroke-width="2"', 'stroke-width="1.5"') + "</svg>")


class PinTest(unittest.TestCase):
    def test_a_changed_or_unpinned_file_is_refused(self):
        with tempfile.TemporaryDirectory() as crate:
            os.makedirs(os.path.join(crate, "lucide"))
            body = svg('<path d="M4 4h16" />').encode()
            with open(os.path.join(crate, "lucide", "bar.svg"), "wb") as handle:
                handle.write(body)
            pin = __import__("hashlib").sha256(body).hexdigest()
            with open(os.path.join(crate, "icons.toml"), "w", encoding="utf-8") as handle:
                handle.write(f'[[icon]]\nname = "bar"\nsha256 = "{pin}"\n')
            files = icon_masks.generate(crate)
            sides = [6 * quarters for quarters in range(4, 17)]
            self.assertEqual(sorted(files), sorted([f"bar_{side}.coverage" for side in sides] + ["mod.rs"]))
            self.assertIn(b"pub static BAR: Icon", files["mod.rs"])
            with open(os.path.join(crate, "lucide", "extra.svg"), "wb") as handle:
                handle.write(body)
            with self.assertRaises(icon_masks.InputError):
                icon_masks.generate(crate)
            os.remove(os.path.join(crate, "lucide", "extra.svg"))
            with open(os.path.join(crate, "lucide", "bar.svg"), "ab") as handle:
                handle.write(b" ")
            with self.assertRaises(icon_masks.InputError):
                icon_masks.generate(crate)


if __name__ == "__main__":
    unittest.main()

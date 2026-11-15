# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/dev-window: what a save sets off, and what it shows of a failure."""
import importlib.machinery
import importlib.util
import os
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
_loader = importlib.machinery.SourceFileLoader("dev_window", os.path.join(HERE, "dev-window"))
_spec = importlib.util.spec_from_loader("dev_window", _loader)
dev_window = importlib.util.module_from_spec(_spec)
_loader.exec_module(dev_window)


def write(root, path, text="x"):
    full = os.path.join(root, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w", encoding="utf-8") as handle:
        handle.write(text)
    return full


class Watching(unittest.TestCase):
    def test_only_source_is_watched_and_build_output_never(self):
        with tempfile.TemporaryDirectory() as root:
            screen = write(root, "app/tsx/welcome.tsx")
            logic = write(root, "app/src/lib.rs")
            write(root, "app/notes.md")
            write(root, "target/debug/build.rs")
            write(root, "app/.hidden/x.rs")
            self.assertEqual(sorted(dev_window.stamps([root])), sorted([screen, logic]))

    def test_a_save_is_seen_by_what_the_file_holds_not_by_its_time(self):
        with tempfile.TemporaryDirectory() as root:
            screen = write(root, "app/tsx/welcome.tsx", "one")
            before = dev_window.stamps([root])
            times = os.stat(screen)
            write(root, "app/tsx/welcome.tsx", "two")
            # As a bind mount in a container may show it: the old time still.
            os.utime(screen, ns=(times.st_atime_ns, times.st_mtime_ns))
            self.assertEqual(dev_window.changed(before, dev_window.stamps([root])), [screen])
            # Saved again with nothing changed is no change.
            again = dev_window.stamps([root])
            write(root, "app/tsx/welcome.tsx", "two")
            self.assertEqual(dev_window.changed(again, dev_window.stamps([root])), [])

    def test_a_save_an_addition_and_a_removal_are_each_a_change(self):
        before = {"a.rs": 1, "b.rs": 1, "c.rs": 1}
        after = {"a.rs": 1, "b.rs": 2, "d.rs": 1}
        self.assertEqual(dev_window.changed(before, after), ["b.rs", "c.rs", "d.rs"])
        self.assertEqual(dev_window.changed(before, dict(before)), [])


class Steps(unittest.TestCase):
    def test_a_screens_crate_is_the_folder_holding_its_tsx_folder(self):
        self.assertEqual(dev_window.screens_crate("showcase/visitor/tsx/welcome.tsx"), "showcase/visitor")
        self.assertEqual(dev_window.screens_crate("showcase/visitor/tsx/parts/row.tsx"), "showcase/visitor")
        self.assertEqual(dev_window.screens_crate("tsx/welcome.tsx"), os.sep)
        self.assertIsNone(dev_window.screens_crate("showcase/visitor/src/welcome.tsx"))

    def test_each_saved_screen_is_checked_then_each_crate_converted_once(self):
        with tempfile.TemporaryDirectory() as root:
            one = write(root, "a/tsx/one.tsx")
            two = write(root, "a/tsx/two.tsx")
            three = write(root, "b/tsx/three.tsx")
            logic = write(root, "a/src/lib.rs")
            gone = os.path.join(root, "a/tsx/gone.tsx")
            commands = dev_window.steps_for(
                [one, two, three, logic, gone], "sc check {file}", "screens --bless --screens {crate}")
            self.assertEqual(commands, [
                ["sc", "check", one],
                ["sc", "check", two],
                ["sc", "check", three],
                ["screens", "--bless", "--screens", os.path.join(root, "a")],
                ["screens", "--bless", "--screens", os.path.join(root, "b")],
            ])
            # The check may name the screen's crate too: for its list of pictures.
            self.assertEqual(
                dev_window.steps_for([one], "sc check --pictures {crate}/pictures.names {file}", ""),
                [["sc", "check", "--pictures", os.path.join(root, "a", "pictures.names"), one]])
            # Logic alone sets off no step: only the build.
            self.assertEqual(dev_window.steps_for([logic], "sc check {file}", "screens {crate}"), [])
            # A product with no steps of its own.
            self.assertEqual(dev_window.steps_for([one], "", ""), [])

    def test_a_step_that_fails_gives_its_output(self):
        ok, output = dev_window.run(["python3", "-c", "import sys; print('error: no'); sys.exit(3)"])
        self.assertFalse(ok)
        self.assertEqual(output, "error: no\n")
        ok, output = dev_window.run(["python3", "-c", "print('fine')"])
        self.assertTrue(ok)
        ok, output = dev_window.run(["/no/such/program"])
        self.assertFalse(ok)
        self.assertIn("/no/such/program", output)


class Notices(unittest.TestCase):
    def test_a_long_failure_keeps_its_end_and_fits(self):
        short = "error: one line\n"
        self.assertEqual(dev_window.trim(short), short)
        long = "".join(f"line {n}\n" for n in range(5000)) + "error: the last line\n"
        kept = dev_window.trim(long)
        self.assertLessEqual(len(kept.encode()), dev_window.MAX_NOTICE)
        self.assertTrue(kept.startswith("..."))
        self.assertTrue(kept.endswith("error: the last line\n"))


if __name__ == "__main__":
    unittest.main()

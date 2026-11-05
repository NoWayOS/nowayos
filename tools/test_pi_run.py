# SPDX-License-Identifier: MIT OR Apache-2.0
"""Tests for tools/pi-run: the YMODEM framing, the PM watchdog's
checks and writes, the commands `uboot` allows, and whole sessions against a
stand-in for the board on a pseudo-terminal.

The stand-in plays what the Pi 5 printed on 2026-10-07 (the recorded
sessions of the board run, trimmed, with the board's and the card's serial
numbers removed): the bootloader's reset report, U-Boot's banner, countdown
and prompt, and the recorded output of ipc-demo and supervisor-test. At its
prompt it answers loady as a YMODEM receiver, unzip, crc32, md.l, mw.l and
go as U-Boot did, Ctrl-C as abandoning a line; its watchdog resets it when
the time written to WDOG runs out, once RSTC holds the value run by hand
(0x5a000222). It can also stop at its prompt by itself (lab mode), or be
part-way through a boot when a run starts.

Run: python3 -m unittest discover -s tools -p 'test_*.py'   (make tools-test)
Python 3.11 or later, standard library only.
"""

import contextlib
import fcntl
import gzip
import importlib.machinery
import importlib.util
import io
import os
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time
import tty
import unittest
import zlib

HERE = os.path.dirname(os.path.abspath(__file__))
SYSTEMS = os.path.join(HERE, os.pardir, "systems")


def load(name):
    """Import an extensionless script in this directory as a module."""
    loader = importlib.machinery.SourceFileLoader(name.replace("-", "_"), os.path.join(HERE, name))
    spec = importlib.util.spec_from_loader(loader.name, loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


pi_run = load("pi-run")

# The bootloader's reset report and U-Boot's banner, as the board printed
# them at each boot on 2026-10-07 (run3), trimmed; serial numbers zeroed.
BOOT = """
  0.77 RPi: BOOTSYS release VERSION:5855b10b DATE: 2025/06/13 TIME: 10:39:26
  0.78 BOOTMODE: 0x06 partition 0 build-ts BUILD_TIMESTAMP=1749807566 serial 00000000 boardrev e04171 stc 783704
  0.79 AON_RESET: 00000003 PM_RSTS {rsts:08x}
  0.79 POWER_OFF_ON_HALT: 0 WAIT_FOR_POWER_BUTTON 0 power-on-reset {power_on}
  0.80 RP1_BOOT chip ID: 0x20001927
  4.02 RPi: BOOTLOADER release VERSION:5855b10b DATE: 2025/06/13 TIME: 10:39:26
  4.56 Boot mode: SD (01) order f4
{fb_line}  5.76 Loading 'u-boot.bin' to 0x00000000 offset 0x200000
  5.90 MESS:00:00:05.902217:0: Starting OS 5902 ms

NOTICE:  BL31: v2.6(release):v2.6-240-gfc45bc492
NOTICE:  BL31: Built : 12:55:13, Dec  4 2024


U-Boot 2026.07 (Jul 07 2026 - 00:00:00 +0000)

DRAM:  16 GiB
RPI 5 Model B (0xe04171)
Core:  30 devices, 13 uclasses, devicetree: board
In:    serial,usbkbd
Out:   serial,vidconsole
Err:   serial,vidconsole
Net:   No ethernet found.
starting USB...
No USB controllers found
""".replace("\n", "\r\n")
COUNTDOWN = "\x1b[2K\rHit any key to stop autoboot: 3"
# U-Boot built with keyed autoboot: only the stop word stops it.
KEYED_COUNTDOWN = "Type stopboot within 3 seconds to stop autoboot\r\n"
PROMPT = "U-Boot> "

# What ipc-demo printed after go on the board (run3, nowayos 7e7a514),
# the loader's region lines left out.
IPC_DEMO = """\
## Starting application at 0x10000000 ...
LDR|INFO|CPU0: CurrentEL=EL2
LDR|INFO: loader for seL4 starting
LDR|INFO|CPU0: jumping to kernel
Bootstrapping kernel
Available phys memory regions: 1
  [80000..3fc00000)
Booting all finished, dropped to user space
INFO  [sel4_capdl_initializer::initialize] Starting CapDL initializer
INFO  [sel4_capdl_initializer::initialize] Starting threads
MON|INFO: Microkit Monitor started!
NOWAYOS: ipc-producer: starting; ring of 4 slots of 32 bytes, messages region read-write here
NOWAYOS: ipc-producer: sent records 0..=3
NOWAYOS: ipc-consumer: starting; ring of 4 slots of 32 bytes, messages region read-only here
NOWAYOS: ipc-consumer: record 0: PointerMove { x: 120, y: 80 }
NOWAYOS: ipc-consumer: record 1: PointerButton { button: 0, down: true }
NOWAYOS: ipc-consumer: record 2: PointerButton { button: 0, down: false }
NOWAYOS: ipc-consumer: record 3: KeyDown(KeyCode(43))
NOWAYOS: ipc-producer: sent records 4..=7
NOWAYOS: ipc-consumer: record 4: KeyUp(KeyCode(43))
NOWAYOS: ipc-consumer: record 5 rejected: malformed: invalid value in field `tag`
NOWAYOS: ipc-consumer: record 6: KeyDown(KeyCode(40))
NOWAYOS: ipc-consumer: record 7: KeyUp(KeyCode(40))
NOWAYOS: ipc-producer: sent records 8..=10
NOWAYOS: ipc-producer: all 11 records sent
NOWAYOS: ipc-consumer: record 8: Touch { id: 0, x: 300, y: 200, phase: Start }
NOWAYOS: ipc-consumer: record 9: Touch { id: 0, x: 300, y: 200, phase: End }
NOWAYOS: ipc-consumer: end of sequence: 10 records announced, 9 accepted, 1 rejected
"""

# What supervisor-test printed at each of its boots (run2): it resets the
# board itself, and its expect file asks for the second boot.
SUPERVISOR_TEST = """\
## Starting application at 0x10000000 ...
LDR|INFO: loader for seL4 starting
MON|INFO: Microkit Monitor started!
NOWAYOS: supervisor: starting; children faulter (0) and trigger (1)
NOWAYOS: isolation-probe: faulter: no marker - this boot's memory is fresh
NOWAYOS: isolation-probe: faulter wrote its marker; waiting for the trigger
NOWAYOS: isolation-probe: trigger notifying faulter
NOWAYOS: isolation-probe: faulter triggered
NOWAYOS: isolation-probe: faulter read 0x00000000 from its read-only page at 0x40001000
NOWAYOS: isolation-probe: faulter writing to its read-only page at 0x40001000
NOWAYOS: supervisor: faulter stopped: data write at 0x40001000: permission fault (the mapping does not allow it)
NOWAYOS: supervisor: resetting the board
"""

# What each image does after go: its output, then "hang" (until the
# watchdog) or "reset" (the board, itself, at once).
SCRIPTS = {
    "ipc-demo": (IPC_DEMO, "hang"),
    "supervisor-test": (SUPERVISOR_TEST, "reset"),
    "silent": ("## Starting application at 0x10000000 ...\n", "hang"),
    # A system that reads keys from the debug UART: the stand-in board
    # answers each byte it receives once this runs (FakeBoard.keys).
    "keys": ("## Starting application at 0x10000000 ...\nNOWAYOS: ready\n", "hang"),
    "forbidden": (IPC_DEMO.replace("record 6", "MON|ERROR: received message 0x6"), "hang"),
}


def make_image(directory, script, size=40 * 1024):
    """A stand-in image: a first line naming its script, then bytes that do
    not compress, so that it takes more than 256 YMODEM blocks when large."""
    path = os.path.join(directory, f"{script}-{size}.img")
    data = f"pi-run test image: {script}\n".encode()
    seed = zlib.crc32(data)
    blocks = bytearray()
    while len(data) + len(blocks) < size:
        seed = (seed * 1103515245 + 12345) & 0xFFFFFFFF
        blocks += struct.pack("<I", seed)
    with open(path, "wb") as handle:
        handle.write(data + bytes(blocks[:size - len(data)]))
    return path


def receive_ymodem(packets):
    """Decode a YMODEM batch from its packets, checking each as a receiver
    does: framing, block numbers, complements and CRCs. Return (name, data)."""
    first = packets[0]
    assert first[0] == pi_run.SOH and first[1] == 0 and first[2] == 0xFF, "block 0"
    payload = first[3:3 + 128]
    assert struct.unpack(">H", first[131:133])[0] == pi_run.crc16(payload)
    name, size = payload.split(b"\0")[:2]
    data = bytearray()
    for number, packet in enumerate(packets[1:-1], start=1):
        length = 1024 if packet[0] == pi_run.STX else 128
        assert len(packet) == 3 + length + 2
        assert packet[1] == number & 0xFF and packet[2] == 0xFF - packet[1], f"block {number}"
        body = packet[3:3 + length]
        assert struct.unpack(">H", packet[3 + length:])[0] == pi_run.crc16(body), f"CRC {number}"
        data += body
    last = packets[-1]
    assert last[:3] == bytes([pi_run.SOH, 0, 0xFF]) and last[3:131] == bytes(128)
    return name.decode(), bytes(data[:int(size)])


class FakeBoard:
    """The Pi 5 and its U-Boot, as recorded, on a pseudo-terminal."""

    def __init__(self, *, registers=None, cancel_at_block=None, unzip_short=False, crc_wrong=False,
                 watchdog_works=True, at_prompt=True, answers=True, echo_wrong=False, readback_wrong=False,
                 silent_after_cancel=False, power_cycle_after=None, fb=(1920, 1080, 7680, 0x3F40_0000, 32),
                 echo_wrong_after=None, ignore_interrupt_on_line=False, lab_mode=False, mid_boot_at=None,
                 watchdog_at_go=None, go_returns_after=None, crossing=b"", garbled_reboot_after=None,
                 silent_reset=None, go_delay=0.0, go_return_text="## Application terminated, rc = 0x0\r\n",
                 crossing_delay=0.0, go_echo_delay=0.0, keyed=False):
        self.master, self.slave = os.openpty()
        tty.setraw(self.slave)
        self.device = os.ttyname(self.slave)
        self.registers = registers
        self.cancel_at_block = cancel_at_block
        self.unzip_short = unzip_short
        self.crc_wrong = crc_wrong
        self.watchdog_works = watchdog_works
        self.answers = answers
        self.echo_wrong = echo_wrong
        self.echo_wrong_after = echo_wrong_after  # mangle the echo of the character after this text
        self.ignore_interrupt_on_line = ignore_interrupt_on_line
        self.lab_mode = lab_mode
        # U-Boot's keyed autoboot: in a countdown, only the stop word ending
        # what arrived stops it; any other key, Ctrl-C included, is ignored.
        self.keyed = keyed
        self.presskey = b""
        self.stopped_by = []  # what stopped each countdown
        self.mid_boot_at = mid_boot_at  # start part-way through a boot; its countdown this many s in
        self.watchdog_at_go = watchdog_at_go  # the watchdog resets the board this many s after go
        # This many s after go: `go` returns to U-Boot's prompt, with no boot,
        # and `crossing` - keys in flight - typed at it; or a boot whose
        # bootloader banner is garbled, then U-Boot's banner, countdown and
        # (lab mode) prompt.
        self.go_returns_after = go_returns_after
        self.go_return_text = go_return_text  # what `go` prints as it returns, before the prompt
        self.crossing = crossing
        # How long after the prompt the crossing keys are echoed: the board
        # reads nothing meanwhile, so they still come before what follows.
        self.crossing_delay = crossing_delay
        self.garbled_reboot_after = garbled_reboot_after
        # A reset after the first is silent this many s, as the bootloader is
        # before its first line: what arrives meanwhile is kept, not acted on.
        self.silent_reset = silent_reset
        self.during_reset = bytearray()
        self.go_delay = go_delay  # the loader and kernel starting before the image's first line
        # How long U-Boot takes to echo go's Enter, reading nothing meanwhile;
        # go_seen is set when it has the Enter.
        self.go_echo_delay = go_echo_delay
        self.go_seen = threading.Event()
        self.running_since = None
        self.started = None
        self.readback_wrong = readback_wrong
        self.silent_after_cancel = silent_after_cancel
        self.power_cycle_after = power_cycle_after
        self.fb = fb  # the framebuffer the firmware sets up: (width, height, stride, base, depth), or none
        self.power_cycle_at = None
        self.controls = []
        self.rstc = 0x202
        self.state = "prompt" if at_prompt else "off"
        if mid_boot_at is not None:
            self.state = "booting"
        self.boots = 0
        self.typed = ""
        self.commands = []
        self.loads = []
        self.writes = []
        self.staged = b""
        self.memory = b""
        self.wdog = 0
        self.deadline = None
        self.rsts = 0x1000  # the first boot is a power-on
        self.inbox = bytearray()
        self.keys = bytearray()  # what reached a running image's debug UART
        self.stop = threading.Event()
        self.thread = threading.Thread(target=self.run, daemon=True)

    def __enter__(self):
        self.started = time.monotonic()
        self.thread.start()
        return self

    def __exit__(self, *exc):
        self.stop.set()
        self.thread.join(10)
        os.close(self.master)
        os.close(self.slave)

    # -- the line -----------------------------------------------------------

    def send(self, text):
        data = text.encode("latin-1") if isinstance(text, str) else text
        while data:
            try:
                data = data[os.write(self.master, data):]
            except BlockingIOError:
                time.sleep(0.001)

    def pull(self, timeout):
        if not self.inbox:
            ready, _, _ = select.select([self.master], [], [], timeout)
            if ready:
                try:
                    self.inbox += os.read(self.master, 65536)
                except OSError:
                    return None
        if not self.inbox:
            return None
        return self.inbox.pop(0)

    def take(self, count, timeout=5):
        data = bytearray()
        while len(data) < count:
            byte = self.pull(timeout)
            if byte is None:
                return None
            data.append(byte)
        return bytes(data)

    # -- the board ----------------------------------------------------------

    def boot(self, power_on=False):
        if self.silent_reset is not None and self.boots:
            end = time.monotonic() + self.silent_reset
            while time.monotonic() < end:
                byte = self.pull(0.01)
                if byte is not None:
                    self.during_reset.append(byte)
        self.boots += 1
        self.running_since = None
        self.deadline, self.wdog, self.rstc, self.power_cycle_at = None, 0, 0x202, None
        if power_on:
            self.rsts = 0x1000
        rsts, power_on = self.rsts, 1 if self.boots == 1 or power_on else 0
        fb_line = ""
        if self.fb:
            width, height, stride, base, _ = self.fb
            fb_line = (f"  5.26 MESS:00:00:05.263550:0: FB0 disp 0 max-fb 1 {width}x{height} stride {stride} "
                       f"base {base:#x}\r\n")
        else:
            fb_line = "  5.12 MESS:00:00:05.120412:0: Select resolution HDMI0/2 hotplug 0 max_mode 2\r\n"
        self.send(BOOT.format(rsts=rsts, power_on=power_on, fb_line=fb_line))
        self.rsts = 0  # read as 0 once the bootloader has reported it
        # The bootloader runs before U-Boot reads its UART: whatever arrives
        # meanwhile - a key sent as the board resets - is lost.
        time.sleep(0.2)
        self.inbox.clear()
        while select.select([self.master], [], [], 0)[0]:
            try:
                os.read(self.master, 65536)
            except OSError:
                break
        self.send(self.countdown())
        if self.lab_mode:
            # The saved environment's bootcmd only prints a line: the board
            # stops at its prompt, whether or not a key was pressed.
            self.send(("" if self.keyed else "\x1b[2K\rHit any key to stop autoboot: 0 \r\n") +
                      "Lab mode: the board waits at this prompt for the runner\r\n" + PROMPT)
            self.state = "prompt"
            return
        self.state, self.countdown_end = "countdown", time.monotonic() + 3

    def countdown(self):
        self.presskey = b""
        return KEYED_COUNTDOWN if self.keyed else COUNTDOWN

    def reset(self):
        self.send("resetting ...\r\n")
        self.boot()

    def run(self):
        while not self.stop.is_set():
            if self.deadline is not None and time.monotonic() >= self.deadline:
                self.boot()
                continue
            if self.power_cycle_at is not None and time.monotonic() >= self.power_cycle_at:
                self.boot(power_on=True)  # a person cut the power and restored it
                continue
            if self.state == "booting" and time.monotonic() >= self.started + self.mid_boot_at:
                self.send("\r\nU-Boot 2026.07 (Jul 07 2026 - 00:00:00 +0000)\r\n\r\n" + self.countdown())
                self.state, self.countdown_end = "countdown", time.monotonic() + 3
            if self.state == "countdown" and time.monotonic() > self.countdown_end:
                self.send("\r\nU-Boot> ")
                self.state = "prompt"  # (autoboot left out: no card)
            if self.state == "running" and self.running_since is not None:
                since = time.monotonic() - self.running_since
                if self.go_returns_after is not None and since >= self.go_returns_after:
                    self.running_since = None
                    self.send(self.go_return_text + PROMPT)
                    self.state = "prompt"
                    time.sleep(self.crossing_delay)
                    for byte in self.crossing:
                        self.key(byte)
                    continue
                if self.garbled_reboot_after is not None and since >= self.garbled_reboot_after:
                    self.running_since = None
                    self.send("  0.77 RPx: BXOTSYS relXase VERSION:5855b10b\r\n\r\nU-Boot 2026.07\r\n"
                              "\x1b[2K\rHit any key to stop autoboot: 0 \r\n"
                              "Lab mode: the board waits at this prompt for the runner\r\n" + PROMPT)
                    self.state = "prompt"
                    continue
            byte = self.pull(0.02)
            if byte is None or not self.answers:
                continue
            if self.state == "running":
                self.keys.append(byte)
                self.send(f"NOWAYOS: received {byte:#04x}\r\n")
            elif self.state == "countdown" and self.keyed:
                self.presskey += bytes([byte])
                if self.presskey.endswith(pi_run.STOP_WORD.encode()):
                    self.stopped_by.append(self.presskey)
                    self.send(PROMPT)
                    self.state = "prompt"
            elif self.state == "countdown":
                self.stopped_by.append(bytes([byte]))
                self.send("\x1b[2K\rHit any key to stop autoboot: 0 \r\n" + PROMPT)
                self.state = "prompt"
            elif self.state == "prompt":
                self.key(byte)

    def key(self, byte):
        if byte == 0x03:  # Ctrl-C abandons the line
            self.controls.append(byte)
            if self.ignore_interrupt_on_line and self.typed:
                return
            self.typed = ""
            self.send("<INTERRUPT>\r\n" + PROMPT)
        elif byte == 0x0D:
            line, self.typed = self.typed.strip(), ""
            if line.startswith("go "):
                self.go_seen.set()
                time.sleep(self.go_echo_delay)
            self.send("\r\n")
            if line:
                self.commands.append(line)
                if self.command(line) == "prompt":
                    self.send(PROMPT)
            else:
                self.send(PROMPT)
        elif 0x20 <= byte < 0x7F:
            self.typed += chr(byte)
            mangle = self.echo_wrong and len(self.typed) == 1
            if self.echo_wrong_after and self.typed[:-1] == self.echo_wrong_after:
                mangle = True
            self.send(b"X" if mangle else bytes([byte]))
        else:
            self.controls.append(byte)

    def command(self, line):
        words = line.split()
        verb = words[0]
        if verb == "reset":
            self.reset()
            return "booting"
        if verb == "loady":
            self.send(f"## Ready for binary (ymodem) download to {words[1]} at 115200 bps...\r\n")
            received = self.receive()
            if received is None:
                if self.silent_after_cancel:
                    self.answers = False
                    return "silent"
                self.send("\r\n## Binary (ymodem) download aborted\r\n")
                return "prompt"
            self.staged = received
            self.send(f"## Total Size      = 0x{len(received):08x} = {len(received)} Bytes\r\n"
                      f"## Start Addr      = {words[1]}\r\n")
            return "prompt"
        if verb == "unzip":
            self.memory = gzip.decompress(self.staged)
            size = len(self.memory) - (1 if self.unzip_short else 0)
            self.send(f"Uncompressed size: {size} = 0x{size:X}\r\n")
            return "prompt"
        if verb == "crc32":
            length = int(words[2], 16)
            crc = zlib.crc32(self.memory[:length]) ^ (1 if self.crc_wrong else 0)
            # In two writes, the first ending in "==> ": a read can end there.
            self.send(f"crc32 for {int(words[1], 16):08x} ... {int(words[1], 16) + length - 1:08x} ==> ")
            time.sleep(0.3)
            self.send(f"{crc:08x}\r\n")
            return "prompt"
        if verb == "md.l":
            address = int(words[1], 16)
            left = self.wdog
            if self.deadline is not None:
                left = max(1, int((self.deadline - time.monotonic()) * 65536))
            values = self.registers or (self.rstc, self.rsts, left)
            assert address == pi_run.RSTC
            self.send(f"{address:x}: " + " ".join(f"{v:08x}" for v in values) + "           ............\r\n")
            return "prompt"
        if verb == "mw.l":
            address, value = int(words[1], 16), int(words[2], 16)
            self.writes.append((address, value))
            if address == pi_run.WDOG:
                self.wdog = value & 0x00FFFFFF
            if address == pi_run.RSTC:
                if not self.readback_wrong:
                    self.rstc = value & 0x00FFFFFF
                if value == 0x5A000222 and self.wdog and self.watchdog_works:
                    self.deadline = time.monotonic() + self.wdog / 65536
            return "prompt"
        if verb == "go":
            time.sleep(self.go_delay)
            self.running_since = time.monotonic()
            self.loads.append(self.memory.split(b"\n", 1)[0].decode("latin-1"))
            script = self.memory.split(b"\n", 1)[0].decode().rsplit(": ", 1)[-1]
            output, ending = SCRIPTS[script]
            self.send(output.replace("\n", "\r\n"))
            if self.power_cycle_after is not None:
                self.power_cycle_at = time.monotonic() + self.power_cycle_after
            if self.watchdog_at_go is not None:
                self.deadline = time.monotonic() + self.watchdog_at_go
            if ending == "reset":
                time.sleep(0.1)
                self.boot()
            else:
                self.state = "running"
            return "running"
        if verb == "version":
            self.send("U-Boot 2026.07 (Jul 07 2026 - 00:00:00 +0000)\r\n")
            return "prompt"
        if verb == "bdinfo":
            self.send("boot_params = 0x0000000000000100\r\nbaudrate    = 115200 bps\r\n")
            if self.fb:
                width, height, _, base, depth = self.fb
                self.send(f"Video       = fb active\r\nFB base     = 0x{base:016x}\r\n"
                          f"FB size     = {width}x{height}x{depth}\r\n")
            self.send("devicetree  = board\r\n")
            return "prompt"
        self.send(f"Unknown command '{verb}' - try 'help'\r\n")
        return "prompt"

    def receive(self):
        """loady's side of a YMODEM batch; None if it cancelled."""
        self.send("C")
        blocks, data, size = 0, bytearray(), None
        while True:
            first = self.pull(10)
            if first is None:
                return None
            if first == pi_run.EOT:
                self.send(bytes([pi_run.ACK]))
                self.send("C")
                closing = self.take(133)
                self.send(bytes([pi_run.ACK]))
                assert closing[:3] == bytes([pi_run.SOH, 0, 0xFF])
                return bytes(data[:size])
            if first not in (pi_run.SOH, pi_run.STX):
                continue
            length = 1024 if first == pi_run.STX else 128
            rest = self.take(2 + length + 2)
            body = rest[2:2 + length]
            if struct.unpack(">H", rest[2 + length:])[0] != pi_run.crc16(body) or rest[1] != 0xFF - rest[0]:
                self.send(bytes([pi_run.NAK]))
                continue
            if size is None:
                size = int(body.split(b"\0")[1])
                self.send(bytes([pi_run.ACK]))
                self.send("C")
                continue
            blocks += 1
            if self.cancel_at_block == blocks:
                self.send(bytes([pi_run.CAN, pi_run.CAN]))
                return None
            data += body
            self.send(bytes([pi_run.ACK]))


class Framing(unittest.TestCase):
    """YMODEM's packets and CRC."""

    def test_crc16_is_xmodem_s(self):
        self.assertEqual(pi_run.crc16(b"123456789"), 0x31C3)
        self.assertEqual(pi_run.crc16(b""), 0)

    def test_a_batch_decodes_to_the_file_with_its_name_and_size(self):
        data = bytes(range(256)) * 1300  # 332,800 bytes: block numbers wrap past 255
        packets = [packet for _kind, _number, _payload, packet in pi_run.ymodem_packets("x/hello.img.gz", data)]
        self.assertEqual(len(packets), 1 + 325 + 1)
        self.assertEqual(receive_ymodem(packets), ("hello.img.gz", data))
        self.assertEqual(packets[256][1], 0, "block 256 is numbered 0")
        self.assertEqual(packets[257][1], 1)

    def test_the_last_block_is_padded_with_sub(self):
        packets = pi_run.ymodem_packets("a", b"xyz")
        self.assertEqual(packets[1][2], b"xyz" + b"\x1a" * 1021)
        self.assertEqual(packets[0][2][:8], b"a\x003\x00\x00\x00\x00\x00")

    def test_an_empty_file_is_its_header_and_the_closing_block(self):
        self.assertEqual(len(pi_run.ymodem_packets("a", b"")), 2)


class Watchdog(unittest.TestCase):
    """The PM registers' check and the writes that arm the watchdog."""

    def test_the_writes_are_those_run_on_the_board(self):
        # As run on the board on 2026-10-07.
        self.assertEqual(pi_run.watchdog_writes(12),
                         [(0x10_7D20_0024, 0x5A0C_0000), (0x10_7D20_001C, 0x5A00_0222)])
        self.assertEqual(pi_run.watchdog_writes(1)[0], (0x10_7D20_0024, 0x5A01_0000))
        self.assertEqual(pi_run.watchdog_writes(15)[0], (0x10_7D20_0024, 0x5A0F_0000))

    def test_the_time_is_one_to_fifteen_seconds(self):
        for seconds in (0, 16, -1):
            with self.assertRaises(ValueError):
                pi_run.watchdog_writes(seconds)

    def test_the_watchdog_must_read_back_as_armed(self):
        self.assertIsNone(pi_run.check_armed(0x222, 12 * 65536 - 100, 12))
        self.assertIsNone(pi_run.check_armed(0x222, 1, 12))
        for rstc, wdog, why in [(0x202, 0x8000, "RSTC reads back 0x00000202"),
                                (0x5A000222, 0x8000, "RSTC reads back 0x5a000222"),
                                (0x222, 0, "WDOG reads back 0x00000000"),
                                (0x222, 12 * 65536 + 1, "not a count below 0xc0000")]:
            self.assertIn(why, pi_run.check_armed(rstc, wdog, 12))

    def test_registers_as_recorded_pass(self):
        # power-on: RSTS as reported; after a reset: reported 0x20 or 0x1020, read 0
        for rsts, reported in ((0x1000, 0x1000), (0, 0x20), (0, 0x1020)):
            self.assertIsNone(pi_run.check_pm_registers(0x202, rsts, 0, reported))

    def test_registers_otherwise_are_refused(self):
        cases = {
            (0x203, 0, 0, 0x20): "RSTC reads 0x00000203",
            (0x5A000202, 0, 0, 0x20): "RSTC reads 0x5a000202",
            (0x202, 0x20, 0, 0): "bits the bootloader did not report",
            (0x202, 0x1020, 0, 0x1000): "bits the bootloader did not report",
            (0x202, 0, 0x4000, 0x20): "already running",
            (0x202, 0, 0, None): "was not seen",
        }
        for (rstc, rsts, wdog, reported), why in cases.items():
            self.assertIn(why, pi_run.check_pm_registers(rstc, rsts, wdog, reported))

    def test_the_firmware_framebuffer_must_be_the_board_facts(self):
        """The Pi 5's board facts give 1920 x 1080 x 32 at 0x3f40_0000,
        stride 7680; a boot whose framebuffer differs is refused, one with
        none passes."""
        expected = pi_run.board_framebuffer(os.path.join(HERE, os.pardir, "platforms", "rpi5b_2gb.toml"))
        self.assertEqual(expected, (0x3F40_0000, 1920, 1080, 7680, 32))
        bdinfo = "FB base     = 0x000000003f400000\nFB size     = 1920x1080x32\n"
        boot = (1920, 1080, 7680, 0x3F40_0000)
        self.assertEqual(pi_run.framebuffer_mismatch(expected, boot, bdinfo)[0], None)
        problem, note = pi_run.framebuffer_mismatch(expected, None, "devicetree  = board\n")
        self.assertIsNone(problem)
        self.assertIn("no framebuffer at this boot", note)
        for boot_fb, info, why in [
            ((1280, 720, 5120, 0x3F40_0000), bdinfo.replace("1920x1080", "1280x720"), "is 1280x720x32, stride 5120"),
            ((1920, 1080, 3840, 0x3F80_0000), bdinfo.replace("x32", "x16").replace("3f400000", "3f800000"),
             "is 1920x1080x16, stride 3840"),
            ((1920, 1080, 7680, 0x3E40_0000), bdinfo.replace("3f400000", "3e400000"), "at 0x3e400000"),
            (boot, bdinfo.replace("1920x1080", "1280x720"), "(boot log: 1920x1080"),
            (None, bdinfo, "do not both describe a framebuffer"),
            (boot, "devicetree  = board\n", "do not both describe a framebuffer"),
        ]:
            with self.subTest(why=why):
                self.assertIn(why, pi_run.framebuffer_mismatch(expected, boot_fb, info)[0])

    def test_python_3_11_or_later(self):
        self.assertIsNone(pi_run.python_too_old((3, 11, 0)))
        self.assertIsNone(pi_run.python_too_old((3, 13, 1)))
        self.assertIn("needs Python 3.11 or later", pi_run.python_too_old((3, 9, 6)))

    def test_md_output_is_read_word_by_word(self):
        output = "107d20001c: 00000202 00001000 00000000           ............"
        self.assertEqual(pi_run.parse_md(output, 0x107D20001C, 3), [0x202, 0x1000, 0])
        self.assertIsNone(pi_run.parse_md(output, 0x107D200020, 3))
        self.assertIsNone(pi_run.parse_md("garbage", 0x107D20001C, 3))


class UbootCommands(unittest.TestCase):
    """What `pi-run uboot` runs."""

    def test_commands_that_read_are_allowed(self):
        for command in ("bdinfo", "md.w 3f800000 10", "md.l 107d20001c 3", "fdt addr ${fdtcontroladdr}",
                        "fdt print /chosen", "printenv bootcmd", "crc32 10000000 100", "version"):
            self.assertIsNone(pi_run.uboot_command_refusal(command, False), command)

    def test_writes_need_allow_write(self):
        for command in ("mw.w 3f800000 f800 7e900", "cls"):
            self.assertIn("--allow-write", pi_run.uboot_command_refusal(command, False))
            self.assertIsNone(pi_run.uboot_command_refusal(command, True))

    def test_a_control_character_anywhere_is_refused(self):
        # BS, DEL, Ctrl-U and Ctrl-X erase what came before; Ctrl-A and Ctrl-K
        # move and kill; Ctrl-C, ESC, TAB and the rest edit the line too.
        for code in list(range(0x00, 0x20)) + [0x7F]:
            command = f"md 0{chr(code)}go 0x10000000"
            with self.subTest(code=hex(code)):
                self.assertIn("printable ASCII only", pi_run.uboot_command_refusal(command, True))
        self.assertIn("printable ASCII only", pi_run.uboot_command_refusal("md \x15go 0x10000000", False))
        self.assertIn("printable ASCII only", pi_run.uboot_command_refusal("version\u00e9", False))

    def test_starting_resetting_booting_and_storing_are_always_refused(self):
        for command in ("go 0x10000000", "reset", "boot", "bootm 0x1000", "fatwrite mmc 0 1 a 1",
                        "fdt set /chosen x y", "crc32 0 10 1000", "env set bootcmd x", "saveenv",
                        "md 0; go 0", "md 0 && reset", "md $(reset)"):
            self.assertIsNotNone(pi_run.uboot_command_refusal(command, True), command)


class StopWord(unittest.TestCase):
    """pi-run's stop word is the one U-Boot is built with."""

    def test_the_stop_word_and_countdown_match_u_boots_settings(self):
        path = os.path.join(HERE, os.pardir, "platforms", "rpi5b_2gb-u-boot.config")
        with open(path, encoding="utf-8") as handle:
            lines = handle.read().splitlines()
        settings = dict(line.split("=", 1) for line in lines if line.startswith("CONFIG_"))
        self.assertEqual(settings.get("CONFIG_AUTOBOOT_KEYED"), "y")
        self.assertIn("# CONFIG_AUTOBOOT_KEYED_CTRLC is not set", lines)
        self.assertIn("# CONFIG_AUTOBOOT_ENCRYPTION is not set", lines)
        self.assertEqual(settings.get("CONFIG_AUTOBOOT_STOP_STR"), f'"{pi_run.STOP_WORD}"')
        prompt = settings.get("CONFIG_AUTOBOOT_PROMPT", "")
        self.assertIn(pi_run.COUNTDOWN, prompt)
        self.assertIn(pi_run.STOP_WORD, prompt)
        self.assertNotIn(pi_run.ANY_KEY_COUNTDOWN, prompt)
        self.assertIn(pi_run.COUNTDOWN, KEYED_COUNTDOWN)
        self.assertIn(pi_run.COUNTDOWN, COUNTDOWN)


class Session(unittest.TestCase):
    """Whole runs against the stand-in board."""

    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.dir = self.tmp.name
        self.lock = os.path.join(self.dir, "board.lock")

    def tearDown(self):
        self.tmp.cleanup()

    def pi_run(self, board, *args, timeout=120):
        command = [sys.executable, os.path.join(HERE, "pi-run"), *args[:1], "--device", board.device,
                   "--lock", self.lock, "--type-delay", "0", *args[1:]]
        result = subprocess.run(command, stdin=subprocess.DEVNULL, capture_output=True, text=True,
                                timeout=timeout)
        return result.returncode, result.stdout + result.stderr

    def run_series(self, board, *specs, watchdog=2, extra=()):
        logs = os.path.join(self.dir, "logs")
        status, output = self.pi_run(board, "run", "--logs", logs, "--expect-dir", SYSTEMS,
                                     "--watchdog", str(watchdog), *extra, *specs)
        return status, output, logs

    def test_a_series_runs_each_image_checks_it_and_parks(self):
        big = make_image(self.dir, "ipc-demo", 300 * 1024)
        with FakeBoard() as board:
            status, output, logs = self.run_series(
                board, f"ipc-demo@{big}", f"supervisor-test@{make_image(self.dir, 'supervisor-test')}")
            parked = board.state
        self.assertEqual(status, 0, output)
        self.assertIn("ipc-demo: PASS (all 17 line(s) seen in order", output)
        self.assertIn("supervisor-test: PASS (all 10 line(s) seen in order", output)
        self.assertIn("the system reset the board itself", output)
        self.assertIn("the board is parked at U-Boot's prompt", output)
        self.assertEqual(parked, "prompt")
        self.assertEqual(board.loads, ["pi-run test image: ipc-demo"] + ["pi-run test image: supervisor-test"] * 2)
        # loaded where the loader runs, checked, then armed: WDOG, then RSTC
        self.assertIn("unzip 0x18000000 0x10000000", board.commands)
        self.assertIn(f"crc32 0x10000000 {300 * 1024:#x}", board.commands)
        self.assertEqual(board.writes[:2], [(pi_run.WDOG, 0x5A02_0000), (pi_run.RSTC, 0x5A00_0222)])
        self.assertEqual(sorted(os.listdir(logs)), ["01-ipc-demo.log", "02-supervisor-test.log", "session.log"])
        with open(os.path.join(logs, "02-supervisor-test.log"), encoding="latin-1") as handle:
            self.assertEqual(handle.read().count("RPi: BOOTSYS release"), 2, "both boots in one log")

    def test_the_watchdog_ends_a_silent_image_and_the_next_one_runs(self):
        with FakeBoard() as board:
            status, output, _ = self.run_series(
                board, f"silent=ipc-demo@{make_image(self.dir, 'silent')}",
                f"ipc-demo@{make_image(self.dir, 'ipc-demo')}", watchdog=1)
        self.assertEqual(status, 1, output)
        self.assertIn("silent: FAIL (not seen in", output)
        self.assertIn("ipc-demo: PASS", output)
        self.assertEqual(board.boots, 3, "the first boot, one for each watchdog reset")

    def test_forbidden_output_fails_live_and_in_check_serial(self):
        with FakeBoard() as board:
            status, output, _ = self.run_series(
                board, f"forbidden=ipc-demo@{make_image(self.dir, 'forbidden')}", watchdog=1)
        self.assertEqual(status, 1, output)
        self.assertIn("forbidden output: 'NOWAYOS: ipc-consumer: MON|ERROR", output)
        self.assertIn("forbidden: FAIL (forbidden output 'MON|ERROR'", output)

    def assert_stopped_unarmed(self, board, status, output, why):
        self.assertEqual(status, 2, output)
        self.assertIn(why, output)
        self.assertIn("nothing was written to the PM registers, nothing is armed, and the board is at U-Boot's "
                      "prompt", output)
        self.assertEqual(board.writes, [], "nothing written to the PM registers")
        self.assertFalse(any(c.startswith("go") for c in board.commands), "nothing started")
        self.assertEqual(board.state, "prompt")

    def test_a_cancelled_transfer_stops_with_nothing_armed(self):
        with FakeBoard(cancel_at_block=3) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
            self.assert_stopped_unarmed(board, status, output, "U-Boot cancelled the transfer at block 3")

    def test_a_wrong_unzipped_size_stops_with_nothing_armed(self):
        with FakeBoard(unzip_short=True) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
            self.assert_stopped_unarmed(board, status, output, f"unzip did not give {40 * 1024} bytes")

    def test_a_wrong_checksum_stops_with_nothing_armed(self):
        with FakeBoard(crc_wrong=True) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
            self.assert_stopped_unarmed(board, status, output, "U-Boot's CRC-32 of the image is not")

    def test_unexpected_registers_stop_with_nothing_armed(self):
        cases = {(0x202, 0x20, 0): "RSTS reads 0x00000020, with bits the bootloader did not report",
                 (0x202, 0x1000, 0x8000): "WDOG reads 0x00008000: the watchdog is already running",
                 (0x302, 0x1000, 0): "RSTC reads 0x00000302"}
        for registers, why in cases.items():
            with FakeBoard(registers=registers) as board:
                status, output, logs = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
                self.assert_stopped_unarmed(board, status, output, why)
            for name in os.listdir(logs):
                os.remove(os.path.join(logs, name))

    def test_another_monitors_framebuffer_stops_the_run_with_nothing_armed(self):
        with FakeBoard(fb=(1280, 720, 5120, 0x3F40_0000, 32)) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
            self.assert_stopped_unarmed(board, status, output,
                                        "the firmware's framebuffer at this boot is 1280x720x32, stride 5120")

    def test_with_no_monitor_there_is_no_framebuffer_to_check(self):
        with FakeBoard(fb=None) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
        self.assertEqual(status, 0, output)
        self.assertIn("no framebuffer at this boot (no monitor attached at boot)", output)

    def test_a_board_that_does_not_answer_stops_the_run(self):
        with FakeBoard(answers=False, at_prompt=False) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}",
                                                extra=("--boot-timeout", "2"))
        self.assertEqual(status, 2, output)
        self.assertIn("the board is not answering and needs a person to power-cycle it", output)

    def test_a_systems_run_length_is_waited_for(self):
        """A system that asks for N seconds - test-run-length-N.bin in
        the description built beside its image - runs past --watchdog, as its
        watchdog PD keeps the PM watchdog armed, and its reset after N is the
        run's end, not a reset of its own."""
        image = make_image(self.dir, "ipc-demo")
        with open(os.path.join(self.dir, "ipc-demo.system"), "w", encoding="utf-8") as handle:
            handle.write('<system><memory_region name="run" prefill_path="test-run-length-3.bin" /></system>\n')
        with FakeBoard(watchdog_at_go=3.5) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{image}", watchdog=1,
                                                extra=("--reset-grace", "1"))
        self.assertEqual(status, 0, output)
        self.assertIn("for its system's run length, 3 s", output)
        self.assertIn("ipc-demo: the board reset", output)
        self.assertNotIn("reset the board itself", output)
        self.assertEqual(board.loads, ["pi-run test image: ipc-demo"])

    def test_a_run_length_is_read_from_the_description_beside_the_image(self):
        image = make_image(self.dir, "ipc-demo")
        self.assertIsNone(pi_run.run_length(image, "ipc-demo"))
        sdf = os.path.join(self.dir, "ipc-demo.system")
        for text, seconds in (('<memory_region prefill_path="test-run-length-120.bin" />', 120),
                              ('<memory_region prefill_path="screen-1x1.bin" />', None),
                              ('<memory_region prefill_path="test-run-length-0120.bin" />', None)):
            with self.subTest(text=text):
                with open(sdf, "w", encoding="utf-8") as handle:
                    handle.write(text)
                self.assertEqual(pi_run.run_length(image, "ipc-demo"), seconds)
        with open(sdf, "w", encoding="utf-8") as handle:
            handle.write('prefill_path="test-run-length-5.bin" prefill_path="test-run-length-6.bin"')
        with self.assertRaises(pi_run.BoardError):
            pi_run.run_length(image, "ipc-demo")

    def test_the_run_lengths_timing_is_the_watchdog_pds(self):
        """pi-run's PM_SERVICE_SECONDS and RUN_TICK_SECONDS are what the
        watchdog PD arms the PM watchdog for and the ticker's period."""
        root = os.path.join(HERE, os.pardir, "pds")
        with open(os.path.join(root, "watchdog", "src", "lib.rs"), encoding="utf-8") as handle:
            service = re.search(r"pub const PM_TIMEOUT_SECS: u32 = (\d+);", handle.read())
        with open(os.path.join(root, "watchdog-stand-in", "src", "lib.rs"), encoding="utf-8") as handle:
            tick = re.search(r"const TICKER_PERIOD_MS: u64 = (\d+);", handle.read())
        self.assertEqual(int(service.group(1)), pi_run.PM_SERVICE_SECONDS)
        self.assertEqual(int(tick.group(1)), pi_run.RUN_TICK_SECONDS * 1000)

    def test_a_watchdog_that_does_not_fire_stops_the_run(self):
        with FakeBoard(watchdog_works=False) as board:
            status, output, _ = self.run_series(board, f"silent=ipc-demo@{make_image(self.dir, 'silent')}", watchdog=1,
                                                extra=("--reset-grace", "1"))
        self.assertEqual(status, 2, output)
        self.assertIn("the watchdog did not fire; it needs a person to power-cycle it", output)

    def test_one_run_at_a_time(self):
        with open(self.lock, "w") as held:
            fcntl.flock(held, fcntl.LOCK_EX)
            with FakeBoard() as board:
                status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}",
                                                    extra=("--lock-wait", "0.3"))
        self.assertEqual(status, 2, output)
        self.assertIn("the board is busy: another run holds", output)
        self.assertEqual(board.commands, [])

    def test_the_lock_comes_from_pi_board_lock(self):
        env = dict(os.environ, PI_BOARD_LOCK=self.lock)
        out = subprocess.run([sys.executable, "-c", "import sys; sys.argv=['pi-run'];"
                              f"sys.path.insert(0, {HERE!r});"
                              "import importlib.machinery as m, importlib.util as u;"
                              f"l=m.SourceFileLoader('p', {os.path.join(HERE, 'pi-run')!r});"
                              "s=u.spec_from_loader('p', l); p=u.module_from_spec(s); l.exec_module(p);"
                              "print(p.default_lock())"], capture_output=True, text=True, env=env)
        self.assertEqual(out.stdout.strip(), self.lock, out.stderr)

    def test_bad_specs_and_reused_folders_are_refused(self):
        with FakeBoard() as board:
            status, output, _ = self.run_series(board, "no-image-path")
            self.assertEqual(status, 2)
            self.assertIn("give NAME[=EXPECT]@IMAGE", output)
            status, output, _ = self.run_series(board, f"no-such-system@{make_image(self.dir, 'silent')}")
            self.assertIn("cannot read", output)
            logs = os.path.join(self.dir, "logs")
            os.makedirs(logs, exist_ok=True)
            open(os.path.join(logs, "01-old.log"), "w").close()
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
            self.assertIn("already holds logs", output)
            self.assertEqual(board.commands, [])

    def test_uboot_runs_commands_that_read_at_the_prompt(self):
        log = os.path.join(self.dir, "uboot.log")
        with FakeBoard() as board:
            status, output = self.pi_run(board, "uboot", "--log", log, "version; md.l 107d20001c 3")
        self.assertEqual(status, 0, output)
        self.assertIn("=> version\nU-Boot 2026.07", output)
        self.assertIn("=> md.l 107d20001c 3\n107d20001c: 00000202 00001000 00000000", output)
        self.assertEqual(board.commands, ["version", "md.l 107d20001c 3"])

    def test_uboot_refuses_before_touching_the_board(self):
        log = os.path.join(self.dir, "uboot.log")
        with FakeBoard() as board:
            status, output = self.pi_run(board, "uboot", "--log", log, "version", "go 0x10000000")
        self.assertEqual(status, 2, output)
        self.assertIn("refused 'go 0x10000000'", output)
        self.assertEqual(board.commands, [])

    def test_a_line_not_echoed_as_typed_is_abandoned_unrun(self):
        log = os.path.join(self.dir, "uboot.log")
        with FakeBoard(echo_wrong=True) as board:
            status, output = self.pi_run(board, "uboot", "--log", log, "version")
        self.assertEqual(status, 2, output)
        self.assertIn("U-Boot did not echo 'version' as typed", output)
        self.assertIn("the line was abandoned, not run", output)
        self.assertEqual(board.commands, [], "nothing entered")
        self.assertTrue(board.controls and set(board.controls) == {0x03}, "Ctrl-C, and no other control character")

    def test_a_watchdog_that_does_not_read_back_armed_stops_before_go_and_parks(self):
        with FakeBoard(readback_wrong=True) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}",
                                                watchdog=1)
            parked = board.state
        self.assertEqual(status, 2, output)
        self.assertIn("the watchdog did not read back as armed: RSTC reads back 0x00000202", output)
        self.assertIn("after the PM writes entered (mw.l 107d200024 5a010000; mw.l 107d20001c 5a000222) the "
                      "watchdog reset the board, and it is parked at U-Boot's prompt", output)
        self.assertNotIn("nothing armed", output)
        self.assertFalse(any(c.startswith("go") for c in board.commands), "nothing started")
        self.assertEqual(parked, "prompt")

    def test_a_board_silent_after_a_failure_is_not_called_parked(self):
        with FakeBoard(cancel_at_block=2, silent_after_cancel=True) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
        self.assertEqual(status, 2, output)
        self.assertIn("nothing was written to the PM registers, nothing is armed, but U-Boot's prompt does not "
                      "answer: the board may need a person to power-cycle it", output)
        self.assertEqual(board.writes, [])

    def test_show_runs_one_image_with_no_watchdog_until_a_power_cycle_then_parks(self):
        logs = os.path.join(self.dir, "logs")
        with FakeBoard(power_cycle_after=1.0) as board:
            status, output = self.pi_run(board, "show", "--logs", logs, "--expect-dir", SYSTEMS,
                                         f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
            parked = board.state
        self.assertEqual(status, 0, output)
        self.assertIn("no watchdog armed; starting it", output)
        self.assertIn("is running with no watchdog: power-cycle the board to end it", output)
        self.assertIn("the board is parked at U-Boot's prompt", output)
        self.assertIn("ipc-demo: PASS", output)
        self.assertEqual(board.writes, [], "no PM register written")
        self.assertEqual(parked, "prompt")

    KEYS_EXPECT = ("NOWAYOS: ready\nNOWAYOS: received 0x1b\nNOWAYOS: received 0x5b\n"
                   "NOWAYOS: received 0x41\nNOWAYOS: received 0x7a\n!panicked\n")

    def keys_system(self, script, expect=KEYS_EXPECT):
        """An expect folder holding the `keys` system's expect file and its
        script, `script`."""
        folder = os.path.join(self.dir, "systems")
        os.makedirs(folder, exist_ok=True)
        with open(os.path.join(folder, "keys.expect"), "w", encoding="utf-8") as handle:
            handle.write(expect)
        with open(os.path.join(folder, "keys.qmp"), "w", encoding="utf-8") as handle:
            handle.write(script)
        return folder

    def test_a_systems_script_is_typed_through_the_probe(self):
        """A system's script's serial steps go to the running image's
        debug UART, each once the line it waits for has appeared."""
        folder = self.keys_system("wait NOWAYOS: ready\nserial \\e[A\nwait NOWAYOS: received 0x41\n"
                                  "serial z\n")
        logs = os.path.join(self.dir, "logs")
        with FakeBoard() as board:
            status, output = self.pi_run(board, "run", "--logs", logs, "--expect-dir", folder, "--watchdog", "2",
                                         f"keys@{make_image(self.dir, 'keys')}")
            keys = bytes(board.keys)
        self.assertEqual(status, 0, output)
        self.assertIn("keys: PASS (all 5 line(s) seen in order", output)
        self.assertEqual(keys, b"\x1b[Az")

    def test_a_script_only_qemu_can_play_is_refused_before_the_board(self):
        for step in ("key a", "tablet 800 480\nclick 1 1", "screen x.png", 'send {"execute": "stop"}'):
            with self.subTest(step=step):
                folder = self.keys_system(f"wait NOWAYOS: ready\n{step}\n")
                with FakeBoard() as board:
                    status, output = self.pi_run(board, "run", "--logs", os.path.join(self.dir, f"logs-{len(step)}"),
                                                 "--expect-dir", folder, f"keys@{make_image(self.dir, 'keys')}")
                    commands = list(board.commands)
                self.assertEqual(status, 2, output)
                self.assertIn("step is QEMU's alone: on the board a script plays only wait, pause and serial steps",
                              output)
                self.assertEqual(commands, [], "nothing typed at the board")

    def type_session(self, board, act, hold=60, script="wait NOWAYOS: ready\nserial q\n", expect=KEYS_EXPECT,
                     ready=None):
        """pi-run show --type against `board`, its stdin a pseudo-terminal:
        once the image runs, `act(master)` types; returns the exit status,
        the output, what reached the running image, and the terminal's
        settings before and after. `ready(board)`, if given, says when to
        act instead: by default, half a second after the image runs."""
        folder = self.keys_system(script, expect)
        logs = os.path.join(self.dir, "logs")
        master, slave = os.openpty()
        before = termios.tcgetattr(slave)
        try:
            command = [sys.executable, os.path.join(HERE, "pi-run"), "show", "--device", board.device,
                       "--lock", self.lock, "--type-delay", "0", "--logs", logs, "--expect-dir", folder,
                       "--hold", str(hold), "--type", f"keys@{make_image(self.dir, 'keys')}"]
            process = subprocess.Popen(command, stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                       text=True)
            deadline = time.monotonic() + 60
            if ready is None:
                while board.state != "running" and time.monotonic() < deadline:
                    time.sleep(0.05)
                time.sleep(0.5)
            else:
                while not ready(board) and time.monotonic() < deadline:
                    time.sleep(0.01)
            act(master)
            output, _ = process.communicate(timeout=90)
            try:
                after = termios.tcgetattr(slave)
            except termios.error:
                after = None  # its other end closed (Linux reports EIO)
        finally:
            for fd in (master, slave):
                try:
                    os.close(fd)
                except OSError:
                    pass
        return process.returncode, output, bytes(board.keys), before, after

    def test_show_type_stops_at_a_reboot_and_no_key_reaches_u_boot(self):
        """Board safety: a power cycle while a person types,
        in lab mode, where U-Boot waits at its prompt - typing stops at the
        first sign of the boot, no key is entered at U-Boot's prompt, the
        board is parked and the terminal set back as it was."""
        board = FakeBoard(lab_mode=True, power_cycle_after=1.5)
        stop = threading.Event()

        def act(master):
            def keep_typing():
                while not stop.is_set():
                    try:
                        os.write(master, b"x\r")
                    except OSError:
                        return
                    time.sleep(0.05)
            threading.Thread(target=keep_typing, daemon=True).start()

        with board:
            try:
                status, output, keys, before, after = self.type_session(board, act,
                                                                        expect="NOWAYOS: ready\n!panicked\n")
            finally:
                stop.set()
            commands = list(board.commands)
        self.assertIn(b"x\r", keys, "the image got keys before the power cycle")
        self.assertNotIn("x", commands, "no key entered at U-Boot's prompt")
        self.assertRegex(output, r"keys: typing stopped: the board (is booting again|printed 'RPi: BOOT')")
        self.assertIn("the board is parked at U-Boot's prompt", output)
        self.assertEqual(before, after, "the terminal is set back as it was")
        self.assertEqual(status, 0, output)

    def keep_typing(self, stop, chunks=(b"ab",), then=b"v", gap=0.1):
        """An `act` for type_session: types `chunks`, then `then` every
        `gap` s until `stop` is set."""
        def act(master):
            def typing():
                for chunk in chunks:
                    os.write(master, chunk)
                while not stop.is_set():
                    try:
                        os.write(master, then)
                    except OSError:
                        return
                    time.sleep(gap)
            threading.Thread(target=typing, daemon=True).start()
        return act

    def test_show_type_stops_when_u_boot_comes_back_without_a_readable_banner(self):
        """Board safety: a boot whose bootloader banner is
        garbled, then U-Boot's banner, countdown and prompt, in lab mode and
        all in one burst: U-Boot's own lines stop the typing, and nothing typed
        after them reaches U-Boot's prompt."""
        stop = threading.Event()
        with FakeBoard(lab_mode=True, garbled_reboot_after=1.0) as board:
            try:
                status, output, keys, _, _ = self.type_session(board, self.keep_typing(stop),
                                                               expect="NOWAYOS: ready\n")
            finally:
                stop.set()
            typed, commands = board.typed, list(board.commands)
        self.assertIn(b"ab", keys)
        self.assertNotIn("v", typed + " ".join(commands), output)
        self.assertRegex(output, r"keys: typing stopped: the board printed '(U-Boot 20|Hit any key)")
        self.assertIn("the board is parked at U-Boot's prompt", output)

    def test_show_type_stops_when_go_returns_with_a_key_crossing(self):
        """Board safety: `go` returns to U-Boot's prompt with
        no boot, and a key in flight is echoed after the prompt, so the
        prompt is no longer the last thing printed: typing still stops, and
        nothing more reaches U-Boot; the board is parked at a fresh prompt."""
        stop = threading.Event()
        with FakeBoard(go_returns_after=1.0, crossing=b"k") as board:
            try:
                status, output, keys, _, _ = self.type_session(board, self.keep_typing(stop),
                                                               expect="NOWAYOS: ready\n")
            finally:
                stop.set()
            typed, commands, controls = board.typed, list(board.commands), list(board.controls)
        self.assertIn(b"ab", keys)
        self.assertNotIn("v", typed + " ".join(commands), output)
        self.assertIn("keys: typing stopped: the board printed '## Application terminated': it no longer runs "
                      "the image", output)
        self.assertIn("the board is parked at U-Boot's prompt", output)

    def test_show_type_stops_at_a_quiet_return_to_a_prompt_with_a_key_after_it(self):
        """Board safety: `go` returns with nothing but U-Boot's
        prompt, and a key in flight is echoed after it - no other sign, and
        the prompt not the last thing printed: the prompt at a line's start
        stops the typing (PROMPT_ANYWHERE_RE), and nothing more reaches
        U-Boot."""
        stop = threading.Event()
        with FakeBoard(go_returns_after=1.0, go_return_text="", crossing=b"k") as board:
            try:
                status, output, keys, _, _ = self.type_session(board, self.keep_typing(stop),
                                                               expect="NOWAYOS: ready\n")
            finally:
                stop.set()
            typed, commands = board.typed, list(board.commands)
        self.assertIn(b"ab", keys)
        self.assertNotIn("v", typed + " ".join(commands), output)
        self.assertIn("keys: typing stopped: U-Boot's prompt is showing", output)
        self.assertIn("the board is parked at U-Boot's prompt", output)

    def test_the_prompt_is_found_at_any_lines_start_with_text_after_it(self):
        """PROMPT_ANYWHERE_RE: U-Boot's prompt where a line
        starts - after a newline, an escape sequence or <INTERRUPT> - with
        a crossed key echoed after it; never in the middle of a line."""
        for text in ("NOWAYOS: ready\n=> k", "\nU-Boot> go", "<INTERRUPT>=> a", "\x1b[2K=> b", "=> ", "U-Boot> "):
            with self.subTest(text=text):
                self.assertIsNotNone(pi_run.PROMPT_ANYWHERE_RE.search(text))
        for text in ("NOWAYOS: a=> b", "x U-Boot> y", "NOWAYOS: ready\n"):
            with self.subTest(text=text):
                self.assertIsNone(pi_run.PROMPT_ANYWHERE_RE.search(text))

    def test_the_board_is_parked_only_after_ctrl_c_and_a_fresh_prompt(self):
        """Board safety: `go` returns to U-Boot's prompt and a
        crossed key is echoed a moment after it, when the prompt was already
        the last thing read: the run still sends Ctrl-C and waits for a
        fresh prompt, so the key is abandoned, not left on U-Boot's line."""
        with FakeBoard(go_returns_after=1.0, crossing=b"k", crossing_delay=0.5) as board:
            status, output, _, _, _ = self.type_session(board, lambda master: None, expect="NOWAYOS: ready\n")
            time.sleep(1.0)  # past the late echo, whatever the run did
            typed, commands, controls = board.typed, list(board.commands), list(board.controls)
        self.assertEqual(typed, "", "nothing left on U-Boot's line")
        self.assertNotIn("k", " ".join(commands))
        self.assertIn(0x03, controls)
        self.assertIn("the board is parked at U-Boot's prompt", output)

    def test_show_type_sends_nothing_before_go_is_seen_entered(self):
        """Board safety: U-Boot is slow to echo go's Enter;
        what is typed before that echo, or the image's first line, could
        land on U-Boot's line, so it is dropped - and the person told - and
        what is typed once the image runs reaches it."""
        def act(master):
            def typing():
                os.write(master, b"early")
                deadline = time.monotonic() + 30
                while board.state != "running" and time.monotonic() < deadline:
                    time.sleep(0.02)
                time.sleep(0.5)
                os.write(master, b"late")
                time.sleep(0.5)
                os.write(master, b"\x1d")
            threading.Thread(target=typing, daemon=True).start()

        with FakeBoard(go_echo_delay=1.0) as board:
            status, output, keys, _, _ = self.type_session(board, act, ready=lambda b: b.go_seen.is_set())
            typed, commands = board.typed, list(board.commands)
        self.assertEqual(keys, b"late", output)
        self.assertNotIn("early", typed + " ".join(commands))
        self.assertIn("(keys typed before the image started are not sent)", output)

    def test_a_serial_step_after_a_stale_line_is_never_typed(self):
        """Board safety: after a pause, a wait can match a line
        that came long before; that line no longer shows the image runs - a
        system that resets itself is silent at first - so the serial step
        after it is not typed and the script is abandoned."""
        folder = self.keys_system("wait NOWAYOS: ready\nserial a\npause 1000\nwait NOWAYOS: received 0x61\n"
                                  "serial b\n", "NOWAYOS: ready\n")
        with FakeBoard() as board:
            status, output = self.pi_run(board, "run", "--logs", os.path.join(self.dir, "logs"), "--expect-dir",
                                         folder, "--watchdog", "5", f"keys@{make_image(self.dir, 'keys')}")
            keys = bytes(board.keys)
        self.assertEqual(keys, b"a", output)
        self.assertIn("keys: script abandoned, nothing more typed: the line its wait matched came", output)
        self.assertEqual(status, 0, output)

    def test_a_serial_step_inside_the_watchdog_window_is_never_typed(self):
        """Board safety: the watchdog's reset is silent for
        0.77 s, as the bootloader is, so no sign shows in time; a serial step
        due within 1.5 s of the reset - here the line it waits for comes
        late - is refused, and nothing reaches the board while it resets."""
        folder = self.keys_system("wait NOWAYOS: ready\nserial QQ\\r\n", "NOWAYOS: ready\n")
        with FakeBoard(lab_mode=True, silent_reset=0.77, go_delay=0.7) as board:
            status, output = self.pi_run(board, "run", "--logs", os.path.join(self.dir, "logs"), "--expect-dir",
                                         folder, "--watchdog", "2", f"keys@{make_image(self.dir, 'keys')}")
            during, commands, keys = bytes(board.during_reset), list(board.commands), bytes(board.keys)
        self.assertEqual(during, b"", "nothing written while the board reset")
        self.assertNotIn("QQ", " ".join(commands), output)
        self.assertNotIn(b"Q", keys)
        self.assertIn("script abandoned, nothing more typed: the watchdog resets the board within 1.5 s", output)

    def test_show_type_stops_on_ctrl_c(self):
        with FakeBoard() as board:
            status, output, keys, before, after = self.type_session(board, lambda m: os.write(m, b"ab\x03cd"))
        self.assertEqual(status, 2, output)
        self.assertEqual(keys, b"ab", "nothing after Ctrl-C")
        self.assertIn("keys: typing stopped, and the image is left running", output)
        self.assertEqual(before, after)

    def test_show_type_ends_with_the_hold(self):
        with FakeBoard() as board:
            status, output, keys, before, after = self.type_session(board, lambda m: os.write(m, b"z"), hold=3)
        self.assertEqual(status, 2, output)
        self.assertEqual(keys, b"z")
        self.assertIn("keys still runs after 3 s, and is left running", output)
        self.assertEqual(before, after)

    def test_show_type_ends_with_its_input(self):
        """The end of the terminal's input - here its other end closed -
        stops typing, as Ctrl-] does."""
        with FakeBoard() as board:
            status, output, keys, _, _ = self.type_session(board, lambda m: (os.write(m, b"z"), time.sleep(0.5),
                                                                             os.close(m)))
        self.assertEqual(status, 2, output)
        self.assertEqual(keys, b"z")
        self.assertIn("keys: typing stopped, and the image is left running", output)

    def test_a_reset_during_a_scripts_pause_types_nothing_more(self):
        """Board safety: the watchdog resets the board while
        a script pauses; the serial step after the pause, and the wait
        before it, are never played, so nothing reaches U-Boot's prompt in
        lab mode."""
        folder = self.keys_system("wait NOWAYOS: ready\npause 1500\nwait NOWAYOS: ready\nserial QQQ\\r\n",
                                  "NOWAYOS: ready\n")
        logs = os.path.join(self.dir, "logs")
        with FakeBoard(lab_mode=True, watchdog_at_go=0.4) as board:
            status, output = self.pi_run(board, "run", "--logs", logs, "--expect-dir", folder, "--watchdog", "2",
                                         f"keys@{make_image(self.dir, 'keys')}")
            commands, keys = list(board.commands), bytes(board.keys)
        self.assertNotIn("QQQ", " ".join(commands), output)
        self.assertNotIn(b"Q", keys)
        self.assertRegex(output, r"keys: script abandoned, nothing more typed: the board (is booting again|printed "
                                 r"'RPi: BOOT')")
        self.assertIn("the board is parked at U-Boot's prompt", output)

    def test_unsafe_scripts_are_refused_before_the_board(self):
        """A board script starts with a wait - nothing is typed before the
        image shows it runs - types nothing straight after a pause, and its
        pauses fit inside the watchdog."""
        for script, why in (("serial a\n", "starts with a wait step"),
                            ("pause 10\nwait NOWAYOS: ready\n", "starts with a wait step"),
                            ("wait NOWAYOS: ready\npause 1500\npause 600\n", "add up to 2.10 s, more than the 2 s the image runs"),
                            ("wait NOWAYOS: ready\npause 100\nserial a\n", "a serial step follows a wait, never a pause")):
            with self.subTest(script=script):
                folder = self.keys_system(script)
                with FakeBoard() as board:
                    status, output = self.pi_run(board, "run", "--logs", os.path.join(self.dir, f"logs{len(script)}"),
                                                 "--expect-dir", folder, "--watchdog", "2",
                                                 f"keys@{make_image(self.dir, 'keys')}")
                    commands = list(board.commands)
                self.assertEqual(status, 2, output)
                self.assertIn(why, output)
                self.assertEqual(commands, [], "nothing typed at the board")

    def test_show_type_passes_what_is_typed_to_the_board(self):
        """Show --type, for a person to drive: keys typed on the
        terminal reach the running image's debug UART as typed, its lines are
        printed, and Ctrl-] stops the typing and leaves the image running.
        The system's script is not played."""
        folder = self.keys_system("wait NOWAYOS: ready\nserial q\n")
        logs = os.path.join(self.dir, "logs")
        master, slave = os.openpty()
        try:
            with FakeBoard() as board:
                command = [sys.executable, os.path.join(HERE, "pi-run"), "show", "--device", board.device,
                           "--lock", self.lock, "--type-delay", "0", "--logs", logs, "--expect-dir", folder,
                           "--hold", "60", "--type", f"keys@{make_image(self.dir, 'keys')}"]
                process = subprocess.Popen(command, stdin=slave, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                           text=True)
                deadline = time.monotonic() + 60
                while board.state != "running" and time.monotonic() < deadline:
                    time.sleep(0.05)
                time.sleep(0.5)
                os.write(master, b"\x1b[Az")
                while len(board.keys) < 4 and time.monotonic() < deadline:
                    time.sleep(0.05)
                os.write(master, b"\x1d")
                output, _ = process.communicate(timeout=60)
                keys = bytes(board.keys)
        finally:
            os.close(master)
            os.close(slave)
        self.assertEqual(process.returncode, 2, output)
        self.assertEqual(keys, b"\x1b[Az", "as typed, and not the script's q")
        self.assertIn("what you type now goes to the board's debug UART, as keys; Ctrl-] or Ctrl-C stops", output)
        self.assertIn("  | NOWAYOS: received 0x7a", output)
        self.assertIn("keys: typing stopped, and the image is left running", output)
        self.assertEqual(board.writes, [], "no watchdog")

    def test_show_type_needs_a_terminal(self):
        with FakeBoard() as board:
            status, output = self.pi_run(board, "show", "--logs", os.path.join(self.dir, "logs"), "--expect-dir",
                                         SYSTEMS, "--type", f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
            commands = list(board.commands)
        self.assertEqual(status, 2, output)
        self.assertIn("--type passes on what is typed on a terminal", output)
        self.assertEqual(commands, [])

    def test_show_says_when_nobody_power_cycled_the_board(self):
        logs = os.path.join(self.dir, "logs")
        with FakeBoard() as board:
            status, output = self.pi_run(board, "show", "--logs", logs, "--expect-dir", SYSTEMS, "--hold", "1",
                                         f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
        self.assertEqual(status, 2, output)
        self.assertIn("ipc-demo still runs after 1 s, and is left running. To park the board: in lab mode", output)
        self.assertNotIn("parked at U-Boot's prompt\n", output)
        self.assertEqual(board.writes, [])

    @staticmethod
    def exclusive_ptys():
        """Whether this system's pseudo-terminals honour TIOCEXCL (Linux's
        do; macOS's do not, though its serial devices do)."""
        master, slave = os.openpty()
        try:
            fcntl.ioctl(slave, termios.TIOCEXCL)
            try:
                os.close(os.open(os.ttyname(slave), os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK))
                return False
            except OSError:
                return True
        finally:
            os.close(master)
            os.close(slave)

    def test_the_serial_device_is_held_exclusively(self):
        if not self.exclusive_ptys():
            self.skipTest("this system's pseudo-terminals ignore TIOCEXCL")
        master, slave = os.openpty()
        try:
            port = pi_run.Port(os.ttyname(slave))
            try:
                with self.assertRaises(OSError):
                    os.close(os.open(os.ttyname(slave), os.O_RDWR | os.O_NOCTTY | os.O_NONBLOCK))
            finally:
                port.close()
        finally:
            os.close(master)
            os.close(slave)

    def test_a_line_not_abandoned_is_never_entered(self):
        """Ctrl-C after a wrong echo, and no fresh prompt: the line may still be
        live, so nothing more is sent - not a carriage return, not a command."""
        log = os.path.join(self.dir, "uboot.log")
        with FakeBoard(echo_wrong=True, ignore_interrupt_on_line=True) as board:
            status, output = self.pi_run(board, "uboot", "--log", log, "version")
            time.sleep(0.3)
        self.assertEqual(status, 2, output)
        self.assertIn("the line may still be live, so nothing more is sent", output)
        self.assertEqual(board.commands, [], "nothing entered")
        self.assertEqual(board.typed, "version", "the line left as typed, never entered")

    def test_a_pm_write_not_echoed_is_not_called_written(self):
        with FakeBoard(echo_wrong_after="mw.l 107d200024") as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}")
        self.assertEqual(status, 2, output)
        self.assertIn("U-Boot did not echo 'mw.l 107d200024 5a020000' as typed", output)
        self.assertIn("nothing was written to the PM registers, nothing is armed, and the board is at U-Boot's "
                      "prompt", output)
        self.assertEqual(board.writes, [])

    def test_a_pm_write_entered_is_named_when_the_next_one_fails(self):
        with FakeBoard(echo_wrong_after="mw.l 107d20001c") as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}",
                                                watchdog=1, extra=("--reset-grace", "1"))
        self.assertEqual(status, 2, output)
        self.assertIn("the PM writes entered (mw.l 107d200024 5a010000) did not reset the board", output)
        self.assertEqual(board.writes, [(pi_run.WDOG, 0x5A01_0000)])

    def test_a_run_started_part_way_through_a_boot_resets_and_runs(self):
        with FakeBoard(at_prompt=False, mid_boot_at=4.0) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}",
                                                watchdog=1)
        self.assertEqual(status, 0, output)
        self.assertIn("no U-Boot prompt: waiting for the board to boot", output)
        self.assertIn("U-Boot is at its prompt: resetting the board", output)
        self.assertIn("ipc-demo: PASS", output)

    def test_a_board_in_lab_mode_runs_and_parks_with_no_key_needed(self):
        with FakeBoard(lab_mode=True) as board:
            status, output, _ = self.run_series(board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}",
                                                watchdog=1)
            parked = board.state
        self.assertEqual(status, 0, output)
        self.assertIn("the board is parked at U-Boot's prompt", output)
        self.assertEqual(parked, "prompt")

    def test_uboot_wait_boot_takes_a_prompt_it_finds(self):
        log = os.path.join(self.dir, "uboot.log")
        with FakeBoard() as board:
            status, output = self.pi_run(board, "uboot", "--log", log, "--wait-boot", "version", timeout=30)
        self.assertEqual(status, 0, output)
        self.assertNotIn("waiting for U-Boot's countdown", output)
        self.assertEqual(board.commands, ["version"])

    def test_a_keyed_countdown_is_stopped_with_the_stop_word(self):
        # U-Boot stops only for its stop word; a space or Ctrl-C would
        # leave it counting down, and the board would boot its card.
        log = os.path.join(self.dir, "uboot.log")
        with FakeBoard(at_prompt=False, mid_boot_at=1.0, keyed=True) as board:
            status, output = self.pi_run(board, "uboot", "--log", log, "--wait-boot", "version", timeout=30)
        self.assertEqual(status, 0, output)
        self.assertIn("waiting for U-Boot's countdown", output)
        self.assertEqual(board.commands, ["version"])
        self.assertEqual(len(board.stopped_by), 1)
        self.assertTrue(board.stopped_by[0].endswith(b"stopboot"), board.stopped_by)

    def test_a_series_on_a_keyed_board_stops_each_countdown_and_parks(self):
        with FakeBoard(keyed=True) as board:
            status, output, _ = self.run_series(
                board, f"ipc-demo@{make_image(self.dir, 'ipc-demo')}",
                f"supervisor-test@{make_image(self.dir, 'supervisor-test')}")
            parked, stopped_by = board.state, board.stopped_by
        self.assertEqual(status, 0, output)
        self.assertIn("ipc-demo: PASS", output)
        self.assertIn("supervisor-test: PASS", output)
        self.assertEqual(parked, "prompt")
        self.assertTrue(stopped_by and all(key.endswith(b"stopboot") for key in stopped_by), stopped_by)

    def test_an_any_key_countdown_is_stopped_with_a_space(self):
        # A card whose U-Boot predates the stop word: a space, as before.
        log = os.path.join(self.dir, "uboot.log")
        with FakeBoard(at_prompt=False, mid_boot_at=1.0) as board:
            status, output = self.pi_run(board, "uboot", "--log", log, "--wait-boot", "version", timeout=30)
        self.assertEqual(status, 0, output)
        self.assertEqual(board.stopped_by, [b" "])

    def test_uboot_without_a_prompt_says_so(self):
        log = os.path.join(self.dir, "uboot.log")
        with FakeBoard(at_prompt=False, answers=False) as board:
            status, output = self.pi_run(board, "uboot", "--log", log, "version")
        self.assertEqual(status, 2, output)
        self.assertIn("no U-Boot prompt", output)


if __name__ == "__main__":
    unittest.main()

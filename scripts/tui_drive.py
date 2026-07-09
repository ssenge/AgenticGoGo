#!/usr/bin/env python3
"""Drive a TUI on a real, properly-sized pty and capture what it paints.

`script(1)` gives the child a pty but no window size when its own stdout is not a
terminal — ratatui then renders zero cells and the capture is empty. So we fork a
pty ourselves, set TIOCSWINSZ, optionally send a keypress after the first paint,
and print everything the child wrote.

    tui_drive.py --key q --after 1.5 --timeout 15 -- agg dashboard

stdout: whatever the TUI painted (raw, escape codes included).
exit:   the child's exit status, or 124 if it had to be killed on timeout.
"""
import argparse
import fcntl
import os
import pty
import select
import signal
import struct
import sys
import termios
import time

KEYS = {
    "Tab": b"\t",
    "Esc": b"\x1b",
    "Up": b"\x1b[A",
    "Down": b"\x1b[B",
    "PageUp": b"\x1b[5~",
    "PageDown": b"\x1b[6~",
    "Home": b"\x1b[H",
    "End": b"\x1b[F",
    "Enter": b"\r",
}

# A pseudo-key. ratatui only re-emits CHANGED cells, so a title going from `[⏵live]` to
# `[paused]` leaves the shared `e` untouched and the stream contains `paus` … `d` — the word
# never appears contiguously, and grepping the capture silently misses it. Resizing the pty
# makes `Terminal::autoresize()` mark everything dirty, so the next frame repaints in full.
# Put `RESIZE` after the keys you care about and before the quit key.
RESIZE = "RESIZE"


def keybytes(name):
    return KEYS.get(name, name.encode())


ap = argparse.ArgumentParser()
ap.add_argument("--key", default="", help="single key to send once the TUI has painted")
ap.add_argument("--after", type=float, default=1.5, help="seconds to wait before sending --key")
ap.add_argument(
    "--seq",
    default="",
    help='key script: "1.5:Tab,0.5:f,0.5:q" — each entry is <delay-since-previous>:<key>. '
    "Named keys: " + ", ".join(KEYS),
)
ap.add_argument("--timeout", type=float, default=15.0, help="hard deadline")
ap.add_argument("--rows", type=int, default=40)
ap.add_argument("--cols", type=int, default=120)
ap.add_argument("cmd", nargs=argparse.REMAINDER)
a = ap.parse_args()
cmd = a.cmd[1:] if a.cmd and a.cmd[0] == "--" else a.cmd
if not cmd:
    sys.exit("usage: tui_drive.py [opts] -- <cmd> [args...]")

# normalise --key/--after into the same schedule --seq uses
if a.seq:
    steps = []
    t = 0.0
    for item in a.seq.split(","):
        delay, _, name = item.partition(":")
        t += float(delay)
        steps.append((t, RESIZE if name == RESIZE else keybytes(name)))
elif a.key:
    steps = [(a.after, keybytes(a.key))]
else:
    steps = []

pid, fd = pty.fork()
if pid == 0:  # child: the pty is our controlling terminal
    os.execvp(cmd[0], cmd)
    os._exit(127)

def setwinsz(rows, cols):
    fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", rows, cols, 0, 0))


# give the TUI a real window, otherwise it draws nothing
setwinsz(a.rows, a.cols)
resized = 0

out = bytearray()
start = time.monotonic()
next_step = 0
status = None
while True:
    elapsed = time.monotonic() - start
    if elapsed > a.timeout:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
        sys.stdout.buffer.write(bytes(out))
        sys.exit(124)
    while next_step < len(steps) and elapsed >= steps[next_step][0]:
        payload = steps[next_step][1]
        if payload == RESIZE:
            resized += 1
            setwinsz(a.rows - resized, a.cols)  # force a full repaint
            os.kill(pid, signal.SIGWINCH)
        else:
            os.write(fd, payload)
        next_step += 1
    r, _, _ = select.select([fd], [], [], 0.1)
    if r:
        try:
            chunk = os.read(fd, 65536)
        except OSError:
            chunk = b""
        if not chunk:
            break
        out += chunk
    done, st = os.waitpid(pid, os.WNOHANG)
    if done:
        status = st
        break

if status is None:
    _, status = os.waitpid(pid, 0)

# drain anything still buffered in the pty
while True:
    r, _, _ = select.select([fd], [], [], 0.1)
    if not r:
        break
    try:
        chunk = os.read(fd, 65536)
    except OSError:
        break
    if not chunk:
        break
    out += chunk

sys.stdout.buffer.write(bytes(out))
sys.stdout.buffer.flush()
sys.exit(os.waitstatus_to_exitcode(status) if status is not None else 0)

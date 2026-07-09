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

ap = argparse.ArgumentParser()
ap.add_argument("--key", default="", help="key to send once the TUI has painted")
ap.add_argument("--after", type=float, default=1.5, help="seconds to wait before sending --key")
ap.add_argument("--timeout", type=float, default=15.0, help="hard deadline")
ap.add_argument("--rows", type=int, default=40)
ap.add_argument("--cols", type=int, default=120)
ap.add_argument("cmd", nargs=argparse.REMAINDER)
a = ap.parse_args()
cmd = a.cmd[1:] if a.cmd and a.cmd[0] == "--" else a.cmd
if not cmd:
    sys.exit("usage: tui_drive.py [opts] -- <cmd> [args...]")

pid, fd = pty.fork()
if pid == 0:  # child: the pty is our controlling terminal
    os.execvp(cmd[0], cmd)
    os._exit(127)

# give the TUI a real window, otherwise it draws nothing
fcntl.ioctl(fd, termios.TIOCSWINSZ, struct.pack("HHHH", a.rows, a.cols, 0, 0))

out = bytearray()
start = time.monotonic()
sent = False
status = None
while True:
    elapsed = time.monotonic() - start
    if elapsed > a.timeout:
        os.kill(pid, signal.SIGKILL)
        os.waitpid(pid, 0)
        sys.stdout.buffer.write(bytes(out))
        sys.exit(124)
    if a.key and not sent and elapsed >= a.after:
        os.write(fd, a.key.encode())
        sent = True
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

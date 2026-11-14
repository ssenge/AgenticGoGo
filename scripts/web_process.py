#!/usr/bin/env python3
"""Isolated browser check of the process page (A-39): can a human start a project, answer its checkpoint with a decision,
read a step's overlay facts and stop the run, all from the web UI?

Self-contained: a projects folder with fake agents, its own `agg serve --projects --process examples/process/process.yaml`
and SvelteKit app on free ports, Chromium driven through the page. Nothing is shared with the other web fixtures.

    scripts/web_process.py [--root <repo>] [--shots DIR]      exit 0 = every check passed
"""
import argparse
import os
import socket
import subprocess
import sys
import time
from pathlib import Path

from playwright.sync_api import sync_playwright

_ap = argparse.ArgumentParser()
_ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
_ap.add_argument("--shots", default="")
A = _ap.parse_args()
ROOT = Path(A.root)
AGG = ROOT / "target/debug/agg"
P = Path(os.environ.get("TMPDIR", "/tmp")) / f"agg-webprocess.{os.getpid()}"
(P / "bin").mkdir(parents=True)
(P / "home").mkdir()
(P / "projects").mkdir()

# a worker that commits one line per session, a critic that is met
(P / "bin/claude").write_text("""#!/bin/sh
for a in "$@"; do [ "$a" = --version ] && { echo "fake-claude 0.0.0"; exit 0; }; done
echo "session $$" >> work.log; git add -A >/dev/null 2>&1; git commit -qm "worker: did work" >/dev/null 2>&1
printf '%s\\n' '{"type":"result","subtype":"success","is_error":false,"result":"done","session_id":"fake","usage":{"output_tokens":1},"total_cost_usd":0.01}'
""")
(P / "bin/codex").write_text("""#!/bin/sh
for a in "$@"; do [ "$a" = --version ] && { echo "fake-codex 0.0.0"; exit 0; }; done
cat > /dev/null
printf '%s\\n' '{"type":"item.completed","item":{"type":"agent_message","text":"{\\"met\\": true, \\"rationale\\": \\"fine\\"}"}}'
""")
for f in ("claude", "codex"):
    os.chmod(P / "bin" / f, 0o755)


def port():
    """A free local port."""
    s = socket.socket()
    s.bind(("127.0.0.1", 0))
    p = s.getsockname()[1]
    s.close()
    return p


env = {**os.environ, "PATH": f"{P}/bin:{os.environ['PATH']}", "HOME": str(P / "home")}
aport, wport = port(), port()
srv = subprocess.Popen([str(AGG), "serve", "--port", str(aport), "--projects", str(P / "projects"), "--process",
                        str(ROOT / "examples/process/process.yaml")], cwd=P / "projects", env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
web = subprocess.Popen(["node", "build/index.js"], cwd=ROOT / "src/web",
                       env={**os.environ, "AGG_API": f"http://127.0.0.1:{aport}", "PORT": str(wport)},
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
fails = []


def check(cond, what):
    """One check, printed."""
    print(("  ✔ " if cond else "  ✘ ") + what)
    if not cond:
        fails.append(what)


def shot(page, name):
    """A screenshot, when asked for."""
    if A.shots:
        page.screenshot(path=f"{A.shots}/{name}.png", full_page=True)


try:
    time.sleep(2)
    with sync_playwright() as pw:
        browser = pw.chromium.launch()
        page = browser.new_page(viewport={"width": 1200, "height": 900})
        errors = []
        page.on("pageerror", lambda e: errors.append(str(e)))
        page.on("dialog", lambda d: d.accept())
        page.goto(f"http://127.0.0.1:{wport}/process")
        page.get_by_text("Start a new project").wait_for()
        check(True, "the projects page shows the process's new-project form")
        page.fill("#f-name", "demo")
        page.fill("#f-task", "Build the thing.")
        page.click("button[type=submit]")
        page.get_by_text("Process run · demo").wait_for()
        page.get_by_text("Waiting for you · cp-survey").wait_for(timeout=120000)
        check("survey of" in page.locator("pre").first.inner_text(), "the checkpoint's document is on the page")
        shot(page, "checkpoint")
        page.locator("textarea").first.fill("keep the survey short")
        page.get_by_text("Yes, approve").click()
        page.get_by_text("Waiting for you · cp-build").wait_for(timeout=120000)
        decided = (P / "projects/demo/checkpoints/cp-survey-decisions.md").read_text()
        check(decided == "- keep the survey short\n", "the decision was written before the yes")
        page.locator("button.chip", has_text="1.3").first.click()
        page.get_by_text("lists the units to build").wait_for()
        check(True, "a step's overlay facts show as key and value rows")
        page.get_by_role("button", name="Stop").click()
        page.get_by_role("button", name="Resume").wait_for(timeout=60000)
        check(True, "Stop ends the run; Resume is offered")
        shot(page, "stopped")
        check(not errors, f"no page errors {errors}")
        browser.close()
finally:
    for p in (web, srv):
        p.kill()
        p.wait()
    subprocess.run(["pkill", "-f", str(P)], check=False)

sys.exit(1 if fails else 0)

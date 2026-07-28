#!/usr/bin/env python3
"""Real-browser acceptance test for the AgenticGoGo web interface.

Drives Chromium via Playwright and CLICKS what a user clicks — Pause, Resume, Inject…,
Budget…, Stop — then verifies the effect reached the actual loop (its stderr log, the prompt
the next worker session received). Nothing is stubbed but the `claude` worker itself.

    web_e2e.py --url http://127.0.0.1:5173 --project /path/to/proj [--headed] [--shots DIR]

exit 0 = every check passed.
"""
import argparse
import json
import re
import sys
import time
from pathlib import Path

from playwright.sync_api import sync_playwright, expect

ap = argparse.ArgumentParser()
ap.add_argument("--url", required=True)
ap.add_argument("--project", required=True, help="the agg project dir the loop runs in")
ap.add_argument("--headed", action="store_true")
ap.add_argument("--shots", default="", help="write screenshots here")
A = ap.parse_args()
PROJ = Path(A.project)

PASS, FAIL = 0, 0
FAILED = []


def ok(msg):
    global PASS
    PASS += 1
    print(f"  \033[32m✔\033[0m {msg}")


def bad(msg, detail=""):
    global FAIL
    FAIL += 1
    FAILED.append(msg)
    print(f"  \033[31m✘ {msg}\033[0m")
    if detail:
        print(f"      {detail}")


def check(cond, msg, detail=""):
    ok(msg) if cond else bad(msg, detail)


def waitfor(desc, fn, secs=40):
    """Poll a side effect on the real filesystem/log until it happens."""
    deadline = time.time() + secs
    while time.time() < deadline:
        try:
            if fn():
                ok(desc)
                return True
        except Exception:
            pass
        time.sleep(0.1)
    bad(desc, f"timed out after {secs}s")
    return False


def runlog():
    p = PROJ / "run.log"
    return p.read_text(errors="replace") if p.exists() else ""


def prompt():
    p = PROJ / "prompt_latest.txt"
    return p.read_text(errors="replace") if p.exists() else ""


def state():
    # the snapshot is AGG-owned, so it lives in agg/private/ (agg/state/ is the worker's half and a
    # confined worker could otherwise forge the scoreboard this UI trusts); mirrors e2e.sh's snap().
    p = PROJ / "agg" / "private" / "state.json"
    return json.loads(p.read_text()) if p.exists() else {}


def shot(page, name):
    if A.shots:
        Path(A.shots).mkdir(parents=True, exist_ok=True)
        page.screenshot(path=str(Path(A.shots) / f"{name}.png"), full_page=True)


with sync_playwright() as pw:
    browser = pw.chromium.launch(headless=not A.headed)
    page = browser.new_page(viewport={"width": 1280, "height": 900})
    # the Stop button opens a confirm() — accept it, otherwise the click blocks forever
    page.on("dialog", lambda d: d.accept())

    console_errors = []
    page.on("pageerror", lambda e: console_errors.append(str(e)))
    page.on("console", lambda m: console_errors.append(m.text) if m.type == "error" else None)

    # ── the page loads and hydrates ───────────────────────────────────────────
    page.goto(A.url, wait_until="networkidle")
    check("AgenticGoGo" in page.content(), "page loads and renders the app shell")
    expect(page.locator("header.hdr")).to_be_visible()
    ok("header is visible")

    # the loop is live: header must show a four-stage phase, not the retired vocabulary
    status = page.locator("header .status").inner_text().strip()
    check(
        re.search(r"\b(inject|run|verify|gate|backoff)\b", status) is not None,
        f"header shows a four-stage phase (got {status!r})",
    )
    check("judging" not in status and status != "running", "header never shows the retired phase names")

    proj = page.locator("header .proj").inner_text().strip()
    check(proj == PROJ.name, f"header shows the project name (got {proj!r})")

    # goals panel rendered from the live snapshot
    body = page.content()
    check("worked" in body, "goals panel renders the goal id from the live snapshot")
    shot(page, "01-live")

    # ── controls are enabled while a loop is live ────────────────────────────
    pause = page.get_by_role("button", name=re.compile("Pause"))
    resume = page.get_by_role("button", name=re.compile("Resume"))
    inject = page.get_by_role("button", name=re.compile("Inject"))
    budget = page.get_by_role("button", name=re.compile("Budget"))
    stop = page.get_by_role("button", name=re.compile("Stop"))
    for b, n in [(pause, "Pause"), (resume, "Resume"), (inject, "Inject…"), (budget, "Budget…"), (stop, "Stop")]:
        check(b.is_enabled(), f"{n} button is enabled while the loop is live")

    # ── ⏸ Pause ──────────────────────────────────────────────────────────────
    pause.click()
    expect(page.locator(".feedback")).to_contain_text("pause queued", timeout=10_000)
    ok("clicking ⏸ Pause shows the queued feedback")
    waitfor("…and the real loop parks in INJECT", lambda: "pause → waiting for resume/stop" in runlog())
    check(state().get("phase") == "inject", "…and the published phase is inject while paused")
    shot(page, "02-paused")

    # ── ▶ Resume ─────────────────────────────────────────────────────────────
    resume.click()
    expect(page.locator(".feedback")).to_contain_text("resume queued", timeout=10_000)
    ok("clicking ▶ Resume shows the queued feedback")
    waitfor("…and the real loop continues", lambda: "resume → continuing" in runlog())

    # ── ✎ Inject… : open, type, submit ───────────────────────────────────────
    inject.click()
    ta = page.locator("textarea")
    expect(ta).to_be_visible()
    ok("clicking ✎ Inject… opens the instruction textarea")

    submit = page.get_by_role("button", name="Inject", exact=True)
    check(not submit.is_enabled(), "…the Inject submit button is disabled while the text is empty")
    ta.fill("BROWSER_MARKER_QQQ")
    check(submit.is_enabled(), "…and enabled once there is text")
    submit.click()
    expect(page.locator(".feedback")).to_contain_text("instruction queued", timeout=10_000)
    ok("…submitting shows the queued feedback")
    expect(ta).to_have_count(0)
    ok("…and the textarea closes")
    waitfor("…the instruction reaches the NEXT worker's prompt", lambda: "BROWSER_MARKER_QQQ" in prompt())
    check(
        "HIGH-PRIORITY OPERATOR INSTRUCTION" in prompt(),
        "…as a HIGH-PRIORITY header, above the resume prompt",
    )
    shot(page, "03-injected")

    # ── ◫ Budget… : validation then a real value ─────────────────────────────
    budget.click()
    inp = page.locator("input")
    expect(inp).to_be_visible()
    ok("clicking ◫ Budget… opens the ceiling input")
    inp.fill("-5")
    page.get_by_role("button", name="Set budget").click()
    expect(page.locator(".feedback.err")).to_contain_text("non-negative", timeout=10_000)
    ok("…a negative budget is rejected client-side with an error")
    inp.fill("999999")
    page.get_by_role("button", name="Set budget").click()
    expect(page.locator(".feedback")).to_contain_text("token budget set to 999999", timeout=10_000)
    ok("…a valid budget is accepted")
    shot(page, "04-budget")

    # ── ⏹ Stop (confirm() is auto-accepted) ──────────────────────────────────
    stop.click()
    expect(page.locator(".feedback")).to_contain_text("stop queued", timeout=10_000)
    ok("clicking ⏹ Stop (and confirming) shows the queued feedback")
    waitfor("…the real loop stops", lambda: state().get("finished") is True)
    check(
        state().get("finish_reason") == "stopped via bus: stopped from web",
        "…with the finish reason the browser sent",
        f"got {state().get('finish_reason')!r}",
    )

    # ── the UI notices the loop is gone: buttons disable, status goes idle ───
    def idle():
        page.reload(wait_until="networkidle")
        return "idle" in page.locator("header .status").inner_text().lower()

    waitfor("…and the header falls back to 'idle'", idle, secs=30)
    check(not page.get_by_role("button", name=re.compile("Pause")).is_enabled(),
          "…and every control is disabled again")
    check("no loop running" in page.content(), "…with a 'no loop running' hint")
    shot(page, "05-idle")

    check(not console_errors, "no uncaught JS errors on any interaction",
          "; ".join(console_errors[:3]))

    browser.close()

print(f"\n  browser: passed {PASS}  failed {FAIL}")
if FAIL:
    for f in FAILED:
        print(f"    • {f}")
    sys.exit(1)

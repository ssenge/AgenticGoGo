"""Isolated browser check: can a human ANSWER an open ask from the web UI?

Self-contained — it builds its own project, mints a real ask through `agg hil`, starts its own
`agg serve` and SvelteKit app on free ports, and drives Chromium. Nothing is shared with the other
web fixtures, so it cannot perturb them (an earlier version of the API check did exactly that).

    scripts/web_ask.py [--root <repo>]      exit 0 = every check passed

Covers the reply channel end to end: the panel renders an open ask, a click on a recorded option
records the answer in the ledger attributed to the web, and the panel then clears — which only works
because `DashboardState::read` refreshes asks from the ledger rather than trusting a snapshot that
nothing republishes once the workflow has ended.
"""
import json, re, subprocess, sys, time, os, socket
from pathlib import Path
from playwright.sync_api import sync_playwright

import argparse
_ap = argparse.ArgumentParser()
_ap.add_argument("--root", default=str(Path(__file__).resolve().parent.parent))
ROOT = Path(_ap.parse_args().root)
AGG  = ROOT / "target/debug/agg"
P    = Path(os.environ.get("TMPDIR", "/tmp")) / f"agg-webask.{os.getpid()}"; subprocess.run(["rm","-rf",str(P)]); (P/"agg/judges").mkdir(parents=True)
(P/"bin").mkdir()

def port():
    s = socket.socket(); s.bind(("127.0.0.1", 0)); p = s.getsockname()[1]; s.close(); return p

# a project whose goal never closes, so a session always runs
(P/"agg/judges/never.sh").write_text('#!/bin/sh\nprintf %s \'{"met":false}\'\n'); os.chmod(P/"agg/judges/never.sh", 0o755)
(P/"agg/agg.yaml").write_text(
    "project: webask\ndefaults: { model: fake }\nsteps: { worker: {} }\n"
    "sequence: { steps: [worker], done_if: \"never\" }\nsummary: { enabled: false }\n")
(P/"bin/claude").write_text('#!/bin/sh\nfor a in "$@"; do [ "$a" = "--version" ] && { echo "fake 0.0.0"; exit 0; }; done\n'
                            'printf %s \'{"type":"result","is_error":false,"result":"x","usage":{"output_tokens":1}}\'\n')
os.chmod(P/"bin/claude", 0o755)
for c in (["git","init","-q","."],["git","config","user.email","t@t.t"],["git","config","user.name","t"],
          ["git","add","-A"],["git","commit","-qm","i"]):
    subprocess.run(c, cwd=P, check=False)

env = {**os.environ, "PATH": f"{P}/bin:{os.environ['PATH']}"}
run = lambda *a: subprocess.run([str(AGG), *a], cwd=P, env=env, capture_output=True, text=True)

# the worker asks; the loop promotes it
run("hil", "choose", "Which store for billing?", "--option", "postgres", "--option", "sqlite")
run("run", "--max-sessions", "1")
ledger = (P/"agg/private/asks.jsonl").read_text()
ask_id = re.search(r'"id":"([a-f0-9]+)"', ledger).group(1)
print(f"open ask: {ask_id}")

aport, wport = port(), port()
srv = subprocess.Popen([str(AGG), "serve", "--port", str(aport)], cwd=P, env=env,
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
web = subprocess.Popen(["node", "build/index.js"], cwd=ROOT/"src/web",
                       env={**os.environ, "AGG_API": f"http://127.0.0.1:{aport}", "PORT": str(wport)},
                       stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
fails = []
def check(cond, what):
    print(("  ✔ " if cond else "  ✘ ") + what)
    if not cond: fails.append(what)
try:
    for _ in range(60):
        try:
            import urllib.request; urllib.request.urlopen(f"http://127.0.0.1:{wport}/", timeout=1); break
        except Exception: time.sleep(0.5)

    with sync_playwright() as pw:
        b = pw.chromium.launch()
        pg = b.new_page()
        pg.goto(f"http://127.0.0.1:{wport}/", wait_until="networkidle")
        pg.wait_for_timeout(1500)

        check("Waiting on you" in pg.content(), "the Asks panel renders when an ask is open")
        check("Which store for billing?" in pg.content(), "…showing the question")
        check(pg.get_by_role("button", name="postgres").is_visible(), "…with a button per recorded option")
        # the rest of the page must still render — a throwing component takes its siblings with it
        check(pg.get_by_role("button", name=re.compile("Pause")).count() > 0, "…and the normal controls still render")

        pg.get_by_role("button", name="sqlite").click()
        pg.wait_for_timeout(2500)
        led = (P/"agg/private/asks.jsonl").read_text()
        check('"answer":"sqlite"' in led, "clicking an option ANSWERS the ask in the ledger")
        check('"by":"web"' in led, "…attributed to the web")
        check("Waiting on you" not in pg.content(), "…and the panel clears once the loop republishes")
        b.close()
finally:
    srv.terminate(); web.terminate()
print("FAILED:" if fails else "ALL GREEN"); [print(" -", f) for f in fails]
sys.exit(1 if fails else 0)

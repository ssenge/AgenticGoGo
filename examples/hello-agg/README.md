# hello-agg — the smallest possible loop

The whole idea in four files: a worker does a task, a **judge** (any script that prints one
line of JSON `{"met": …}`) checks it, and the loop repeats until the judge says met.

`add.py` starts WRONG on purpose (prints 3) so you can watch the correction loop happen.

## Run it
```bash
cp AGG_RESUME.md.template AGG_RESUME.md   # the worker's standing instruction
chmod +x check.sh
agg run
```

The judge rejects `3` → the worker edits `add.py` to `print(1 + 1)` → the judge sees `2` →
`met:true` → the loop stops. That's the entire model.

## Files
- `add.py` — the (broken) target the worker fixes
- `check.sh` — the judge (prints `{"met":...}`)
- `goals.yaml` — one binary goal, `stop_when: prints_two`
- `agg.yaml` — minimal config (haiku worker)
- `AGG_RESUME.md.template` — copy to `AGG_RESUME.md` (the live prompt is gitignored)

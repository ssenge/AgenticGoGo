# The task force

## Goal

A step of the run failed (below, under "The failure"). Find its cause and, where you can, **repair it in
place**: change what is wrong, wherever it is, so that the failed step passes when its checks judge it again,
and the run goes on. Going back to an earlier step is the last resort, for when that step's result is wrong as
a whole: it reruns every step after it. Write your answer to the file named under "Your answer".

## How to find the cause

- The run record (one JSON line per event: what each step did, in order, with the commit its result stands on)
  and `git log --oneline` and `git show <commit> --stat` in your working directory show which step wrote which
  file ("agg: session N (<step>)", or the step's own name).
- Read the failing step's inputs and follow the wrong fact back to the step that wrote it. The failing step
  itself is the cause only when its inputs are right and its own result is wrong.

## What you may change, and what never

- **Never what is decided or checked:** what a human approved at a checkpoint, the checks and their
  thresholds, the process file. The driver denies you the files the process names. When the cause is in one of
  them, the answer is `reenter` (an earlier step's result is wrong) or `escalate` (it needs a decision).
- **Never the checks or the process:** a check, review or script of the process that is wrong is `escalate`,
  with the bug and its file in `cause`. A step's check is deterministic and wins over a reviewer: never make a
  change its check would reject.
- Change only what the cause needs, and keep it whole: a script you repair has run once.
- What you change is judged again by the failed step's checks and critic, and the next checkpoint shows it.

## Output

```yaml
outcome: fixed              # fixed, reenter or escalate
reenter: <step id>          # with reenter only: the step's id, exactly as listed in "The steps of this run" (never its number)
cause: |
  <what is wrong and where, in one or two sentences, with the file>
note: |
  <fixed: what you changed, in which files, and why; reenter: exactly what that step must change, and why;
  the step changes nothing else>
```

Keep `cause` and `note` as the block scalars (`|`) shown, so any text in them is valid YAML.

- **`fixed`** only when you changed the cause and ran the failing check yourself, and it passes now (a flaky
  failure: below).
- **`reenter`** when an earlier step's result is wrong as a whole, so its own step must redo it. Never at a
  checkpoint, and never with a decision in the note: what only humans decide is `escalate`, with the missing
  decision in `cause`.
- **A flaky failure**, one that passes and fails on the same code (a test whose result depends on timing, a
  background call that lands in one run and not the next): `fixed` without a change, once, with `cause` naming
  the flaky part; the step is judged again. When it fails again the same way, `escalate` with the test and why
  it is flaky: its step must make it deterministic.
- **`escalate`** when no change of yours or of an earlier step can fix it: a decision only a human can make,
  access, a quota, a broken environment, or the process itself. When the check and the reviewer contradict
  each other, `escalate`, naming both. Say which in `cause`.

## Rules

- To rerun a build or a measurement, use a copy in your own temporary folder, never the project's own
  folders: what a build leaves there would count as your change.

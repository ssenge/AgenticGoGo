# Mixed agents

One process, two kinds of agent: the **worker** is a command agent (a CLI described in YAML, here a shell script that
stands in for one), the **reviewer** is Claude. Relentless drives each as it is: the worker's output lines are shown and its
exit code decides success, the reviewer makes read-only one-shot calls.

```
rlt process examples/mixed-agents/process.yaml --scenario examples/mixed-agents/scenario.yaml \
  --projects examples/mixed-agents/projects
```

plays it without tokens (the scenario scripts the reviewer and the human at the checkpoint).

Swap the worker for a real CLI in `projects/demo/tools.yaml`: `gemini`, `opencode`, `qwen` and `pi` are shipped as
**experimental** presets (see [docs/BACKENDS.md](../../docs/BACKENDS.md#command-agents-and-the-experimental-presets)); any
other CLI is described under `rlt: agents:` in `process.yaml`.

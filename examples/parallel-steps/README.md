# Different steps at once

A `kind: parallel` step runs its children at once, each as a lane in a worktree of its own, and joins them in the order they
are listed. Here the guide and the slides are written by two agents at the same time; a script step then collects both.

Run it on a project (a git repository holding `tools.yaml` and `agents/`; copy `projects/demo`):

```bash
rlt process examples/parallel-steps/process.yaml path/to/project
```

**Try it without tokens.** The writer is a script that sleeps two seconds, and a scenario plays the process around it:

```bash
rlt process examples/parallel-steps/process.yaml --scenario examples/parallel-steps/scenario.yaml --projects examples/parallel-steps/projects
```

`parallel: { max: 2 }` is optional (without it every child runs at once) and takes the same keys as a parallel `each`:
`max`, `fail_fast`, `seed` and `target`. Nothing parallel and no checkpoint can be inside a lane: for more than this (a lane that
starts lanes, or the best of several tries) drive the engine from Rust with `fork()` and `merge()`
([the Rust API](../../docs/RUST_API.md#forks)).

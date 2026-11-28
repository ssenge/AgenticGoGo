# A session on another machine

The same process as any, with the work in a one-item `each` whose `parallel.target` is a host: the run sends the project's clone
there, `rlt lane` runs the step on it (detached: a dropped connection does not end it), and the result comes back and is joined.

Prepare the host once: `rlt install-remote build-box`, the agent CLI on it, a folder `/srv/rlt` the user can write. Then:

```
rlt process examples/remote-session/process.yaml <project>      # ANTHROPIC_API_KEY (or your agent's key) in the environment
```

The key travels by name, in a file of mode 0600 inside the ssh stream; it is not in a command line or a log. The operator's `rlt stop`
kills the remote lane; `--resume` polls it again instead of starting a second. See [docs/PROCESS.md](../../docs/PROCESS.md#where-lanes-run).

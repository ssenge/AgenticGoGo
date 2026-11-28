# A disposable machine around each lane

Make a machine, run the lane on it, bring the clone back, remove the machine. `<cloud>` stands for your provider's CLI; the
four commands are all `rlt` needs.

```yaml
parallel:
  max: 3
  target:
    kind: command
    launch: sh scripts/vm-launch.sh {lane_dir} {lane}
    poll: sh scripts/vm-poll.sh {lane}
    kill: sh scripts/vm-remove.sh {lane}
    fetch: sh scripts/vm-fetch.sh {lane_dir} {lane}     # fetches, then removes the machine
```

- `vm-launch.sh`: `<cloud> create-machine --name rlt-<lane> …`, wait until it accepts ssh, send `{lane_dir}` (tar over ssh), start
  `rlt lane --spec rlt/private/lane-spec.json` detached there.
- `vm-poll.sh`: exit 0 when `rlt/private/lane-result.json` exists on the machine, 1 otherwise.
- `vm-fetch.sh`: tar the clone back into `{lane_dir}`, then `<cloud> delete-machine`.
- `vm-remove.sh`: `<cloud> delete-machine` (the operator stopped the run).

The machine needs `git`, `rlt` ([`rlt install-remote`](../PROCESS.md#where-lanes-run)) and the agent CLI. Pass credentials by
`pass_env` (the `command` target's launch command inherits them in its environment), never in a script.

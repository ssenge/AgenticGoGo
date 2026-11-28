# Recipes: lane targets of your own

The `command` target (`parallel.target: { kind: command, launch, poll, fetch, kill }`, [PROCESS.md](../PROCESS.md#where-lanes-run)) runs a
lane wherever you can start `rlt lane --spec rlt/private/lane-spec.json` in a copy of `{lane_dir}`. These recipes are
documentation, not code of the engine; adapt them.

- [ssh-launcher.md](ssh-launcher.md): the same as the built-in `ssh` target, written out, for a host that needs more than it offers.
- [cloud-vm.md](cloud-vm.md): a disposable machine made and removed around each lane with a provider's CLI.

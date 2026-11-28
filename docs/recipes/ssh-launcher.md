# An ssh launcher, written out

The built-in `ssh` target does this; write it yourself when the host needs a jump host, a different shell or a queue in front.

```yaml
parallel:
  max: 4
  target:
    kind: command
    launch: |
      tar -C {lane_dir} -cf - . | ssh build-box 'mkdir -p /srv/rlt/{lane} && tar -C /srv/rlt/{lane} -xf - &&
        cd /srv/rlt/{lane} && (setsid nohup rlt lane --spec rlt/private/lane-spec.json > rlt/private/lane.log 2>&1 < /dev/null &
        echo $! > rlt/private/lane.pid)'
    poll: ssh build-box 'test -f /srv/rlt/{lane}/rlt/private/lane-result.json'
    kill: ssh build-box 'kill $(cat /srv/rlt/{lane}/rlt/private/lane.pid)'
    fetch: ssh build-box 'tar -C /srv/rlt/{lane} -cf - .' | tar -C {lane_dir} -xf -
```

Keep the lane's log and pid under `rlt/private/` (git ignores it: a lane's session commits every other new file). A credential
goes into a file the lane sources, sent in the same stream, never into a command line.

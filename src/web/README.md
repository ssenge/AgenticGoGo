# rlt-web — the Relentless web interface

A **standalone** SvelteKit app to monitor and steer a running `rlt` loop from a browser — locally
now, deployable (e.g. Vercel) later. It is decoupled from the `rlt` binary: it talks to `rlt` only
over HTTP.

```
browser ──► this app (SvelteKit) ──HTTP──► rlt serve ──► rlt/private/ ◄── rlt run (the loop)
```

The app's own server endpoints (a BFF, under `src/routes/api/`) proxy to `rlt serve`; the browser
never calls `rlt` directly. That keeps rlt's API off the public internet and gives one place for
auth in the remote phase.

## Run it locally (two processes)

1. **Start the rlt API** in your project (the one running `rlt run`):
   ```
   rlt serve                 # serves http://127.0.0.1:7878
   ```
2. **Start this web app:**
   ```
   cd src/web
   npm install
   npm run dev               # serves http://localhost:5173
   ```
3. Open <http://localhost:5173>.

If `rlt serve` runs on a different host/port, set `RLT_API`:
```
RLT_API=http://127.0.0.1:9000 npm run dev
```

## What it shows / does

- **Monitor:** project · model · session · uptime · tokens; a live goal scoreboard (met /
  in-progress / regressed / pending, with the honest `(API-eq)` usage figure) + progress bar; a
  real-time activity feed; memory size.
- **Steer:** Pause · Resume · Inject an instruction · adjust the token Budget · Stop — the same bus
  control verbs as `rlt send`. Controls are disabled when no loop is running (the API returns 409).

- **Process runs** (`/process`): with `rlt serve --projects <folder> --process <file>`, the projects of the folder and the
  process's new-project form; per project Start fresh · Resume · Stop, the checkpoints waiting for you (their document,
  open points, decisions so far, a box for your decisions, Yes/No), the map (click a step for its facts) and where the
  time went. See `docs/PROCESS.md` ("The web UI of a process").

## Remote deploy (later)

- Build: `npm run build` (adapter-node). Swap `svelte.config.js` to `adapter-vercel` for Vercel.
- Set `RLT_API` to a reachable `rlt serve`, and `RLT_TOKEN` if you started it with `--token`.
- Start `rlt serve --host 0.0.0.0 --token <secret> --cors-origin https://your-app`.

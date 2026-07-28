# agg-web — the AgenticGoGo web interface

A **standalone** SvelteKit app to monitor and steer a running `agg` loop from a browser — locally
now, deployable (e.g. Vercel) later. It is decoupled from the `agg` binary: it talks to `agg` only
over HTTP.

```
browser ──► this app (SvelteKit) ──HTTP──► agg serve ──► agg/private/ ◄── agg run (the loop)
```

The app's own server endpoints (a BFF, under `src/routes/api/`) proxy to `agg serve`; the browser
never calls `agg` directly. That keeps agg's API off the public internet and gives one place for
auth in the remote phase.

## Run it locally (two processes)

1. **Start the agg API** in your project (the one running `agg run`):
   ```
   agg serve                 # serves http://127.0.0.1:7878
   ```
2. **Start this web app:**
   ```
   cd src/web
   npm install
   npm run dev               # serves http://localhost:5173
   ```
3. Open <http://localhost:5173>.

If `agg serve` runs on a different host/port, set `AGG_API`:
```
AGG_API=http://127.0.0.1:9000 npm run dev
```

## What it shows / does

- **Monitor:** project · model · session · uptime · tokens; a live goal scoreboard (met /
  in-progress / regressed / pending, with the honest `(API-eq)` usage figure) + progress bar; a
  real-time activity feed; memory size.
- **Steer:** Pause · Resume · Inject an instruction · adjust the token Budget · Stop — the same bus
  control verbs as `agg send`. Controls are disabled when no loop is running (the API returns 409).

## Remote deploy (later)

- Build: `npm run build` (adapter-node). Swap `svelte.config.js` to `adapter-vercel` for Vercel.
- Set `AGG_API` to a reachable `agg serve`, and `AGG_TOKEN` if you started it with `--token`.
- Start `agg serve --host 0.0.0.0 --token <secret> --cors-origin https://your-app`.

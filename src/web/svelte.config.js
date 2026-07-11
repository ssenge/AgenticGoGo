import adapter from '@sveltejs/adapter-node';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  kit: {
    // adapter-node: the standalone tool runs as its own Node server (local now, deployable later).
    // Swap to adapter-vercel for a Vercel deploy in the remote phase — the app code is unchanged.
    adapter: adapter()
  }
};

export default config;

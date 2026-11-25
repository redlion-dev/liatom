import { defineConfig } from "vite";

// In dev the local testchain RPC is reached through the dev server (same origin, no CORS setup):
// run `ssh -N -L 26657:localhost:26657 <dev-vm>` first.
export default defineConfig({
  base: "./", // relative assets: the app can live under any path, e.g. /atom-liquid-staking/
  define: { global: "globalThis" },
  server: {
    proxy: {
      "/rpc": { target: "http://localhost:26657", rewrite: (p) => p.replace(/^\/rpc/, "") },
    },
  },
  build: { target: "es2022" },
});

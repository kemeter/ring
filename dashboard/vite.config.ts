import { readFileSync } from 'node:fs';
import adapter from '@sveltejs/adapter-static';
import { sveltekit } from '@sveltejs/kit/vite';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';
import { defineConfig } from 'vite';

// Single source of truth for the displayed version: the workspace Cargo.toml.
// Read at build time and exposed as __RING_VERSION__ so the menu never drifts
// from the actual project version.
function ringVersion(): string {
  try {
    const cargo = readFileSync(new URL('../Cargo.toml', import.meta.url), 'utf8');
    const m = cargo.match(/^\s*version\s*=\s*"([^"]+)"/m);
    return m ? m[1] : 'dev';
  } catch {
    return 'dev';
  }
}

export default defineConfig({
  define: {
    __RING_VERSION__: JSON.stringify(ringVersion())
  },
  plugins: [
    sveltekit({
      preprocess: vitePreprocess(),
      adapter: adapter({
        pages: 'build',
        assets: 'build',
        fallback: 'index.html',
        precompress: false,
        strict: true
      })
      // No `paths: { relative: true }` — adapter-static rewrites our absolute
      // `/foo` URLs to `./foo` relative to the current page, which breaks
      // dynamic routes (`./deployments/abc` from `/deployments` resolves to
      // `/deployments/deployments/abc`). Keep paths absolute, served from
      // root by the Rust backend.
    })
  ],
  server: {
    port: 5173,
    // In dev, requests to /api/* hit the Rust backend rather than the
    // Vite dev server. Matches the production routing where `ring dashboard`
    // (or the embedded mode) proxies the same prefix to the real API.
    proxy: {
      '/api': {
        target: 'http://127.0.0.1:3030',
        changeOrigin: true,
        rewrite: (path) => path.replace(/^\/api/, '')
      }
    }
  }
});

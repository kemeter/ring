import adapter from '@sveltejs/adapter-static';
import { vitePreprocess } from '@sveltejs/vite-plugin-svelte';

/** @type {import('@sveltejs/kit').Config} */
const config = {
  preprocess: vitePreprocess(),
  kit: {
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
  }
};

export default config;

import { sveltekit } from '@sveltejs/kit/vite';
import { defineConfig } from 'vite';

export default defineConfig({
  plugins: [sveltekit()],
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

import { defineConfig, devices } from '@playwright/test';

// Functional tests: a real Chromium against a real `ring server start` with
// the embedded dashboard (see e2e/ring-server.sh). They need the dashboard
// built (`bun run build`), the Rust binary built (`cargo build`, or RING_BIN)
// and a reachable Docker daemon.
export const API_URL = 'http://127.0.0.1:3130';
export const DASHBOARD_URL = 'http://127.0.0.1:3131';

export default defineConfig({
  testDir: './e2e',
  // One server for the whole suite: tests use their own namespaces, but
  // running them one at a time keeps Docker and the scheduler predictable.
  workers: 1,
  timeout: 90_000,
  expect: { timeout: 15_000 },
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? [['github'], ['list']] : 'list',
  use: {
    baseURL: DASHBOARD_URL,
    trace: 'retain-on-failure',
    ...devices['Desktop Chrome']
  },
  webServer: {
    command: 'bash e2e/ring-server.sh',
    url: `${DASHBOARD_URL}/login`,
    reuseExistingServer: !process.env.CI,
    timeout: 120_000,
    stdout: 'pipe',
    stderr: 'pipe'
  }
});

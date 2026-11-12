import { type ChildProcess, spawn } from 'node:child_process';
import { mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { expect, test } from './fixtures';

const LOCAL_PORT = 3132;

/** Starts `ring dashboard` against the suite's server with the admin token,
 *  and resolves with the link it prints, which holds the session secret. */
function startLocalDashboard(token: string): Promise<{ link: string; stop: () => void }> {
  const configDir = mkdtempSync(join(tmpdir(), 'ring-e2e-local-'));
  writeFileSync(
    join(configDir, 'config.toml'),
    [
      '[contexts.default]',
      'current = true',
      'host = "127.0.0.1"',
      'api.scheme = "http"',
      'api.port = 3130',
      ''
    ].join('\n')
  );
  const child: ChildProcess = spawn(
    process.env.RING_BIN ?? '../target/debug/ring',
    ['dashboard', '--listen', `127.0.0.1:${LOCAL_PORT}`, '--no-open'],
    { env: { ...process.env, RING_CONFIG_DIR: configDir, RING_TOKEN: token } }
  );
  const stop = () => {
    child.kill();
    rmSync(configDir, { recursive: true, force: true });
  };

  return new Promise((resolve, reject) => {
    let output = '';
    const timer = setTimeout(() => {
      stop();
      reject(new Error(`ring dashboard printed no link:\n${output}`));
    }, 20_000);
    child.stdout?.on('data', (chunk: Buffer) => {
      output += chunk.toString();
      const link = output.match(/Dashboard:\s+(\S+)/)?.[1];
      if (link) {
        clearTimeout(timer);
        resolve({ link, stop });
      }
    });
    child.on('exit', (code) => {
      clearTimeout(timer);
      reject(new Error(`ring dashboard exited with ${code}:\n${output}`));
    });
  });
}

test.describe('ring dashboard', () => {
  let local: { link: string; stop: () => void };

  test.beforeAll(async ({ request }) => {
    const response = await request.post('http://127.0.0.1:3130/login', {
      data: { username: 'admin', password: 'changeme' }
    });
    local = await startLocalDashboard((await response.json()).token);
  });

  test.afterAll(() => local?.stop());

  test('refuses a browser that did not open its link', async ({ page }) => {
    const response = await page.goto(`http://127.0.0.1:${LOCAL_PORT}/deployments`);
    expect(response?.status()).toBe(403);
    await expect(page.getByText('Open the dashboard with the link')).toBeVisible();
  });

  test('opens signed in with the CLI token, without a login screen', async ({ page }) => {
    await page.goto(local.link);
    // The secret moves to a cookie and leaves the address bar.
    await expect(page).toHaveURL(`http://127.0.0.1:${LOCAL_PORT}/`);
    await expect(page.getByRole('heading', { name: 'Overview' })).toBeVisible();
    await expect(page.getByRole('button', { name: 'Sign out' })).toHaveCount(0);

    await page.goto(`http://127.0.0.1:${LOCAL_PORT}/deployments`);
    await expect(page.getByRole('heading', { name: 'Deployments' })).toBeVisible();
  });

  test('refuses a request from another origin, even with the session', async ({ request }) => {
    const secret = new URL(local.link).searchParams.get('session');
    const response = await request.get(`http://127.0.0.1:${LOCAL_PORT}/api/deployments`, {
      headers: {
        Cookie: `ring_dashboard_session=${secret}`,
        Origin: 'http://127.0.0.1:3131'
      }
    });
    expect(response.status()).toBe(403);
  });
});

import { type APIRequestContext, test as base, expect } from '@playwright/test';
import { API_URL } from '../playwright.config';

/** The admin account the first migration seeds. */
export const ADMIN = { username: 'admin', password: 'changeme' };

export interface RingApi {
  token: string;
  get<T = unknown>(path: string): Promise<T>;
  post<T = unknown>(path: string, body: unknown): Promise<T>;
  /** The raw status of a request made with another token. */
  statusWith(token: string, path: string): Promise<number>;
}

async function login(request: APIRequestContext): Promise<string> {
  const response = await request.post(`${API_URL}/login`, { data: ADMIN });
  expect(response.ok(), await response.text()).toBe(true);
  return (await response.json()).token;
}

interface Fixtures {
  /** Calls the API directly, as the admin, to arrange and check state. */
  api: RingApi;
  /** A namespace of this test's own, so tests never see each other's data. */
  namespace: string;
  /** Fails the test when the page reports a CSP violation. */
  cspGuard: undefined;
}

export const test = base.extend<Fixtures>({
  api: async ({ request }, use) => {
    const token = await login(request);
    const headers = { Authorization: `Bearer ${token}` };
    const check = async (response: Awaited<ReturnType<APIRequestContext['get']>>) => {
      expect(response.ok(), `${response.url()}: ${await response.text()}`).toBe(true);
      const text = await response.text();
      return text ? JSON.parse(text) : undefined;
    };
    await use({
      token,
      get: async (path) => check(await request.get(`${API_URL}${path}`, { headers })),
      post: async (path, body) =>
        check(await request.post(`${API_URL}${path}`, { headers, data: body })),
      statusWith: async (other, path) =>
        (
          await request.get(`${API_URL}${path}`, { headers: { Authorization: `Bearer ${other}` } })
        ).status()
    });
  },

  namespace: async ({ api }, use, testInfo) => {
    const name = `e2e-${testInfo.workerIndex}-${Date.now().toString(36)}`;
    await api.post('/namespaces', { name });
    await use(name);
  },

  cspGuard: [
    async ({ page }, use) => {
      const violations: string[] = [];
      page.on('console', (message) => {
        if (message.type() === 'error' && /Content Security Policy/i.test(message.text())) {
          violations.push(message.text());
        }
      });
      await use(undefined);
      expect(violations, 'CSP violations').toEqual([]);
    },
    { auto: true }
  ]
});

/** Same fixtures, with the page already signed in as the admin. */
export const authedTest = test.extend({
  page: async ({ page, api }, use) => {
    await page.addInitScript((token) => localStorage.setItem('ring.token', token), api.token);
    await use(page);
  }
});

export { expect };

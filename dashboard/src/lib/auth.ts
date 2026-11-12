// Auth is the same Bearer JWT the CLI uses. We store it in localStorage so
// the SPA stays decoupled from any server-rendered session — same approach
// as the Sozune dashboard.

const TOKEN_KEY = 'ring.token';

export function getToken(): string | null {
  if (typeof localStorage === 'undefined') {
    return null;
  }
  return localStorage.getItem(TOKEN_KEY);
}

export function setToken(token: string): void {
  if (typeof localStorage === 'undefined') {
    return;
  }
  localStorage.setItem(TOKEN_KEY, token);
}

export function clearToken(): void {
  if (typeof localStorage === 'undefined') {
    return;
  }
  localStorage.removeItem(TOKEN_KEY);
}

/** True under `ring dashboard`: the local proxy signs every request with the
 *  operator's CLI token, so there is no login and no token in the browser.
 *  The Rust server tells the mode through a `<meta>` tag on every page. */
export function isLocalMode(): boolean {
  if (typeof document === 'undefined') {
    return false;
  }
  return (
    document.querySelector('meta[name="ring-dashboard-mode"]')?.getAttribute('content') === 'local'
  );
}

/** Whether API calls can be made: signed in, or behind the local proxy. */
export function hasSession(): boolean {
  return isLocalMode() || getToken() !== null;
}

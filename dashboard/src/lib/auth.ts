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

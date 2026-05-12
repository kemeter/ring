// All API calls go through `/api/*`. In dev that's proxied by Vite to the
// local ring-server; in production (whether served by `ring dashboard` or
// the embedded mode), the Rust side proxies the same prefix to the real
// API. The dashboard JS therefore never needs to know the API URL.

import { clearToken, getToken } from './auth';

export interface Deployment {
  id: string;
  name: string;
  namespace: string;
  runtime: string;
  status: string;
  replicas: number;
  image: string;
}

export interface CurrentUser {
  id: string;
  username: string;
  status: string;
}

async function request<T>(path: string, init: RequestInit = {}): Promise<T> {
  const token = getToken();
  const headers = new Headers(init.headers);
  headers.set('Accept', 'application/json');
  if (init.body && !headers.has('Content-Type')) {
    headers.set('Content-Type', 'application/json');
  }
  if (token) {
    headers.set('Authorization', `Bearer ${token}`);
  }

  const res = await fetch(`/api${path}`, { ...init, headers });

  if (res.status === 401) {
    clearToken();
    if (typeof window !== 'undefined' && !window.location.pathname.endsWith('/')) {
      window.location.assign('./');
    }
    throw new Error('401 Unauthorized');
  }

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new Error(`${res.status} ${res.statusText}${text ? `: ${text}` : ''}`);
  }
  if (res.status === 204) {
    return undefined as T;
  }
  return res.json() as Promise<T>;
}

export async function login(username: string, password: string): Promise<string> {
  const res = await fetch('/api/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json' },
    body: JSON.stringify({ username, password })
  });
  if (!res.ok) {
    throw new Error(`${res.status} ${res.statusText}`);
  }
  const data = (await res.json()) as { token: string };
  return data.token;
}

export function listDeployments(): Promise<Deployment[]> {
  return request<Deployment[]>('/deployments');
}

export function getCurrentUser(): Promise<CurrentUser> {
  return request<CurrentUser>('/users/me');
}

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

export interface Namespace {
  id: string;
  name: string;
  created_at: string;
  updated_at: string | null;
}

export interface DeploymentPort {
  published: number;
  target: number;
  protocol?: string;
}

export interface DeploymentVolume {
  type: string;
  source?: string | null;
  key?: string | null;
  destination: string;
  driver: string;
  permission: string;
}

/** Discriminated union mirroring `enum HealthCheck` on the server. */
export type HealthCheck =
  | {
      type: 'tcp';
      port: number;
      interval: string;
      timeout: string;
      threshold: number;
      on_failure: string;
      readiness?: boolean;
      min_healthy_time?: string | null;
    }
  | {
      type: 'http';
      url: string;
      interval: string;
      timeout: string;
      threshold: number;
      on_failure: string;
      readiness?: boolean;
      min_healthy_time?: string | null;
    }
  | {
      type: 'command';
      command: string;
      interval: string;
      timeout: string;
      threshold: number;
      on_failure: string;
      readiness?: boolean;
      min_healthy_time?: string | null;
    };

export interface ResourceLimits {
  cpu?: string;
  memory?: string;
}

export interface DeploymentResources {
  limits?: ResourceLimits;
  requests?: ResourceLimits;
}

/** Either a literal string or a `{ secretRef: "name" }` reference. */
export type EnvValue = string | { secretRef: string };

export interface DeploymentDetail extends Deployment {
  created_at: string;
  updated_at: string;
  kind: string;
  restart_count: number;
  command: string[];
  ports: DeploymentPort[];
  labels: Record<string, string>;
  instances: string[];
  environment: Record<string, EnvValue>;
  volumes: DeploymentVolume[];
  health_checks: HealthCheck[];
  resources?: DeploymentResources | null;
  image_digest?: string | null;
  parent_id?: string | null;
  network?: unknown;
}

export interface DeploymentEvent {
  id?: string;
  deployment_id?: string;
  level?: string;
  message?: string;
  /** Server returns `timestamp` (RFC3339). `created_at` kept as a fallback
   *  for older API versions. */
  timestamp?: string;
  created_at?: string;
  component?: string;
  reason?: string;
  [key: string]: unknown;
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

export function listNamespaces(): Promise<Namespace[]> {
  return request<Namespace[]>('/namespaces');
}

export function getDeployment(id: string): Promise<DeploymentDetail> {
  return request<DeploymentDetail>(`/deployments/${encodeURIComponent(id)}`);
}

export function listDeploymentEvents(id: string): Promise<DeploymentEvent[]> {
  return request<DeploymentEvent[]>(`/deployments/${encodeURIComponent(id)}/events`);
}

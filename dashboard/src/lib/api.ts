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

export interface Secret {
  id: string;
  created_at: string;
  updated_at: string | null;
  namespace: string;
  name: string;
}

export interface Config {
  id: string;
  /** Server returns a SQL-style timestamp like `2026-05-13 19:26:27.93 UTC`,
   *  not RFC3339. Parse with `Date(s)` may not work on all browsers; pages
   *  fall back to the raw string when parsing fails. */
  created_at: string;
  /** Empty string when never updated; some endpoints return `null` instead. */
  updated_at: string | null;
  namespace: string;
  name: string;
  data: string;
  /** Free-form string set by the client (often `key=value,key=value`). The
   *  API stores it as-is and does not parse it. Empty string means no labels. */
  labels: string;
}

export interface LogEntry {
  instance: string;
  message: string;
  level: string;
  /** RFC3339 timestamp when the runtime tagged the line, when available. */
  timestamp?: string | null;
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

export interface AuditEntry {
  id: string;
  timestamp: string;
  user_id: string | null;
  action: string;
  target_type: string;
  target_name: string;
  namespace: string | null;
}

export function getNamespaceAudit(name: string, limit?: number): Promise<AuditEntry[]> {
  const q = limit ? `?limit=${limit}` : '';
  return request<AuditEntry[]>(`/namespaces/${encodeURIComponent(name)}/audit${q}`);
}

export function getDeployment(id: string): Promise<DeploymentDetail> {
  return request<DeploymentDetail>(`/deployments/${encodeURIComponent(id)}`);
}

export function listDeploymentEvents(id: string): Promise<DeploymentEvent[]> {
  return request<DeploymentEvent[]>(`/deployments/${encodeURIComponent(id)}/events`);
}

export function listSecrets(): Promise<Secret[]> {
  return request<Secret[]>('/secrets');
}

export function listConfigs(): Promise<Config[]> {
  return request<Config[]>('/configs');
}

export interface LogsQuery {
  tail?: number;
  since?: string;
  container?: string;
}

function logsUrl(id: string, query: LogsQuery & { follow?: boolean; ticket?: string }): string {
  const params = new URLSearchParams();
  if (query.tail !== undefined) {
    params.set('tail', String(query.tail));
  }
  if (query.since) {
    params.set('since', query.since);
  }
  if (query.container) {
    params.set('container', query.container);
  }
  if (query.follow) {
    params.set('follow', 'true');
  }
  if (query.ticket) {
    params.set('ticket', query.ticket);
  }
  const qs = params.toString();
  return `/api/deployments/${encodeURIComponent(id)}/logs${qs ? `?${qs}` : ''}`;
}

export function fetchLogsSnapshot(id: string, query: LogsQuery = {}): Promise<LogEntry[]> {
  return request<LogEntry[]>(
    `/deployments/${encodeURIComponent(id)}/logs${buildLogsQs({ ...query, follow: false })}`
  );
}

function buildLogsQs(query: LogsQuery & { follow?: boolean }): string {
  const params = new URLSearchParams();
  if (query.tail !== undefined) {
    params.set('tail', String(query.tail));
  }
  if (query.since) {
    params.set('since', query.since);
  }
  if (query.container) {
    params.set('container', query.container);
  }
  if (query.follow) {
    params.set('follow', 'true');
  }
  const qs = params.toString();
  return qs ? `?${qs}` : '';
}

/** Mint a single-use-ish stream ticket scoped to a specific resource.
 *  Required because `EventSource` cannot send an Authorization header. */
export function mintStreamTicket(scope: string): Promise<{ ticket: string; expires_in: number }> {
  return request<{ ticket: string; expires_in: number }>('/auth/stream-ticket', {
    method: 'POST',
    body: JSON.stringify({ scope })
  });
}

export interface LogStreamHandle {
  /** Stop reading and close the EventSource. Safe to call twice. */
  close(): void;
}

/** Opens a live log stream. Mints a ticket first (because EventSource can't
 *  set an Authorization header), then connects with `?ticket=…`.  The
 *  caller is responsible for calling `close()` to release the connection. */
export async function streamLogs(
  id: string,
  query: LogsQuery,
  onEntry: (entry: LogEntry) => void,
  onError?: (err: Event) => void
): Promise<LogStreamHandle> {
  const { ticket } = await mintStreamTicket(`deployment:logs:${id}`);
  const url = logsUrl(id, { ...query, follow: true, ticket });
  const es = new EventSource(url);
  es.onmessage = (ev) => {
    try {
      onEntry(JSON.parse(ev.data) as LogEntry);
    } catch {
      // Defensive: ignore malformed frames rather than killing the stream.
    }
  };
  if (onError) {
    es.onerror = onError;
  }
  return {
    close() {
      es.close();
    }
  };
}

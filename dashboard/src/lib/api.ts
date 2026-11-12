// All API calls go through `/api/*`. In dev that's proxied by Vite to the
// local ring-server; in production (whether served by `ring dashboard` or
// the embedded mode), the Rust side proxies the same prefix to the real
// API. The dashboard JS therefore never needs to know the API URL.

import { clearToken, getToken, isLocalMode } from './auth';

/** Why a deployment is backing off or failed. */
export interface DeploymentFailure {
  reason: string;
  /** Human-readable detail for `reason`. */
  message: string;
}

export interface Deployment {
  id: string;
  name: string;
  namespace: string;
  runtime: string;
  /** `pending`, `progressing`, `running`, `backing_off`, `completed`,
   *  `failed` or `deleted`. */
  status: string;
  /** Why the deployment is backing off or failed; null in any other status. */
  failure?: DeploymentFailure | null;
  replicas: number;
  image: string;
  /** SQL-style timestamp from the API; parse with `formatDate`. */
  created_at?: string;
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

/**
 * One running instance of a deployment. Mirrors `DeploymentInstance` from
 * `src/api/dto/deployment.rs`: `address` is the routable guest IP, omitted
 * when the instance has no reachable address (e.g. no published ports).
 */
export interface DeploymentInstance {
  id: string;
  address?: string;
}

export interface DeploymentDetail extends Deployment {
  created_at: string;
  updated_at: string;
  kind: string;
  restart_count: number;
  command: string[];
  ports: DeploymentPort[];
  labels: Record<string, string>;
  instances: DeploymentInstance[];
  environment: Record<string, EnvValue>;
  volumes: DeploymentVolume[];
  health_checks: HealthCheck[];
  resources?: DeploymentResources | null;
  image_digest?: string | null;
  parent_id?: string | null;
  network?: unknown;
}

export interface MemoryStats {
  usage_bytes: number;
  limit_bytes: number;
  usage_percent: number;
}

export interface NetworkStats {
  rx_bytes: number;
  tx_bytes: number;
  rx_packets: number;
  tx_packets: number;
}

export interface DiskIoStats {
  read_bytes: number;
  write_bytes: number;
}

export interface PidStats {
  current: number;
  limit: number;
}

export interface InstanceStats {
  instance_id: string;
  instance_name: string;
  cpu_usage_percent: number;
  memory: MemoryStats;
  network: NetworkStats;
  disk_io: DiskIoStats;
  pids: PidStats;
  restart_count: number;
}

/** Mirrors `DeploymentStatsOutput` from `src/api/dto/stats.rs`. */
export interface DeploymentStats {
  deployment_id: string;
  deployment_name: string;
  instance_count: number;
  total_cpu_usage_percent: number;
  total_memory: MemoryStats;
  total_network: NetworkStats;
  total_disk_io: DiskIoStats;
  total_pids: number;
  instances: InstanceStats[];
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
    if (isLocalMode()) {
      // Nothing to sign in to: it is the CLI token that is no longer valid.
      throw new Error(
        '401 Unauthorized: the CLI token of this context was refused, run `ring login`'
      );
    }
    clearToken();
    if (typeof window !== 'undefined' && !window.location.pathname.endsWith('/login')) {
      window.location.assign('/login');
    }
    throw new Error('401 Unauthorized');
  }

  if (!res.ok) {
    const text = await res.text().catch(() => '');
    throw new ApiError(res.status, res.statusText, text);
  }
  if (res.status === 204 || res.headers.get('content-length') === '0') {
    return undefined as T;
  }
  return res.json() as Promise<T>;
}

/** A refusal from the API, keeping its status and parsed body so a page can
 *  react to a specific case (a 409 naming the deployments in the way). */
export class ApiError extends Error {
  readonly status: number;
  readonly body: Record<string, unknown> | null;

  constructor(status: number, statusText: string, text: string) {
    super(`${status} ${statusText}${text ? `: ${errorDetail(text)}` : ''}`);
    this.status = status;
    try {
      this.body = JSON.parse(text);
    } catch {
      this.body = null;
    }
  }
}

/** The deployments a 409 says still use the resource, when it says so. */
export function blockingDeployments(error: unknown): string[] | null {
  if (!(error instanceof ApiError) || error.status !== 409) {
    return null;
  }
  const names = error.body?.deployments;
  return Array.isArray(names) ? names.map(String) : null;
}

/** The readable part of an error body. Validation failures are RFC 7807
 *  problem+json (`violations`); other refusals are `{ error, hint }` or
 *  `{ errors: [...] }`. Anything else is shown as received. */
function errorDetail(text: string): string {
  let body: {
    detail?: string;
    violations?: Array<{ property_path: string; message: string }>;
    error?: string;
    errors?: string[];
    message?: string;
    hint?: string;
  };
  try {
    body = JSON.parse(text);
  } catch {
    return text;
  }
  if (body.violations?.length) {
    return body.violations.map((v) => `${v.property_path}: ${v.message}`).join('; ');
  }
  const message = body.error ?? body.errors?.join('; ') ?? body.message ?? body.detail;
  if (!message) {
    return text;
  }
  return body.hint ? `${message} (${body.hint})` : message;
}

function jsonBody(body: unknown): RequestInit {
  return { body: JSON.stringify(body) };
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

// Revoke the current session server-side. Best-effort: the caller clears the
// local token regardless, but this ensures the token can't be replayed after
// sign-out. A 401 (token already invalid) is treated as success.
export async function logout(): Promise<void> {
  try {
    await request<void>('/logout', { method: 'POST' });
  } catch {
    // Already invalid or unreachable — the local clearToken() that follows is
    // what the user sees, so swallow and let sign-out proceed.
  }
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

/** Ask the server to tear a deployment down. The row is kept with a
 *  `deleted` status while the scheduler removes its instances. */
export function deleteDeployment(id: string): Promise<void> {
  return request<void>(`/deployments/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

export function listDeploymentEvents(id: string): Promise<DeploymentEvent[]> {
  return request<DeploymentEvent[]>(`/deployments/${encodeURIComponent(id)}/events`);
}

export function getDeploymentMetrics(id: string): Promise<DeploymentStats> {
  return request<DeploymentStats>(`/deployments/${encodeURIComponent(id)}/metrics`);
}

/** One recorded health-check probe. Mirrors `HealthCheckResultRecord` from
 *  `src/models/health_check_logs.rs`. `status` is `success` | `failed` |
 *  `timeout`; timestamps are RFC3339. */
export interface HealthCheckResult {
  id: string;
  deployment_id: string;
  check_type: string;
  status: string;
  message?: string | null;
  created_at: string;
  started_at: string;
  finished_at: string;
}

/** Recent probe history for a deployment, newest first (server default).
 *  `limit` caps how many records come back. */
export function getDeploymentHealthChecks(id: string, limit = 50): Promise<HealthCheckResult[]> {
  return request<HealthCheckResult[]>(
    `/deployments/${encodeURIComponent(id)}/health-checks?limit=${limit}`
  );
}

/** Mirrors `NodeRootDto` from `src/api/dto/node.rs`. Memory figures are GiB. */
export interface NodeInfo {
  hostname: string;
  os: string;
  arch: string;
  uptime: string;
  cpu_count: number;
  memory_total: number;
  memory_available: number;
  load_average: number[];
}

export function getNode(): Promise<NodeInfo> {
  return request<NodeInfo>('/node/get');
}

export function listSecrets(): Promise<Secret[]> {
  return request<Secret[]>('/secrets');
}

export function listConfigs(): Promise<Config[]> {
  return request<Config[]>('/configs');
}

export interface Volume {
  id: string;
  created_at: string;
  /** Absent until the volume is updated. */
  updated_at: string | null;
  namespace: string;
  name: string;
  /** Bytes. `null` when the backend doesn't report a size. */
  size: number | null;
  backend_type: string;
  host_path: string;
  /** Unlike `Config.labels` (a free-form string), the volumes endpoint
   *  returns a real object. */
  labels: Record<string, string>;
}

/** Lists volumes, optionally narrowed server-side to given namespaces. The
 *  endpoint expects a repeated `namespace[]` parameter. */
export function listVolumes(namespaces: string[] = []): Promise<Volume[]> {
  const params = new URLSearchParams();
  for (const ns of namespaces) {
    params.append('namespace[]', ns);
  }
  const qs = params.toString();
  return request<Volume[]>(`/volumes${qs ? `?${qs}` : ''}`);
}

export interface Webhook {
  id: string;
  url: string;
  /** Event names this webhook subscribes to. */
  events: string[];
  created_at: string;
  /** Set once revoked; the webhook then stops receiving deliveries. */
  revoked_at: string | null;
}

/** Webhooks are not namespaced. The HMAC secret is never returned by the API. */
export function listWebhooks(): Promise<Webhook[]> {
  return request<Webhook[]>('/webhooks');
}

export interface User {
  id: string;
  username: string;
  created_at: string;
  updated_at: string | null;
  status: string;
  /** Last successful login; absent for an account that never signed in. */
  login_at: string | null;
}

/** Requires the `users:read` scope. Password hashes are never returned. */
export function listUsers(): Promise<User[]> {
  return request<User[]>('/users');
}

export interface Token {
  id: string;
  name: string;
  /** Leading characters of the clear token (e.g. `ring_pat_a1b2c3`), enough to
   *  identify it. The secret itself is only ever returned at creation. */
  token_prefix: string;
  scopes: string[];
  /** Empty means the token is valid in every namespace. */
  namespaces: string[];
  created_at: string;
  expire_at: string | null;
  last_used_at: string | null;
  revoked_at: string | null;
}

/** Lists the caller's own personal access tokens — never another user's, and
 *  never login sessions. Requires the `admin` scope. */
export function listTokens(): Promise<Token[]> {
  return request<Token[]>('/tokens');
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

// Mutations. Each resource can be created and deleted from its list page;
// the API enforces scopes and namespaces, the dashboard only shows its
// refusals.

export interface SecretInput {
  namespace: string;
  name: string;
  value: string;
}

export function createSecret(input: SecretInput): Promise<Secret> {
  return request<Secret>('/secrets', { method: 'POST', ...jsonBody(input) });
}

/** Refused with 409 while a deployment uses the secret, unless `force`. */
export function deleteSecret(id: string, force = false): Promise<void> {
  const qs = force ? '?force=true' : '';
  return request<void>(`/secrets/${encodeURIComponent(id)}${qs}`, { method: 'DELETE' });
}

export interface ConfigInput {
  namespace: string;
  name: string;
  data: string;
  labels?: string;
}

export function createConfig(input: ConfigInput): Promise<Config> {
  return request<Config>('/configs', { method: 'POST', ...jsonBody(input) });
}

/** Replaces a config's name, data and labels; its namespace cannot change. */
export function updateConfig(id: string, input: Omit<ConfigInput, 'namespace'>): Promise<Config> {
  return request<Config>(`/configs/${encodeURIComponent(id)}`, {
    method: 'PUT',
    ...jsonBody(input)
  });
}

export function deleteConfig(id: string): Promise<void> {
  return request<void>(`/configs/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

export interface VolumeInput {
  namespace: string;
  name: string;
  /** Bytes. */
  size?: number;
  backend_type?: 'local' | 'directory';
  labels?: Record<string, string>;
}

export function createVolume(input: VolumeInput): Promise<Volume> {
  return request<Volume>('/volumes', { method: 'POST', ...jsonBody(input) });
}

/** Refused with 409 while a deployment mounts the volume, unless `force`. */
export function deleteVolume(id: string, force = false): Promise<void> {
  const qs = force ? '?force=true' : '';
  return request<void>(`/volumes/${encodeURIComponent(id)}${qs}`, { method: 'DELETE' });
}

export function createNamespace(name: string): Promise<Namespace> {
  return request<Namespace>('/namespaces', { method: 'POST', ...jsonBody({ name }) });
}

/** Refused with 409 while the namespace still holds resources. */
export function deleteNamespace(name: string): Promise<void> {
  return request<void>(`/namespaces/${encodeURIComponent(name)}`, { method: 'DELETE' });
}

/** Event kinds a webhook can subscribe to, mirroring `KNOWN_EVENT_KINDS`
 *  in `src/events/mod.rs`. `*` and `deployment.*` are accepted too. */
export const WEBHOOK_EVENTS = [
  'deployment.status_changed',
  'deployment.health_check_failed',
  'deployment.rolling_update',
  'deployment.scaled',
  'deployment.error'
] as const;

export interface WebhookInput {
  url: string;
  events: string[];
  /** Generated by the server when omitted. */
  secret?: string;
}

/** The HMAC secret comes back here, once, and is never returned again. */
export interface WebhookCreated extends Webhook {
  secret: string;
}

export function createWebhook(input: WebhookInput): Promise<WebhookCreated> {
  return request<WebhookCreated>('/webhooks', { method: 'POST', ...jsonBody(input) });
}

/** Revokes the webhook: it stays listed, and stops receiving deliveries. */
export function deleteWebhook(id: string): Promise<void> {
  return request<void>(`/webhooks/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

export interface UserInput {
  username: string;
  password: string;
}

export function createUser(input: UserInput): Promise<User> {
  return request<User>('/users', { method: 'POST', ...jsonBody(input) });
}

export const USER_ROLES = ['admin', 'operator', 'viewer'] as const;

export function setUserRole(id: string, role: (typeof USER_ROLES)[number]): Promise<void> {
  return request<void>(`/users/${encodeURIComponent(id)}`, {
    method: 'PUT',
    ...jsonBody({ role })
  });
}

export function deleteUser(id: string): Promise<void> {
  return request<void>(`/users/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

/** Scopes a token may carry, mirroring `KNOWN_SCOPES` in `src/models/token.rs`. */
export const TOKEN_SCOPES = [
  'deployments:read',
  'deployments:write',
  'secrets:read',
  'secrets:write',
  'configs:read',
  'configs:write',
  'namespaces:read',
  'namespaces:write',
  'volumes:read',
  'volumes:write',
  'users:read',
  'users:write',
  'webhooks:read',
  'webhooks:write',
  'admin'
] as const;

export interface TokenInput {
  name: string;
  scopes: string[];
  /** Empty means every namespace. */
  namespaces: string[];
  /** RFC 3339; never expires when omitted. */
  expire_at?: string;
}

/** The clear `ring_pat_…` value comes back here, once. */
export interface TokenCreated {
  id: string;
  name: string;
  token: string;
  message: string;
}

export function createToken(input: TokenInput): Promise<TokenCreated> {
  return request<TokenCreated>('/tokens', { method: 'POST', ...jsonBody(input) });
}

/** Issues a new value for the token and revokes the old one. */
export function rotateToken(id: string): Promise<TokenCreated> {
  return request<TokenCreated>(`/tokens/${encodeURIComponent(id)}/rotate`, { method: 'POST' });
}

export function revokeToken(id: string): Promise<void> {
  return request<void>(`/tokens/${encodeURIComponent(id)}`, { method: 'DELETE' });
}

export interface ExecOptions {
  /** argv of the process to start, e.g. `['/bin/sh']`. */
  command: string[];
  /** Instance (container id or name) to enter; the first one by default. */
  container?: string;
  cols: number;
  rows: number;
}

export interface ExecHandlers {
  onOutput(data: Uint8Array): void;
  /** The process exited; `code` is null when the runtime could not tell. */
  onExit(code: number | null): void;
  /** The server reported a failure inside the session. */
  onError(message: string): void;
  /** The socket closed, after `onExit` or on its own. */
  onClose(): void;
}

export interface ExecSession {
  send(data: string): void;
  resize(cols: number, rows: number): void;
  close(): void;
}

type ExecServerFrame =
  | { type: 'stdout' | 'stderr'; data: string }
  | { type: 'exit'; code: number | null }
  | { type: 'error'; message: string };

const textEncoder = new TextEncoder();

function toBase64(text: string): string {
  let binary = '';
  for (const byte of textEncoder.encode(text)) {
    binary += String.fromCharCode(byte);
  }
  return btoa(binary);
}

function fromBase64(data: string): Uint8Array {
  return Uint8Array.from(atob(data), (c) => c.charCodeAt(0));
}

/** Opens an interactive exec session over a WebSocket. Like the log stream,
 *  it authenticates with a ticket, since a WebSocket cannot carry an
 *  Authorization header. Resolves once the socket is open; rejects when the
 *  server refuses it (exec disabled, missing scope, no running instance). */
export async function openExec(
  id: string,
  options: ExecOptions,
  handlers: ExecHandlers
): Promise<ExecSession> {
  const { ticket } = await mintStreamTicket(`deployment:exec:${id}`);
  const params = new URLSearchParams();
  for (const arg of options.command) {
    params.append('command', arg);
  }
  params.set('tty', 'true');
  params.set('cols', String(options.cols));
  params.set('rows', String(options.rows));
  if (options.container) {
    params.set('container', options.container);
  }
  params.set('ticket', ticket);

  const scheme = window.location.protocol === 'https:' ? 'wss' : 'ws';
  const socket = new WebSocket(
    `${scheme}://${window.location.host}/api/deployments/${encodeURIComponent(id)}/exec?${params}`
  );

  await new Promise<void>((resolve, reject) => {
    socket.onopen = () => resolve();
    // The browser hides the HTTP status of a refused handshake, so the
    // likely causes are spelled out instead.
    socket.onerror = () =>
      reject(
        new Error(
          'The server refused the session: exec may be disabled, your token may lack the admin scope, or no instance is running.'
        )
      );
  });

  socket.onerror = null;
  socket.onmessage = (event) => {
    let frame: ExecServerFrame;
    try {
      frame = JSON.parse(event.data) as ExecServerFrame;
    } catch {
      return;
    }
    switch (frame.type) {
      case 'stdout':
      case 'stderr':
        handlers.onOutput(fromBase64(frame.data));
        break;
      case 'exit':
        handlers.onExit(frame.code);
        break;
      case 'error':
        handlers.onError(frame.message);
        break;
    }
  };
  socket.onclose = () => handlers.onClose();

  const sendFrame = (frame: object) => {
    if (socket.readyState === WebSocket.OPEN) {
      socket.send(JSON.stringify(frame));
    }
  };

  return {
    send(data) {
      sendFrame({ type: 'stdin', data: toBase64(data) });
    },
    resize(cols, rows) {
      sendFrame({ type: 'resize', cols, rows });
    },
    close() {
      socket.close();
    }
  };
}

export const RUNTIMES = [
  'docker',
  'podman',
  'containerd',
  'cloud-hypervisor',
  'firecracker'
] as const;

/** The body of `POST /deployments`. Only the fields the form edits are
 *  typed; the JSON mode can send any other field the API accepts. */
export interface DeploymentInput {
  namespace: string;
  name: string;
  image: string;
  runtime: string;
  kind: 'worker' | 'job';
  replicas: number;
  command?: string[];
  environment?: Record<string, EnvValue>;
  ports?: Array<{ published: number; target: number; protocol: 'tcp' | 'udp' }>;
  [field: string]: unknown;
}

/** Creates a deployment, or rolls out a new version of the one with the
 *  same name in the namespace. */
export function createDeployment(input: DeploymentInput): Promise<Deployment> {
  return request<Deployment>('/deployments', { method: 'POST', ...jsonBody(input) });
}

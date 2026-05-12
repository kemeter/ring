<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import {
    getDeployment,
    listDeploymentEvents,
    type DeploymentDetail,
    type DeploymentEvent,
    type EnvValue,
    type HealthCheck
  } from '$lib/api';
  import { getToken } from '$lib/auth';

  let detail = $state<DeploymentDetail | null>(null);
  let events = $state<DeploymentEvent[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;

  let id = $derived($page.params.id ?? '');

  async function refresh() {
    if (!id) {
      return;
    }
    try {
      // Events can fail (e.g. older versions of the API) without invalidating
      // the rest of the page — degrade gracefully.
      const [d, ev] = await Promise.all([
        getDeployment(id),
        listDeploymentEvents(id).catch(() => [] as DeploymentEvent[])
      ]);
      // The API omits empty collections in some shapes (e.g. health_checks
      // is missing entirely when none are configured). Normalize so the
      // template can safely read `.length`, `Object.keys`, etc.
      detail = {
        ...d,
        command: d.command ?? [],
        ports: d.ports ?? [],
        volumes: d.volumes ?? [],
        instances: d.instances ?? [],
        labels: d.labels ?? {},
        environment: d.environment ?? {},
        health_checks: d.health_checks ?? []
      };
      events = ev;
      errorMsg = null;
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
      lastFetch = new Date();
    }
  }

  onMount(() => {
    if (!getToken()) {
      goto('/');
      return;
    }
    void refresh();
    poll = setInterval(() => void refresh(), 5000);
  });

  onDestroy(() => {
    if (poll) {
      clearInterval(poll);
    }
  });

  function statusKind(s: string): 'success' | 'warn' | 'danger' | 'neutral' {
    const k = s.toLowerCase();
    if (k === 'running') {
      return 'success';
    }
    if (
      k === 'failed' ||
      k === 'crashloopbackoff' ||
      k === 'error' ||
      k === 'createcontainererror' ||
      k === 'imagepullbackoff'
    ) {
      return 'danger';
    }
    if (k === 'pending' || k === 'booting' || k === 'created') {
      return 'warn';
    }
    return 'neutral';
  }

  function envDisplay(value: EnvValue): { kind: 'literal' | 'secret'; text: string } {
    if (typeof value === 'string') {
      return { kind: 'literal', text: value };
    }
    return { kind: 'secret', text: `secretRef: ${value.secretRef}` };
  }

  function hcSummary(hc: HealthCheck): string {
    switch (hc.type) {
      case 'tcp':
        return `port ${hc.port}`;
      case 'http':
        return hc.url;
      case 'command':
        return hc.command;
    }
  }

  function timeAgo(d: Date | null): string {
    if (!d) {
      return '';
    }
    const s = Math.floor((Date.now() - d.getTime()) / 1000);
    if (s < 5) {
      return 'just now';
    }
    if (s < 60) {
      return `${s}s ago`;
    }
    return `${Math.floor(s / 60)}m ago`;
  }

  function formatDate(iso: string | undefined | null): string {
    if (!iso) {
      return '—';
    }
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  }

  /** Collapse runs of consecutive events sharing the same level + message
   *  into a single row with a count. The scheduler emits e.g. "Scaled up"
   *  on every reconciliation tick, which floods the timeline with no
   *  added signal — we keep the first occurrence's timestamp and tally
   *  the rest. */
  interface GroupedEvent {
    key: string;
    first: DeploymentEvent;
    count: number;
  }
  function groupConsecutive(list: DeploymentEvent[]): GroupedEvent[] {
    const out: GroupedEvent[] = [];
    for (const ev of list) {
      const key = `${ev.level ?? ''}|${ev.message ?? ''}|${ev.reason ?? ''}`;
      const last = out[out.length - 1];
      if (last && last.key === key) {
        last.count += 1;
      } else {
        out.push({ key, first: ev, count: 1 });
      }
    }
    return out;
  }

  let groupedEvents = $derived(groupConsecutive(events));
</script>

{#if loading && !detail}
  <p class="muted">Loading…</p>
{:else if errorMsg && !detail}
  <div class="alert"><strong>error</strong> {errorMsg}</div>
  <p><a href="/deployments">← Back to deployments</a></p>
{:else if detail}
  {@const kind = statusKind(detail.status)}
  <nav class="breadcrumbs">
    <a href="/deployments">Deployments</a>
    <span class="sep">/</span>
    <a href="/deployments?namespace={detail.namespace}">{detail.namespace}</a>
    <span class="sep">/</span>
    <span>{detail.name}</span>
  </nav>

  <header class="page-header">
    <div>
      <div class="title-row">
        <h1>{detail.name}</h1>
        <span
          class="status-pill"
          class:success={kind === 'success'}
          class:warn={kind === 'warn'}
          class:danger={kind === 'danger'}
        >
          <span
            class="dot"
            class:success={kind === 'success'}
            class:warn={kind === 'warn'}
            class:danger={kind === 'danger'}
          ></span>
          {detail.status}
        </span>
      </div>
      <p class="subtitle">
        <span class="mono">{detail.id}</span>
      </p>
    </div>
    <div class="header-actions">
      {#if lastFetch}
        <span class="refresh-meta">updated {timeAgo(lastFetch)}</span>
      {/if}
      <button class="btn-secondary" onclick={refresh} disabled={loading}>
        {loading ? 'loading…' : 'Refresh'}
      </button>
    </div>
  </header>

  <section class="grid">
    <div class="card pad">
      <h2>Overview</h2>
      <dl>
        <dt>Runtime</dt>
        <dd>{detail.runtime}</dd>
        <dt>Kind</dt>
        <dd>{detail.kind}</dd>
        <dt>Namespace</dt>
        <dd>{detail.namespace}</dd>
        <dt>Replicas</dt>
        <dd>{detail.replicas}</dd>
        <dt>Restart count</dt>
        <dd>{detail.restart_count}</dd>
        <dt>Image</dt>
        <dd class="mono">{detail.image}</dd>
        {#if detail.image_digest}
          <dt>Digest</dt>
          <dd class="mono small">{detail.image_digest}</dd>
        {/if}
        {#if detail.command.length > 0}
          <dt>Command</dt>
          <dd class="mono">{detail.command.join(' ')}</dd>
        {/if}
        {#if detail.parent_id}
          <dt>Parent</dt>
          <dd>
            <a class="mono" href="/deployments/{detail.parent_id}">{detail.parent_id}</a>
          </dd>
        {/if}
        <dt>Created</dt>
        <dd>{formatDate(detail.created_at)}</dd>
        <dt>Updated</dt>
        <dd>{formatDate(detail.updated_at)}</dd>
      </dl>
    </div>

    <div class="card pad">
      <h2>Resources</h2>
      {#if detail.resources?.limits || detail.resources?.requests}
        <dl>
          {#if detail.resources.limits?.cpu}
            <dt>CPU limit</dt>
            <dd class="mono">{detail.resources.limits.cpu}</dd>
          {/if}
          {#if detail.resources.limits?.memory}
            <dt>Memory limit</dt>
            <dd class="mono">{detail.resources.limits.memory}</dd>
          {/if}
          {#if detail.resources.requests?.cpu}
            <dt>CPU request</dt>
            <dd class="mono">{detail.resources.requests.cpu}</dd>
          {/if}
          {#if detail.resources.requests?.memory}
            <dt>Memory request</dt>
            <dd class="mono">{detail.resources.requests.memory}</dd>
          {/if}
        </dl>
      {:else}
        <p class="muted">No resource limits set.</p>
      {/if}
    </div>
  </section>

  <section class="card">
    <header class="section-head">
      <h2>Instances</h2>
      <span class="count">{detail.instances.length}</span>
    </header>
    {#if detail.instances.length === 0}
      <p class="muted pad">No running instances.</p>
    {:else}
      <ul class="instance-list">
        {#each detail.instances as inst (inst)}
          <li class="mono">{inst}</li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <header class="section-head">
      <h2>Ports</h2>
      <span class="count">{detail.ports.length}</span>
    </header>
    {#if detail.ports.length === 0}
      <p class="muted pad">No ports published.</p>
    {:else}
      <table>
        <thead>
          <tr>
            <th class="num">Published</th>
            <th class="num">Target</th>
            <th>Protocol</th>
          </tr>
        </thead>
        <tbody>
          {#each detail.ports as p}
            <tr>
              <td class="num mono">{p.published}</td>
              <td class="num mono">{p.target}</td>
              <td>{p.protocol ?? 'tcp'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <section class="card">
    <header class="section-head">
      <h2>Volumes</h2>
      <span class="count">{detail.volumes.length}</span>
    </header>
    {#if detail.volumes.length === 0}
      <p class="muted pad">No volumes mounted.</p>
    {:else}
      <table>
        <thead>
          <tr>
            <th>Type</th>
            <th>Source</th>
            <th>Destination</th>
            <th>Mode</th>
          </tr>
        </thead>
        <tbody>
          {#each detail.volumes as v}
            <tr>
              <td>{v.type}</td>
              <td class="mono">{v.source ?? v.key ?? '—'}</td>
              <td class="mono">{v.destination}</td>
              <td>{v.permission}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <section class="card">
    <header class="section-head">
      <h2>Environment</h2>
      <span class="count">{Object.keys(detail.environment).length}</span>
    </header>
    {#if Object.keys(detail.environment).length === 0}
      <p class="muted pad">No environment variables.</p>
    {:else}
      <table>
        <thead>
          <tr>
            <th>Key</th>
            <th>Value</th>
          </tr>
        </thead>
        <tbody>
          {#each Object.entries(detail.environment).sort(([a], [b]) => a.localeCompare(b)) as [k, v] (k)}
            {@const disp = envDisplay(v)}
            <tr>
              <td class="mono">{k}</td>
              <td class="mono">
                {#if disp.kind === 'secret'}
                  <span class="secret-tag">{disp.text}</span>
                {:else}
                  {disp.text}
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  <section class="card">
    <header class="section-head">
      <h2>Health checks</h2>
      <span class="count">{detail.health_checks.length}</span>
    </header>
    {#if detail.health_checks.length === 0}
      <p class="muted pad">No health checks configured.</p>
    {:else}
      <table>
        <thead>
          <tr>
            <th>Type</th>
            <th>Target</th>
            <th>Interval</th>
            <th>Timeout</th>
            <th class="num">Threshold</th>
            <th>On failure</th>
            <th>Readiness</th>
          </tr>
        </thead>
        <tbody>
          {#each detail.health_checks as hc}
            <tr>
              <td>{hc.type}</td>
              <td class="mono">{hcSummary(hc)}</td>
              <td>{hc.interval}</td>
              <td>{hc.timeout}</td>
              <td class="num mono">{hc.threshold}</td>
              <td>{hc.on_failure}</td>
              <td>{hc.readiness ? 'yes' : 'no'}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </section>

  {#if events.length > 0}
    <section class="card">
      <header class="section-head">
        <h2>Recent events</h2>
        <span class="count">{events.length}</span>
      </header>
      <ul class="events">
        {#each groupedEvents as g, i (g.first.id ?? i)}
          {@const ts = g.first.timestamp ?? g.first.created_at}
          <li>
            <span class="event-time mono">{formatDate(ts)}</span>
            {#if g.first.level}
              <span class="event-level event-level-{g.first.level.toLowerCase()}"
                >{g.first.level}</span
              >
            {/if}
            <span class="event-msg">
              {g.first.message ?? JSON.stringify(g.first)}
              {#if g.count > 1}
                <span class="event-multiplier">×{g.count}</span>
              {/if}
              {#if g.first.reason}
                <span class="event-reason">{g.first.reason}</span>
              {/if}
            </span>
          </li>
        {/each}
      </ul>
    </section>
  {/if}
{/if}

<style>
  .breadcrumbs {
    margin-bottom: 1rem;
    color: var(--fg-2);
    font-size: 0.825rem;
  }
  .breadcrumbs a {
    color: var(--fg-1);
  }
  .breadcrumbs a:hover {
    color: var(--accent);
  }
  .breadcrumbs .sep {
    margin: 0 0.4rem;
    color: var(--fg-3);
  }

  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-start;
    margin-bottom: 1.75rem;
  }
  .title-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  h1 {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 600;
    letter-spacing: -0.02em;
  }
  .subtitle {
    margin: 0.35rem 0 0;
    color: var(--fg-3);
    font-size: 0.78rem;
  }
  .header-actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .refresh-meta {
    color: var(--fg-3);
    font-size: 0.75rem;
  }
  .btn-secondary {
    background: var(--bg-2);
    color: var(--fg-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.5rem 0.875rem;
    font-size: 0.8rem;
    font-weight: 500;
  }
  .btn-secondary:hover {
    background: var(--bg-hover);
    color: var(--fg-0);
  }
  .btn-secondary:disabled {
    opacity: 0.5;
    cursor: not-allowed;
  }

  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
    margin-bottom: 1rem;
  }

  .card {
    background: var(--bg-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    margin-bottom: 1rem;
    overflow: hidden;
  }
  .card.pad {
    padding: 1.125rem 1.25rem;
  }
  .section-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.85rem 1.125rem;
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .pad h2 {
    margin-bottom: 0.9rem;
  }
  .count {
    color: var(--fg-3);
    font-size: 0.75rem;
    font-variant-numeric: tabular-nums;
  }
  .muted {
    color: var(--fg-2);
  }
  .muted.pad {
    padding: 1.25rem;
  }

  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    column-gap: 1.5rem;
    row-gap: 0.55rem;
    margin: 0;
  }
  dt {
    color: var(--fg-2);
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  dd {
    margin: 0;
    color: var(--fg-0);
    font-size: 0.85rem;
    word-break: break-word;
  }
  dd.small {
    font-size: 0.78rem;
    color: var(--fg-1);
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: 0.65rem 1rem;
    font-size: 0.82rem;
    border-bottom: 1px solid var(--border);
  }
  tbody tr:last-child td {
    border-bottom: none;
  }
  th {
    font-weight: 500;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-2);
    background: var(--bg-0);
  }
  td.num,
  th.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .mono {
    font-family: var(--font-mono);
  }

  .instance-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .instance-list li {
    padding: 0.55rem 1.125rem;
    border-bottom: 1px solid var(--border);
    font-size: 0.82rem;
    color: var(--fg-1);
  }
  .instance-list li:last-child {
    border-bottom: none;
  }

  .status-pill {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.18rem 0.55rem;
    border-radius: 999px;
    font-size: 0.72rem;
    font-weight: 500;
    color: var(--fg-2);
    background: var(--bg-2);
    border: 1px solid var(--border);
  }
  .status-pill.success {
    color: var(--success);
    background: var(--success-bg);
    border-color: transparent;
  }
  .status-pill.warn {
    color: var(--warning);
    background: var(--warning-bg);
    border-color: transparent;
  }
  .status-pill.danger {
    color: var(--danger);
    background: var(--danger-bg);
    border-color: transparent;
  }
  .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg-3);
  }
  .dot.success {
    background: var(--success);
  }
  .dot.warn {
    background: var(--warning);
  }
  .dot.danger {
    background: var(--danger);
  }

  .secret-tag {
    background: var(--accent-bg);
    color: var(--accent);
    padding: 0.1rem 0.45rem;
    border-radius: var(--radius-sm);
    font-size: 0.75rem;
  }

  .alert {
    background: var(--danger-bg);
    border: 1px solid var(--danger);
    color: var(--fg-0);
    padding: 0.85rem 1rem;
    border-radius: var(--radius);
    margin-bottom: 1.25rem;
    font-size: 0.825rem;
  }
  .alert strong {
    color: var(--danger);
    margin-right: 0.5rem;
  }

  .events {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .events li {
    display: grid;
    grid-template-columns: 11rem auto 1fr;
    gap: 0.75rem;
    align-items: baseline;
    padding: 0.55rem 1.125rem;
    border-bottom: 1px solid var(--border);
    font-size: 0.82rem;
  }
  .events li:last-child {
    border-bottom: none;
  }
  .event-time {
    color: var(--fg-3);
    font-size: 0.75rem;
  }
  .event-level {
    text-transform: uppercase;
    font-size: 0.7rem;
    letter-spacing: 0.05em;
    color: var(--fg-2);
  }
  .event-level-error {
    color: var(--danger);
  }
  .event-level-warning,
  .event-level-warn {
    color: var(--warning);
  }
  .event-level-info {
    color: var(--success);
  }
  .event-msg {
    color: var(--fg-0);
    word-break: break-word;
  }
  .event-multiplier {
    display: inline-block;
    margin-left: 0.5rem;
    padding: 0.05rem 0.4rem;
    border-radius: var(--radius-sm);
    background: var(--bg-2);
    color: var(--fg-2);
    font-size: 0.7rem;
    font-variant-numeric: tabular-nums;
  }
  .event-reason {
    display: inline-block;
    margin-left: 0.5rem;
    padding: 0.05rem 0.4rem;
    border-radius: var(--radius-sm);
    background: var(--accent-bg);
    color: var(--accent);
    font-size: 0.7rem;
  }
</style>

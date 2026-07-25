<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import {
    getDeployment,
    getDeploymentHealthChecks,
    getDeploymentMetrics,
    listDeploymentEvents,
    type DeploymentDetail,
    type DeploymentEvent,
    type DeploymentPort,
    type DeploymentStats,
    type DeploymentVolume,
    type EnvValue,
    type HealthCheck,
    type HealthCheckResult
  } from '$lib/api';
  import CopyButton from '$lib/CopyButton.svelte';
  import DeploymentLogs from '$lib/DeploymentLogs.svelte';
  import DetailTableCard from '$lib/DetailTableCard.svelte';
  import { getToken } from '$lib/auth';
  import { formatBytes, formatDate, timeAgo } from '$lib/utils';

  let detail = $state<DeploymentDetail | null>(null);
  let events = $state<DeploymentEvent[]>([]);
  let metrics = $state<DeploymentStats | null>(null);
  let metricsError = $state<string | null>(null);
  let hcHistory = $state<HealthCheckResult[]>([]);
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
      // Events and metrics can fail (older API, or a deployment with no live
      // instances) without invalidating the rest of the page — degrade
      // gracefully. Metrics carry their own error so the card can explain why
      // it's empty instead of silently showing nothing.
      const [d, ev, m, hc] = await Promise.all([
        getDeployment(id),
        listDeploymentEvents(id).catch(() => [] as DeploymentEvent[]),
        getDeploymentMetrics(id)
          .then((stats) => {
            metricsError = null;
            return stats;
          })
          .catch((e) => {
            metricsError = e instanceof Error ? e.message : String(e);
            return null;
          }),
        getDeploymentHealthChecks(id).catch(() => [] as HealthCheckResult[])
      ]);
      metrics = m;
      hcHistory = hc;
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
      goto('/login');
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

  /** Map a recorded probe status to a colour class. `success` → green,
   *  `failed`/`timeout` → red, anything else neutral. */
  function hcStatusClass(status: string): string {
    const s = status.toLowerCase();
    if (s === 'success') {
      return 'hc-ok';
    }
    if (s === 'failed' || s === 'timeout') {
      return 'hc-fail';
    }
    return 'hc-unknown';
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

  /** Environment as sorted `[key, value]` pairs — the card takes a list, and
   *  sorting here keeps the ordering stable across re-renders. */
  let envEntries = $derived<Array<[string, EnvValue]>>(
    Object.entries(detail?.environment ?? {}).sort(([a], [b]) => a.localeCompare(b))
  );
</script>

<svelte:head>
  <title>{detail ? `Ring · ${detail.name}` : 'Ring · Deployment'}</title>
</svelte:head>

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
        <CopyButton value={detail.id} label="Copy deployment ID" />
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
        {#each detail.instances as inst (inst.id)}
          <li class="mono">
            {inst.id}{#if inst.address}<span class="muted"> ({inst.address})</span>{/if}
          </li>
        {/each}
      </ul>
    {/if}
  </section>

  <section class="card">
    <header class="section-head">
      <h2>Metrics</h2>
      <span class="count">live</span>
    </header>
    {#if metrics && metrics.instance_count > 0}
      <div class="metrics-totals">
        <div class="metric">
          <span class="metric-label">CPU</span>
          <span class="metric-value mono">{metrics.total_cpu_usage_percent.toFixed(1)}%</span>
        </div>
        <div class="metric">
          <span class="metric-label">Memory</span>
          <span class="metric-value mono">
            {formatBytes(metrics.total_memory.usage_bytes)}
            <span class="metric-sub"
              >/ {formatBytes(metrics.total_memory.limit_bytes)} ({metrics.total_memory.usage_percent.toFixed(
                1
              )}%)</span
            >
          </span>
        </div>
        <div class="metric">
          <span class="metric-label">Net I/O</span>
          <span class="metric-value mono">
            {formatBytes(metrics.total_network.rx_bytes)} rx
            <span class="metric-sub">/ {formatBytes(metrics.total_network.tx_bytes)} tx</span>
          </span>
        </div>
        <div class="metric">
          <span class="metric-label">Disk I/O</span>
          <span class="metric-value mono">
            {formatBytes(metrics.total_disk_io.read_bytes)} read
            <span class="metric-sub">/ {formatBytes(metrics.total_disk_io.write_bytes)} write</span>
          </span>
        </div>
        <div class="metric">
          <span class="metric-label">PIDs</span>
          <span class="metric-value mono">{metrics.total_pids}</span>
        </div>
      </div>
      <table>
        <thead>
          <tr>
            <th>Instance</th>
            <th class="num">CPU</th>
            <th class="num">Memory</th>
            <th class="num">Net rx / tx</th>
            <th class="num">Disk r / w</th>
            <th class="num">PIDs</th>
            <th class="num">Restarts</th>
          </tr>
        </thead>
        <tbody>
          {#each metrics.instances as inst (inst.instance_id)}
            <tr>
              <td class="mono" title={inst.instance_id}>{inst.instance_name}</td>
              <td class="num mono">{inst.cpu_usage_percent.toFixed(1)}%</td>
              <td class="num mono">
                {formatBytes(inst.memory.usage_bytes)}
                <span class="metric-sub">({inst.memory.usage_percent.toFixed(1)}%)</span>
              </td>
              <td class="num mono">
                {formatBytes(inst.network.rx_bytes)} / {formatBytes(inst.network.tx_bytes)}
              </td>
              <td class="num mono">
                {formatBytes(inst.disk_io.read_bytes)} / {formatBytes(inst.disk_io.write_bytes)}
              </td>
              <td class="num mono">{inst.pids.current} / {inst.pids.limit}</td>
              <td class="num mono">{inst.restart_count}</td>
            </tr>
          {/each}
        </tbody>
      </table>
    {:else if metricsError}
      <p class="muted pad">Metrics unavailable: {metricsError}</p>
    {:else}
      <p class="muted pad">No live instances to report metrics for.</p>
    {/if}
  </section>

  <DetailTableCard
    title="Ports"
    columns={[{ label: 'Published', align: 'right' }, { label: 'Target', align: 'right' }, { label: 'Protocol' }]}
    items={detail.ports}
    emptyText="No ports published."
  >
    {#snippet row(p: DeploymentPort)}
      <tr>
        <td class="num mono">{p.published}</td>
        <td class="num mono">{p.target}</td>
        <td>{p.protocol ?? 'tcp'}</td>
      </tr>
    {/snippet}
  </DetailTableCard>

  <DetailTableCard
    title="Volumes"
    columns={[{ label: 'Type' }, { label: 'Source' }, { label: 'Destination' }, { label: 'Mode' }]}
    items={detail.volumes}
    emptyText="No volumes mounted."
  >
    {#snippet row(v: DeploymentVolume)}
      <tr>
        <td>{v.type}</td>
        <td class="mono">{v.source ?? v.key ?? '\u2014'}</td>
        <td class="mono">{v.destination}</td>
        <td>{v.permission}</td>
      </tr>
    {/snippet}
  </DetailTableCard>

  <DetailTableCard
    title="Environment"
    columns={[{ label: 'Key' }, { label: 'Value' }]}
    items={envEntries}
    emptyText="No environment variables."
    key={([k]) => k}
  >
    {#snippet row([k, v]: [string, EnvValue])}
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
    {/snippet}
  </DetailTableCard>

  <DetailTableCard
    title="Health checks"
    columns={[
      { label: 'Type' },
      { label: 'Target' },
      { label: 'Interval' },
      { label: 'Timeout' },
      { label: 'Threshold', align: 'right' },
      { label: 'On failure' },
      { label: 'Readiness' }
    ]}
    items={detail.health_checks}
    emptyText="No health checks configured."
  >
    {#snippet row(hc: HealthCheck)}
      <tr>
        <td>{hc.type}</td>
        <td class="mono">{hcSummary(hc)}</td>
        <td>{hc.interval}</td>
        <td>{hc.timeout}</td>
        <td class="num mono">{hc.threshold}</td>
        <td>{hc.on_failure}</td>
        <td>{hc.readiness ? 'yes' : 'no'}</td>
      </tr>
    {/snippet}
  </DetailTableCard>

  {#if detail.health_checks.length > 0}
    <section class="card">
      <header class="section-head">
        <h2>Health check history</h2>
        <span class="count">{hcHistory.length}</span>
      </header>
      {#if hcHistory.length === 0}
        <p class="muted pad">No probe results recorded yet.</p>
      {:else}
        <table>
          <thead>
            <tr>
              <th>Time</th>
              <th>Type</th>
              <th>Status</th>
              <th>Message</th>
            </tr>
          </thead>
          <tbody>
            {#each hcHistory as r (r.id)}
              <tr>
                <td class="mono">{formatDate(r.finished_at)}</td>
                <td>{r.check_type}</td>
                <td>
                  <span class="hc-status {hcStatusClass(r.status)}">{r.status}</span>
                </td>
                <td class="mono small">{r.message ?? '—'}</td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    </section>
  {/if}

  <DeploymentLogs {id} />

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

  /* Override the shared .page-header on this page: the detail layout uses
   * top-aligned columns because the title block stacks pill + id below the
   * h1, and we want the action buttons to sit next to the title (not the
   * bottom of the meta column). */
  .page-header {
    align-items: flex-start;
  }
  .title-row {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .page-header .subtitle {
    margin-top: 0.35rem;
    color: var(--fg-3);
    font-size: 0.78rem;
  }

  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
    margin-bottom: 1rem;
  }

  /* Detail page stacks multiple cards vertically — the shared .card has no
   * vertical rhythm baked in. */
  .card {
    margin-bottom: 1rem;
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
  .muted.pad {
    padding: 1.25rem;
  }

  .metrics-totals {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(11rem, 1fr));
    gap: 0.85rem 1.5rem;
    padding: 1rem 1.125rem;
    border-bottom: 1px solid var(--border);
  }
  .metric {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
  }
  .metric-label {
    color: var(--fg-2);
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .metric-value {
    color: var(--fg-0);
    font-size: 0.95rem;
    font-variant-numeric: tabular-nums;
  }
  .metric-sub {
    color: var(--fg-3);
    font-size: 0.72rem;
    font-weight: 400;
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

  .hc-status {
    display: inline-block;
    padding: 0.1rem 0.45rem;
    border-radius: var(--radius-sm);
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .hc-ok {
    color: var(--success);
    background: var(--success-bg);
  }
  .hc-fail {
    color: var(--danger);
    background: var(--danger-bg);
  }
  .hc-unknown {
    color: var(--fg-3);
    background: var(--bg-2);
  }
  td.small {
    font-size: 0.78rem;
    color: var(--fg-1);
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

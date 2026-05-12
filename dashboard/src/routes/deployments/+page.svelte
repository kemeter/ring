<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { listDeployments, type Deployment } from '$lib/api';
  import { getToken } from '$lib/auth';

  let items = $state<Deployment[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      items = await listDeployments();
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
      goto('../');
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

  function statusKind(s: string): 'success' | 'warn' | 'danger' | 'neutral' {
    const k = s.toLowerCase();
    if (k === 'running') {
      return 'success';
    }
    if (k === 'failed' || k === 'crashloopbackoff' || k === 'error') {
      return 'danger';
    }
    if (k === 'pending' || k === 'booting' || k === 'created') {
      return 'warn';
    }
    return 'neutral';
  }

  let runningCount = $derived(items.filter((d) => d.status.toLowerCase() === 'running').length);
  let totalReplicas = $derived(items.reduce((acc, d) => acc + (d.replicas ?? 0), 0));
</script>

<header class="page-header">
  <div>
    <h1>Deployments</h1>
    <p class="subtitle">All workloads scheduled by this Ring instance</p>
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

<section class="stats">
  <div class="stat-card">
    <div class="stat-label">Running</div>
    <div class="stat-value">{runningCount}<span class="unit">/ {items.length}</span></div>
    <div class="stat-sub">deployments currently healthy</div>
  </div>
  <div class="stat-card">
    <div class="stat-label">Replicas</div>
    <div class="stat-value">{totalReplicas}</div>
    <div class="stat-sub">across every deployment</div>
  </div>
</section>

{#if errorMsg}
  <div class="alert">
    <strong>error</strong> {errorMsg}
  </div>
{/if}

{#if !loading && items.length > 0}
  <section class="card">
    <table>
      <thead>
        <tr>
          <th>Namespace</th>
          <th>Name</th>
          <th>Runtime</th>
          <th>Status</th>
          <th class="num">Replicas</th>
          <th>Image</th>
        </tr>
      </thead>
      <tbody>
        {#each items as d (d.id)}
          {@const kind = statusKind(d.status)}
          <tr>
            <td>{d.namespace}</td>
            <td><span class="deployment-name">{d.name}</span></td>
            <td>{d.runtime}</td>
            <td>
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
                {d.status}
              </span>
            </td>
            <td class="num mono">{d.replicas}</td>
            <td class="mono">{d.image}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>
{/if}

{#if !loading && items.length === 0 && !errorMsg}
  <div class="empty">
    <p>No deployments yet.</p>
    <p class="muted">Use <code>ring apply -f deployment.yaml</code> to create one.</p>
  </div>
{/if}

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: flex-end;
    margin-bottom: 1.75rem;
  }
  h1 {
    margin: 0;
    font-size: 1.5rem;
    font-weight: 600;
    letter-spacing: -0.02em;
  }
  .subtitle {
    margin: 0.25rem 0 0;
    color: var(--fg-2);
    font-size: 0.825rem;
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

  .stats {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 1rem;
    margin-bottom: 1.5rem;
  }
  .stat-card {
    background: var(--bg-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 1rem 1.125rem;
  }
  .stat-label {
    font-size: 0.7rem;
    text-transform: uppercase;
    letter-spacing: 0.08em;
    color: var(--fg-2);
    font-weight: 500;
  }
  .stat-value {
    font-size: 1.5rem;
    font-weight: 600;
    margin-top: 0.35rem;
    letter-spacing: -0.02em;
    font-variant-numeric: tabular-nums;
  }
  .unit {
    font-size: 0.85rem;
    color: var(--fg-2);
    font-weight: 500;
    margin-left: 4px;
  }
  .stat-sub {
    font-size: 0.75rem;
    color: var(--fg-3);
    margin-top: 0.25rem;
  }

  .card {
    background: var(--bg-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 0;
    overflow: hidden;
  }

  table {
    width: 100%;
    border-collapse: collapse;
  }
  th,
  td {
    text-align: left;
    padding: 0.7rem 1rem;
    font-size: 0.85rem;
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
  td.mono {
    font-family: var(--font-mono);
    color: var(--fg-1);
  }
  .deployment-name {
    font-weight: 500;
    color: var(--fg-0);
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

  .empty {
    background: var(--bg-1);
    border: 1px dashed var(--border);
    border-radius: var(--radius-lg);
    padding: 3rem 1rem;
    text-align: center;
  }
  .empty p {
    margin: 0.25rem 0;
  }
  .empty .muted {
    color: var(--fg-2);
    font-size: 0.85rem;
  }
  code {
    background: var(--bg-2);
    padding: 0.1rem 0.4rem;
    border-radius: var(--radius-sm);
    font-family: var(--font-mono);
  }
</style>

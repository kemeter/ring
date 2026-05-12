<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { listDeployments, listNamespaces, type Deployment, type Namespace } from '$lib/api';
  import { getToken } from '$lib/auth';

  let namespaces = $state<Namespace[]>([]);
  let deployments = $state<Deployment[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      // Fetch both in parallel so the deployment counts are consistent with
      // the namespace list. Order matters less than the joint snapshot.
      const [ns, dep] = await Promise.all([listNamespaces(), listDeployments()]);
      namespaces = ns;
      deployments = dep;
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

  function deploymentCount(name: string): number {
    return deployments.filter((d) => d.namespace === name).length;
  }

  function runningCount(name: string): number {
    return deployments.filter(
      (d) => d.namespace === name && d.status.toLowerCase() === 'running'
    ).length;
  }

  function formatDate(iso: string): string {
    try {
      return new Date(iso).toLocaleString();
    } catch {
      return iso;
    }
  }

  let totalDeployments = $derived(deployments.length);
  let usedNamespaces = $derived(
    new Set(deployments.map((d) => d.namespace)).size
  );
</script>

<header class="page-header">
  <div>
    <h1>Namespaces</h1>
    <p class="subtitle">Logical groupings for deployments</p>
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
    <div class="stat-label">Namespaces</div>
    <div class="stat-value">{namespaces.length}</div>
    <div class="stat-sub">{usedNamespaces} currently hosting workloads</div>
  </div>
  <div class="stat-card">
    <div class="stat-label">Deployments</div>
    <div class="stat-value">{totalDeployments}</div>
    <div class="stat-sub">across every namespace</div>
  </div>
</section>

{#if errorMsg}
  <div class="alert">
    <strong>error</strong> {errorMsg}
  </div>
{/if}

{#if !loading && namespaces.length > 0}
  <section class="card">
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th class="num">Deployments</th>
          <th class="num">Running</th>
          <th>Created</th>
          <th>Updated</th>
        </tr>
      </thead>
      <tbody>
        {#each namespaces as ns (ns.id)}
          {@const total = deploymentCount(ns.name)}
          {@const running = runningCount(ns.name)}
          <tr>
            <td>
              <a class="ns-link" href="/deployments?namespace={ns.name}">{ns.name}</a>
            </td>
            <td class="num mono">{total}</td>
            <td class="num mono">
              {#if total === 0}
                <span class="muted">0</span>
              {:else if running === total}
                <span class="dot-inline success"></span>{running}
              {:else}
                <span class="dot-inline warn"></span>{running}
              {/if}
            </td>
            <td class="muted">{formatDate(ns.created_at)}</td>
            <td class="muted">{ns.updated_at ? formatDate(ns.updated_at) : '—'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>
{/if}

{#if !loading && namespaces.length === 0 && !errorMsg}
  <div class="empty">
    <p>No namespaces yet.</p>
    <p class="muted">
      Namespaces are created automatically when you <code>ring apply</code> a deployment with a new
      namespace, or explicitly with <code>ring namespace create &lt;name&gt;</code>.
    </p>
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
  }
  .muted {
    color: var(--fg-2);
  }
  .ns-link {
    color: var(--fg-0);
    font-weight: 500;
  }
  .ns-link:hover {
    color: var(--accent);
  }
  .dot-inline {
    display: inline-block;
    width: 6px;
    height: 6px;
    border-radius: 50%;
    margin-right: 0.4rem;
    vertical-align: middle;
  }
  .dot-inline.success {
    background: var(--success);
  }
  .dot-inline.warn {
    background: var(--warning);
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
    margin: 0.35rem 0;
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

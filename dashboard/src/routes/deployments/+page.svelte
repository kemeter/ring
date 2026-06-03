<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { listDeployments, type Deployment } from '$lib/api';
  import { getToken } from '$lib/auth';
  import { formatDate, timeAgo } from '$lib/utils';

  let items = $state<Deployment[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;

  // Filters are mirrored to the URL so a filtered view is shareable and
  // a back-button restores the state. Empty string means "no filter".
  let namespaceFilter = $state('');
  let runtimeFilter = $state('');
  let statusFilter = $state('');
  let searchQuery = $state('');

  // One-shot read of the URL on mount; subsequent changes are pushed back
  // by `syncUrl()` below.
  function loadFiltersFromUrl(url: URL): void {
    namespaceFilter = url.searchParams.get('namespace') ?? '';
    runtimeFilter = url.searchParams.get('runtime') ?? '';
    statusFilter = url.searchParams.get('status') ?? '';
    searchQuery = url.searchParams.get('q') ?? '';
  }

  function syncUrl(): void {
    const params = new URLSearchParams();
    if (namespaceFilter) {
      params.set('namespace', namespaceFilter);
    }
    if (runtimeFilter) {
      params.set('runtime', runtimeFilter);
    }
    if (statusFilter) {
      params.set('status', statusFilter);
    }
    if (searchQuery) {
      params.set('q', searchQuery);
    }
    const qs = params.toString();
    const next = qs ? `?${qs}` : '';
    // Replace the URL silently — no navigation, no re-mount.
    history.replaceState(null, '', `${$page.url.pathname}${next}`);
  }

  function clearFilters() {
    namespaceFilter = '';
    runtimeFilter = '';
    statusFilter = '';
    searchQuery = '';
    syncUrl();
  }

  // Distinct values, derived from the current dataset, used to populate
  // the dropdowns. Sorted alphabetically so the order is stable.
  let allNamespaces = $derived(
    Array.from(new Set(items.map((d) => d.namespace))).sort()
  );
  let allRuntimes = $derived(Array.from(new Set(items.map((d) => d.runtime))).sort());
  let allStatuses = $derived(Array.from(new Set(items.map((d) => d.status))).sort());

  let visible = $derived(
    items.filter((d) => {
      if (namespaceFilter && d.namespace !== namespaceFilter) {
        return false;
      }
      if (runtimeFilter && d.runtime !== runtimeFilter) {
        return false;
      }
      if (statusFilter && d.status !== statusFilter) {
        return false;
      }
      if (searchQuery) {
        const q = searchQuery.toLowerCase();
        return d.name.toLowerCase().includes(q) || d.image.toLowerCase().includes(q);
      }
      return true;
    })
  );

  let hasActiveFilter = $derived(
    Boolean(namespaceFilter || runtimeFilter || statusFilter || searchQuery)
  );

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
      goto('/login');
      return;
    }
    loadFiltersFromUrl($page.url);
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

  let runningCount = $derived(visible.filter((d) => d.status.toLowerCase() === 'running').length);
  let totalReplicas = $derived(visible.reduce((acc, d) => acc + (d.replicas ?? 0), 0));
</script>

<svelte:head><title>Ring · Deployments</title></svelte:head>

<header class="page-header">
  <div>
    <h1>Deployments</h1>
    <p class="subtitle">
      {#if hasActiveFilter}
        Showing {visible.length} of {items.length}
        — <a
          href="/deployments"
          onclick={(e) => {
            e.preventDefault();
            clearFilters();
          }}>clear filters</a
        >
      {:else}
        All workloads scheduled by this Ring instance
      {/if}
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

<section class="stats">
  <div class="stat-card">
    <div class="stat-label">Running</div>
    <div class="stat-value">{runningCount}<span class="unit">/ {visible.length}</span></div>
    <div class="stat-sub">deployments currently healthy</div>
  </div>
  <div class="stat-card">
    <div class="stat-label">Replicas</div>
    <div class="stat-value">{totalReplicas}</div>
    <div class="stat-sub">across every deployment</div>
  </div>
</section>

<section class="filters">
  <div class="filter search">
    <input
      type="search"
      placeholder="Search name or image…"
      bind:value={searchQuery}
      oninput={syncUrl}
    />
  </div>
  <div class="filter">
    <select bind:value={namespaceFilter} onchange={syncUrl}>
      <option value="">All namespaces</option>
      {#each allNamespaces as ns}
        <option value={ns}>{ns}</option>
      {/each}
    </select>
  </div>
  <div class="filter">
    <select bind:value={runtimeFilter} onchange={syncUrl}>
      <option value="">All runtimes</option>
      {#each allRuntimes as rt}
        <option value={rt}>{rt}</option>
      {/each}
    </select>
  </div>
  <div class="filter">
    <select bind:value={statusFilter} onchange={syncUrl}>
      <option value="">All statuses</option>
      {#each allStatuses as st}
        <option value={st}>{st}</option>
      {/each}
    </select>
  </div>
  {#if hasActiveFilter}
    <button class="btn-clear" onclick={clearFilters}>Clear</button>
  {/if}
</section>

{#if errorMsg}
  <div class="alert">
    <strong>error</strong> {errorMsg}
  </div>
{/if}

{#if !loading && visible.length > 0}
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
          <th>Created</th>
        </tr>
      </thead>
      <tbody>
        {#each visible as d (d.id)}
          {@const kind = statusKind(d.status)}
          <tr>
            <td>{d.namespace}</td>
            <td><a class="deployment-name" href="/deployments/{d.id}">{d.name}</a></td>
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
            <td class="created">{formatDate(d.created_at)}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>
{/if}

{#if !loading && visible.length === 0 && !errorMsg}
  <div class="empty">
    {#if hasActiveFilter}
      <p>No deployments match the current filters.</p>
      <p class="muted">
        <a
          href="/deployments"
          onclick={(e) => {
            e.preventDefault();
            clearFilters();
          }}>Clear filters</a
        >
        or adjust them.
      </p>
    {:else}
      <p>No deployments yet.</p>
      <p class="muted">Use <code>ring apply -f deployment.yaml</code> to create one.</p>
    {/if}
  </div>
{/if}

<style>
  .filters {
    display: flex;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-bottom: 1rem;
    align-items: center;
  }
  .filter {
    min-width: 160px;
  }
  .filter.search {
    flex: 1 1 220px;
    min-width: 220px;
  }
  .filter input,
  .filter select {
    width: 100%;
    background: var(--bg-1);
    color: var(--fg-0);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.45rem 0.7rem;
    font-size: 0.82rem;
    outline: none;
    transition: border-color 0.1s;
  }
  .filter input:focus,
  .filter select:focus {
    border-color: var(--accent);
  }
  .filter input::placeholder {
    color: var(--fg-3);
  }
  .filter select {
    appearance: none;
    -webkit-appearance: none;
    background-image: linear-gradient(45deg, transparent 50%, var(--fg-2) 50%),
      linear-gradient(135deg, var(--fg-2) 50%, transparent 50%);
    background-position:
      calc(100% - 14px) center,
      calc(100% - 9px) center;
    background-size:
      5px 5px,
      5px 5px;
    background-repeat: no-repeat;
    padding-right: 1.75rem;
  }
  .btn-clear {
    background: var(--bg-2);
    color: var(--fg-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.45rem 0.8rem;
    font-size: 0.78rem;
  }
  .btn-clear:hover {
    background: var(--bg-hover);
    color: var(--fg-0);
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

  td.num,
  th.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  td.mono {
    font-family: var(--font-mono);
    color: var(--fg-1);
  }
  .created {
    color: var(--fg-2);
    font-size: 0.8rem;
    white-space: nowrap;
  }
  .deployment-name {
    font-weight: 500;
    color: var(--fg-0);
  }
  .deployment-name:hover {
    color: var(--accent);
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

</style>

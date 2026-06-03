<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { getNode, type NodeInfo } from '$lib/api';
  import { getToken } from '$lib/auth';
  import { timeAgo } from '$lib/utils';

  let node = $state<NodeInfo | null>(null);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      node = await getNode();
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

  /** Memory percent used, guarding against a zero total. */
  let memUsedPercent = $derived(
    node && node.memory_total > 0
      ? ((node.memory_total - node.memory_available) / node.memory_total) * 100
      : 0
  );
</script>

<svelte:head><title>Ring · Node</title></svelte:head>

<header class="page-header">
  <h1>Node</h1>
  <div class="header-actions">
    {#if lastFetch}
      <span class="refresh-meta">updated {timeAgo(lastFetch)}</span>
    {/if}
    <button class="btn-secondary" onclick={refresh} disabled={loading}>
      {loading ? 'loading…' : 'Refresh'}
    </button>
  </div>
</header>

{#if loading && !node}
  <p class="muted">Loading…</p>
{:else if errorMsg && !node}
  <div class="alert"><strong>error</strong> {errorMsg}</div>
{:else if node}
  <section class="grid">
    <div class="card pad">
      <h2>Host</h2>
      <dl>
        <dt>Hostname</dt>
        <dd class="mono">{node.hostname}</dd>
        <dt>OS</dt>
        <dd>{node.os}</dd>
        <dt>Architecture</dt>
        <dd class="mono">{node.arch}</dd>
        <dt>Uptime</dt>
        <dd>{node.uptime}</dd>
      </dl>
    </div>

    <div class="card pad">
      <h2>Resources</h2>
      <dl>
        <dt>CPU cores</dt>
        <dd class="mono">{node.cpu_count}</dd>
        <dt>Memory</dt>
        <dd class="mono">
          {(node.memory_total - node.memory_available).toFixed(2)} / {node.memory_total.toFixed(2)}
          GiB
          <span class="metric-sub">({memUsedPercent.toFixed(1)}% used)</span>
        </dd>
        <dt>Memory free</dt>
        <dd class="mono">{node.memory_available.toFixed(2)} GiB</dd>
        <dt>Load average</dt>
        <dd class="mono">
          {node.load_average.map((l) => l.toFixed(2)).join(', ')}
          <span class="metric-sub">(1m, 5m, 15m)</span>
        </dd>
      </dl>
    </div>
  </section>
{/if}

<style>
  .page-header {
    display: flex;
    justify-content: space-between;
    align-items: center;
    margin-bottom: 1.25rem;
  }
  h1 {
    margin: 0;
    font-size: 1.4rem;
    letter-spacing: -0.02em;
  }
  .header-actions {
    display: flex;
    align-items: center;
    gap: 0.75rem;
  }
  .refresh-meta {
    color: var(--fg-3);
    font-size: 0.78rem;
  }

  .grid {
    display: grid;
    grid-template-columns: 1fr 1fr;
    gap: 1rem;
  }
  .card.pad {
    padding: 1.125rem 1.25rem;
  }
  h2 {
    margin: 0 0 0.9rem;
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: -0.01em;
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
  .mono {
    font-family: var(--font-mono);
  }
  .metric-sub {
    color: var(--fg-3);
    font-size: 0.72rem;
  }
</style>

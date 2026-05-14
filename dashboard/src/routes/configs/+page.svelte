<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { listConfigs, type Config } from '$lib/api';
  import { getToken } from '$lib/auth';
  import { formatDate, timeAgo } from '$lib/utils';

  let configs = $state<Config[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;
  let nsFilter = $state<string>('');
  let expanded = $state<Set<string>>(new Set());

  async function refresh() {
    try {
      configs = await listConfigs();
      errorMsg = null;
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
    } finally {
      loading = false;
      lastFetch = new Date();
    }
  }

  function syncUrl(): void {
    const params = new URLSearchParams();
    if (nsFilter) {
      params.set('namespace', nsFilter);
    }
    const qs = params.toString();
    history.replaceState(null, '', `${$page.url.pathname}${qs ? `?${qs}` : ''}`);
  }

  onMount(() => {
    if (!getToken()) {
      goto('/');
      return;
    }
    nsFilter = $page.url.searchParams.get('namespace') ?? '';
    void refresh();
    poll = setInterval(() => void refresh(), 5000);
  });

  onDestroy(() => {
    if (poll) {
      clearInterval(poll);
    }
  });

  function toggle(id: string) {
    // Clone before mutating: Svelte 5 reactivity tracks the reference, not the
    // contents of the Set, so an in-place add()/delete() wouldn't re-render.
    const next = new Set(expanded);
    if (next.has(id)) {
      next.delete(id);
    } else {
      next.add(id);
    }
    expanded = next;
  }

  function byteSize(s: string): string {
    const bytes = new TextEncoder().encode(s).length;
    if (bytes < 1024) {
      return `${bytes} B`;
    }
    if (bytes < 1024 * 1024) {
      return `${(bytes / 1024).toFixed(1)} KiB`;
    }
    return `${(bytes / 1024 / 1024).toFixed(1)} MiB`;
  }

  let namespaces = $derived(
    Array.from(new Set(configs.map((c) => c.namespace))).sort((a, b) => a.localeCompare(b))
  );
  let filtered = $derived(nsFilter ? configs.filter((c) => c.namespace === nsFilter) : configs);
</script>

<header class="page-header">
  <div>
    <h1>Configs</h1>
    <p class="subtitle">Free-form configuration blobs mounted into deployments</p>
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

{#if errorMsg}
  <div class="alert">
    <strong>error</strong> {errorMsg}
  </div>
{/if}

{#if !loading && configs.length > 0}
  {#if namespaces.length > 1}
    <div class="filter-bar">
      <label for="ns-filter">Namespace</label>
      <select id="ns-filter" bind:value={nsFilter} onchange={syncUrl}>
        <option value="">All ({configs.length})</option>
        {#each namespaces as ns}
          <option value={ns}>{ns} ({configs.filter((c) => c.namespace === ns).length})</option>
        {/each}
      </select>
    </div>
  {/if}

  <section class="card">
    <table>
      <thead>
        <tr>
          <th></th>
          <th>Name</th>
          <th>Namespace</th>
          <th class="num">Size</th>
          <th>Labels</th>
          <th>Created</th>
        </tr>
      </thead>
      <tbody>
        {#each filtered as c (c.id)}
          {@const open = expanded.has(c.id)}
          <tr>
            <td class="caret-cell">
              <button
                type="button"
                class="caret"
                aria-expanded={open}
                aria-controls={`config-${c.id}-data`}
                aria-label={open ? 'Collapse config data' : 'Expand config data'}
                onclick={() => toggle(c.id)}
              >
                {open ? '▾' : '▸'}
              </button>
            </td>
            <td class="mono">{c.name}</td>
            <td>
              <a class="ns-link" href="/configs?namespace={c.namespace}">{c.namespace}</a>
            </td>
            <td class="num mono">{byteSize(c.data)}</td>
            <td class="mono small">{c.labels || '—'}</td>
            <td class="muted">{formatDate(c.created_at)}</td>
          </tr>
          {#if open}
            <tr class="data-row" id={`config-${c.id}-data`}>
              <td></td>
              <td colspan="5">
                <pre class="data">{c.data}</pre>
              </td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>
  </section>
{/if}

{#if !loading && configs.length === 0 && !errorMsg}
  <div class="empty">
    <p>No configs yet.</p>
    <p class="muted">
      Configs are created over the API (<code>POST /configs</code>) and mounted into deployments via
      the <code>config:</code> field in your manifest.
    </p>
  </div>
{/if}

<style>
  td.num,
  th.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  td.mono {
    font-family: var(--font-mono);
  }
  td.small {
    font-size: 0.78rem;
  }
  .ns-link {
    color: var(--fg-0);
    font-weight: 500;
  }
  .ns-link:hover {
    color: var(--accent);
  }

  .caret-cell {
    width: 2rem;
    padding-right: 0;
  }
  .caret {
    background: transparent;
    border: none;
    color: var(--fg-3);
    font-size: 0.75rem;
    padding: 0.15rem 0.35rem;
    border-radius: var(--radius-sm);
    cursor: pointer;
  }
  .caret:hover {
    background: var(--bg-hover);
    color: var(--fg-1);
  }
  .caret:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .data-row td {
    background: var(--bg-0);
    padding: 0;
  }
  .data {
    margin: 0;
    padding: 0.85rem 1rem;
    font-family: var(--font-mono);
    font-size: 0.78rem;
    color: var(--fg-1);
    white-space: pre-wrap;
    word-break: break-all;
    max-height: 24rem;
    overflow-y: auto;
  }
</style>

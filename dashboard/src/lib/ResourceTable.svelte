<script lang="ts" generics="T">
  /**
   * The list-page shell shared by every resource listing: header with
   * refresh-meta, optional namespace filter, error / empty / table states,
   * plus the load-and-poll lifecycle.
   *
   * Pages own their columns and cells (via the `row` snippet) and nothing
   * else — everything that was duplicated verbatim across the secrets and
   * configs pages lives here instead.
   */
  import { onDestroy, onMount } from 'svelte';
  import type { Snippet } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { getToken } from '$lib/auth';
  import { timeAgo } from '$lib/utils';

  interface Props {
    title: string;
    subtitle: string;
    /** Column headers, in order. `align: 'right'` gets tabular numerals. */
    columns: Array<{ label: string; align?: 'right' }>;
    /** Fetches the full list; re-run on refresh and on every poll tick. */
    load: () => Promise<T[]>;
    /** Stable key per item, for keyed `{#each}`. */
    key: (item: T) => string;
    /** Reads the namespace of an item. Omit for resources without one — the
     *  filter bar is then never rendered. */
    namespaceOf?: (item: T) => string;
    /** Poll interval in ms; `0` disables polling. */
    pollMs?: number;
    /** One `<tr>` per item. */
    row: Snippet<[T]>;
    /** Shown when the list comes back empty. */
    empty: Snippet;
  }

  let {
    title,
    subtitle,
    columns,
    load,
    key,
    namespaceOf,
    pollMs = 5000,
    row,
    empty
  }: Props = $props();

  let items = $state<T[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let nsFilter = $state<string>('');
  let poll: ReturnType<typeof setInterval> | null = null;

  export async function refresh(): Promise<void> {
    try {
      items = await load();
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
      goto('/login');
      return;
    }
    nsFilter = $page.url.searchParams.get('namespace') ?? '';
    void refresh();
    if (pollMs > 0) {
      poll = setInterval(() => void refresh(), pollMs);
    }
  });

  onDestroy(() => {
    if (poll) {
      clearInterval(poll);
    }
  });

  function countIn(ns: string): number {
    return namespaceOf ? items.filter((i) => namespaceOf(i) === ns).length : 0;
  }

  let namespaces = $derived(
    namespaceOf
      ? Array.from(new Set(items.map(namespaceOf))).sort((a, b) => a.localeCompare(b))
      : []
  );
  let filtered = $derived(
    namespaceOf && nsFilter ? items.filter((i) => namespaceOf(i) === nsFilter) : items
  );
</script>

<svelte:head><title>Ring · {title}</title></svelte:head>

<header class="page-header">
  <div>
    <h1>{title}</h1>
    <p class="subtitle">{subtitle}</p>
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

{#if !loading && items.length > 0}
  {#if namespaces.length > 1}
    <div class="filter-bar">
      <label for="ns-filter">Namespace</label>
      <select id="ns-filter" bind:value={nsFilter} onchange={syncUrl}>
        <option value="">All ({items.length})</option>
        {#each namespaces as ns}
          <option value={ns}>{ns} ({countIn(ns)})</option>
        {/each}
      </select>
    </div>
  {/if}

  <section class="card">
    <table>
      <thead>
        <tr>
          {#each columns as col}
            <th class:num={col.align === 'right'}>{col.label}</th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#each filtered as item (key(item))}
          {@render row(item)}
        {/each}
      </tbody>
    </table>
  </section>
{/if}

{#if !loading && items.length === 0 && !errorMsg}
  <div class="empty">
    {@render empty()}
  </div>
{/if}

<style>
  th.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
</style>

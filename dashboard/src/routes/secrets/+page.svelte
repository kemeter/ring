<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { listSecrets, type Secret } from '$lib/api';
  import { getToken } from '$lib/auth';
  import { formatDate, timeAgo } from '$lib/utils';

  let secrets = $state<Secret[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;
  let nsFilter = $state<string>('');

  async function refresh() {
    try {
      secrets = await listSecrets();
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
    poll = setInterval(() => void refresh(), 5000);
  });

  onDestroy(() => {
    if (poll) {
      clearInterval(poll);
    }
  });

  let namespaces = $derived(
    Array.from(new Set(secrets.map((s) => s.namespace))).sort((a, b) => a.localeCompare(b))
  );
  let filtered = $derived(nsFilter ? secrets.filter((s) => s.namespace === nsFilter) : secrets);
</script>

<header class="page-header">
  <div>
    <h1>Secrets</h1>
    <p class="subtitle">AES-256-GCM encrypted values, scoped per namespace</p>
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

{#if !loading && secrets.length > 0}
  {#if namespaces.length > 1}
    <div class="filter-bar">
      <label for="ns-filter">Namespace</label>
      <select id="ns-filter" bind:value={nsFilter} onchange={syncUrl}>
        <option value="">All ({secrets.length})</option>
        {#each namespaces as ns}
          <option value={ns}>{ns} ({secrets.filter((s) => s.namespace === ns).length})</option>
        {/each}
      </select>
    </div>
  {/if}

  <section class="card">
    <table>
      <thead>
        <tr>
          <th>Name</th>
          <th>Namespace</th>
          <th>Created</th>
          <th>Updated</th>
        </tr>
      </thead>
      <tbody>
        {#each filtered as s (s.id)}
          <tr>
            <td class="mono">{s.name}</td>
            <td>
              <a class="ns-link" href="/secrets?namespace={s.namespace}">{s.namespace}</a>
            </td>
            <td class="muted">{formatDate(s.created_at)}</td>
            <td class="muted">{s.updated_at ? formatDate(s.updated_at) : '—'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>
{/if}

{#if !loading && secrets.length === 0 && !errorMsg}
  <div class="empty">
    <p>No secrets yet.</p>
    <p class="muted">
      Create one with <code>ring secret create &lt;name&gt; --namespace &lt;ns&gt; --value
      &lt;value&gt;</code>. The value is encrypted before being stored and is never returned by the
      API.
    </p>
  </div>
{/if}

<style>
  .ns-link {
    color: var(--fg-0);
    font-weight: 500;
  }
  .ns-link:hover {
    color: var(--accent);
  }
  td.mono {
    font-family: var(--font-mono);
  }
</style>

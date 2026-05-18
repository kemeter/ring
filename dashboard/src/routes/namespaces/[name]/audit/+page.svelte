<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { page } from '$app/stores';
  import { getNamespaceAudit, type AuditEntry } from '$lib/api';
  import { getToken } from '$lib/auth';
  import { formatDate, timeAgo } from '$lib/utils';

  let name = $derived($page.params.name ?? '');
  let entries = $state<AuditEntry[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;

  async function refresh() {
    try {
      entries = await getNamespaceAudit(name);
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

  function actionClass(action: string): string {
    if (action === 'delete') return 'warn';
    if (action === 'create') return 'success';
    return '';
  }
</script>

<header class="page-header">
  <div>
    <h1>Audit — {name}</h1>
    <p class="subtitle">Who changed what in this namespace</p>
  </div>
  <div class="header-actions">
    {#if lastFetch}
      <span class="refresh-meta">updated {timeAgo(lastFetch)}</span>
    {/if}
    <a class="btn-secondary" href="/namespaces">Back</a>
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

{#if !loading && entries.length > 0}
  <section class="card">
    <table>
      <thead>
        <tr>
          <th>When</th>
          <th>User</th>
          <th>Action</th>
          <th>Type</th>
          <th>Target</th>
        </tr>
      </thead>
      <tbody>
        {#each entries as e (e.id)}
          <tr>
            <td class="muted">{formatDate(e.timestamp)}</td>
            <td class="mono">{e.user_id ?? '—'}</td>
            <td>
              <span class="dot-inline {actionClass(e.action)}"></span>{e.action}
            </td>
            <td class="muted">{e.target_type}</td>
            <td class="mono">{e.target_name}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  </section>
{/if}

{#if !loading && entries.length === 0 && !errorMsg}
  <div class="empty">
    <p>No audit entries for <code>{name}</code>.</p>
    <p class="muted">
      Write actions (create / update / delete on deployments, secrets, configs) are recorded here as
      they happen.
    </p>
  </div>
{/if}

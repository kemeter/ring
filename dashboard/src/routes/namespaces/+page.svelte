<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import ConfirmAction from '#lib/ConfirmAction.svelte';
  import CreatePanel from '#lib/CreatePanel.svelte';
  import {
    createNamespace,
    deleteNamespace,
    listDeployments,
    listNamespaces,
    setNamespaceSecurityLevel,
    SECURITY_LEVELS,
    type Deployment,
    type Namespace,
    type SecurityLevel
  } from '#lib/api.ts';
  import { hasSession } from '#lib/auth.ts';
  import { formatDate, timeAgo } from '#lib/utils.ts';

  let namespaces = $state<Namespace[]>([]);
  let deployments = $state<Deployment[]>([]);
  let loading = $state(true);
  let errorMsg = $state<string | null>(null);
  let lastFetch = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;
  let creating = $state(false);
  let newName = $state('');

  async function create() {
    await createNamespace(newName);
    newName = '';
    await refresh();
  }

  /** Level picked in a row, waiting for confirmation. */
  let pickedLevel = $state<Record<string, SecurityLevel | ''>>({});

  async function applyLevel(name: string, level: SecurityLevel) {
    await setNamespaceSecurityLevel(name, level);
    pickedLevel[name] = '';
    await refresh();
  }

  async function remove(name: string) {
    await deleteNamespace(name);
    await refresh();
  }

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
    if (!hasSession()) {
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

  function deploymentCount(name: string): number {
    return deployments.filter((d) => d.namespace === name).length;
  }

  function runningCount(name: string): number {
    return deployments.filter(
      (d) => d.namespace === name && d.status === 'running'
    ).length;
  }

  let totalDeployments = $derived(deployments.length);
  let usedNamespaces = $derived(
    new Set(deployments.map((d) => d.namespace)).size
  );
</script>

<svelte:head><title>Ring · Namespaces</title></svelte:head>

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
    <button class="btn-primary" type="button" onclick={() => (creating = true)}>New namespace</button>
  </div>
</header>

<CreatePanel bind:open={creating} title="New namespace" submit={create}>
  <div class="form-grid">
    <label class="field">
      <span>Name</span>
      <input bind:value={newName} required autocomplete="off" />
      <span class="hint">Lowercase letters, digits and '-'.</span>
    </label>
  </div>
</CreatePanel>

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
          <th>Security level</th>
          <th>Created</th>
          <th>Updated</th>
          <th></th>
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
            <td class="mono">{ns.security_level}</td>
            <td class="muted">{formatDate(ns.created_at)}</td>
            <td class="muted">{ns.updated_at ? formatDate(ns.updated_at) : '—'}</td>
            <td class="actions">
              {#if pickedLevel[ns.name]}
                {@const level = pickedLevel[ns.name] as SecurityLevel}
                <ConfirmAction
                  label={`Make ${level}`}
                  prompt={level === 'privileged'
                    ? `Deployments of ${ns.name} may then reach the host, and every other namespace.`
                    : `Deployments of ${ns.name} that ${level} does not allow stop starting new instances.`}
                  pendingLabel="Saving…"
                  action={() => applyLevel(ns.name, level)}
                  startConfirming
                  oncancel={() => (pickedLevel[ns.name] = '')}
                />
              {:else}
                <a class="ns-link audit" href="/namespaces/{ns.name}/audit">Audit</a>
                <select
                  class="level-picker"
                  bind:value={pickedLevel[ns.name]}
                  aria-label={`Change the security level of ${ns.name}`}
                >
                  <option value="">Change level…</option>
                  {#each SECURITY_LEVELS.filter((l) => l !== ns.security_level) as l}
                    <option value={l}>{l}</option>
                  {/each}
                </select>
                <ConfirmAction
                  label="Delete"
                  prompt={`Delete namespace ${ns.name}?`}
                  pendingLabel="Deleting…"
                  action={() => remove(ns.name)}
                />
              {/if}
            </td>
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
  .level-picker {
    margin-right: 0.4rem;
    background: var(--bg-2);
    color: var(--fg-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.45rem 0.5rem;
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

  td.num,
  th.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  td.mono {
    font-family: var(--font-mono);
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
  .audit {
    margin-right: 0.75rem;
  }
</style>

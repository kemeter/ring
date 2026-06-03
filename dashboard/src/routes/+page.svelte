<script lang="ts">
  import { onDestroy, onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import {
    getNode,
    listConfigs,
    listDeployments,
    listNamespaces,
    listSecrets,
    type Deployment,
    type NodeInfo
  } from '$lib/api';
  import { getToken } from '$lib/auth';
  import { timeAgo } from '$lib/utils';

  let deployments = $state<Deployment[]>([]);
  let namespaceCount = $state(0);
  let secretCount = $state(0);
  let configCount = $state(0);
  let node = $state<NodeInfo | null>(null);
  let firstLoad = $state(true);
  let refreshing = $state(false);
  let errorMsg = $state<string | null>(null);
  let lastRefresh = $state<Date | null>(null);
  let poll: ReturnType<typeof setInterval> | null = null;

  /** Classify a deployment status into one of four buckets, matching the
   *  deployments list page. */
  function statusKind(s: string): 'success' | 'warn' | 'danger' | 'neutral' {
    const k = s.toLowerCase();
    if (k === 'running') {
      return 'success';
    }
    if (
      k === 'failed' ||
      k === 'crashloopbackoff' ||
      k === 'crash_loop_back_off' ||
      k === 'error' ||
      k === 'createcontainererror' ||
      k === 'create_container_error' ||
      k === 'imagepullbackoff' ||
      k === 'image_pull_back_off'
    ) {
      return 'danger';
    }
    if (
      k === 'pending' ||
      k === 'booting' ||
      k === 'created' ||
      k === 'creating' ||
      k === 'starting'
    ) {
      return 'warn';
    }
    return 'neutral';
  }

  let running = $derived(deployments.filter((d) => statusKind(d.status) === 'success').length);
  let failing = $derived(deployments.filter((d) => statusKind(d.status) === 'danger').length);
  let pending = $derived(deployments.filter((d) => statusKind(d.status) === 'warn').length);
  let failingDeployments = $derived(deployments.filter((d) => statusKind(d.status) === 'danger'));

  /** Global system status pill in the top bar: red if anything is failing,
   *  amber if something is still converging, green otherwise. */
  let systemStatus = $derived.by<{ label: string; tone: 'ok' | 'warn' | 'err' }>(() => {
    if (failing > 0) {
      return { label: 'degraded', tone: 'err' };
    }
    if (pending > 0) {
      return { label: 'converging', tone: 'warn' };
    }
    return { label: 'running', tone: 'ok' };
  });

  let memUsedPercent = $derived(
    node && node.memory_total > 0
      ? ((node.memory_total - node.memory_available) / node.memory_total) * 100
      : 0
  );

  async function load() {
    refreshing = true;
    try {
      // Each source degrades independently: a single failing call shouldn't
      // blank the whole overview.
      const [deps, ns, secrets, configs, nodeInfo] = await Promise.all([
        listDeployments(),
        listNamespaces().catch(() => null),
        listSecrets().catch(() => null),
        listConfigs().catch(() => null),
        getNode().catch(() => null)
      ]);
      deployments = deps;
      if (ns) namespaceCount = ns.length;
      if (secrets) secretCount = secrets.length;
      if (configs) configCount = configs.length;
      node = nodeInfo;
      errorMsg = null;
    } catch (e) {
      errorMsg = e instanceof Error ? e.message : String(e);
    } finally {
      firstLoad = false;
      refreshing = false;
      lastRefresh = new Date();
    }
  }

  onMount(() => {
    if (!getToken()) {
      goto('/login');
      return;
    }
    void load();
    poll = setInterval(() => void load(), 5000);
  });

  onDestroy(() => {
    if (poll) {
      clearInterval(poll);
    }
  });
</script>

<svelte:head><title>Ring · Overview</title></svelte:head>

<main class="overview">
  <header class="topbar">
    <div>
      <h1>Overview</h1>
      <div class="sub">
        All systems at a glance. {running} of {deployments.length} deployments running.
      </div>
    </div>
    <div class="topbar-right">
      <span class="status-pill status-{systemStatus.tone}">
        <span class="status-ind"></span>
        {systemStatus.label}
      </span>
      {#if refreshing}
        <span class="spinner" aria-label="refreshing" title="refreshing…"></span>
      {/if}
      {#if lastRefresh}
        <span class="updated">updated {timeAgo(lastRefresh)}</span>
      {/if}
    </div>
  </header>

  {#if errorMsg}
    <div class="alert"><strong>error</strong> {errorMsg}</div>
  {/if}

  {#if firstLoad}
    <section class="grid">
      {#each Array(4) as _}
        <article class="card card-skeleton">
          <div class="skeleton-line skeleton-title"></div>
          <div class="skeleton-line skeleton-value"></div>
          <div class="skeleton-line skeleton-sub"></div>
        </article>
      {/each}
    </section>
  {:else}
    <section class="grid">
      <!-- Deployments -->
      <article class="card">
        <div class="head">
          <span class="title">Deployments</span>
          <span class="pill pill-{failing > 0 ? 'err' : 'ok'}">
            <span class="dot"></span>
            {failing > 0 ? `${failing} failing` : 'all up'}
          </span>
        </div>
        <div class="value">
          <span class="value-ok">{running}</span><small>/ {deployments.length} running</small>
        </div>
        <div class="sub">
          {running} running &nbsp;·&nbsp; {pending} pending &nbsp;·&nbsp; {failing} failing
        </div>
        <div class="foot">
          <span></span>
          <a class="link" href="/deployments">View all <span class="arr">→</span></a>
        </div>
      </article>

      <!-- Namespaces -->
      <article class="card">
        <div class="head">
          <span class="title">Namespaces</span>
          <span class="pill"><span class="dot"></span> scoped</span>
        </div>
        <div class="value">{namespaceCount}<small>namespaces</small></div>
        <div class="sub">isolation boundary for deployments, secrets and configs</div>
        <div class="foot">
          <span></span>
          <a class="link" href="/namespaces">Manage <span class="arr">→</span></a>
        </div>
      </article>

      <!-- Secrets -->
      <article class="card">
        <div class="head">
          <span class="title">Secrets</span>
          <span class="pill"><span class="dot"></span> encrypted</span>
        </div>
        <div class="value">{secretCount}<small>secrets</small></div>
        <div class="sub">stored encrypted, injected as env or mounted as files</div>
        <div class="foot">
          <span></span>
          <a class="link" href="/secrets">View all <span class="arr">→</span></a>
        </div>
      </article>

      <!-- Configs -->
      <article class="card">
        <div class="head">
          <span class="title">Configs</span>
          <span class="pill"><span class="dot"></span> ready</span>
        </div>
        <div class="value">{configCount}<small>configs</small></div>
        <div class="sub">non-secret config data mounted into deployments</div>
        <div class="foot">
          <span></span>
          <a class="link" href="/configs">View all <span class="arr">→</span></a>
        </div>
      </article>

      <!-- Node -->
      <article class="card card-wide node-card">
        <div class="head">
          <span class="title">Node</span>
          <span class="pill pill-ok"><span class="dot"></span> {node ? 'online' : 'unknown'}</span>
        </div>
        {#if node}
          <dl class="node-dl">
            <dt>Host</dt>
            <dd class="mono">{node.hostname}</dd>
            <dt>CPU cores</dt>
            <dd class="mono">{node.cpu_count}</dd>
            <dt>Memory</dt>
            <dd class="mono">
              {(node.memory_total - node.memory_available).toFixed(1)} / {node.memory_total.toFixed(
                1
              )} GiB <span class="node-sub">({memUsedPercent.toFixed(0)}%)</span>
            </dd>
            <dt>Load</dt>
            <dd class="mono">{node.load_average.map((l) => l.toFixed(2)).join(', ')}</dd>
          </dl>
        {:else}
          <p class="node-empty">Node info unavailable.</p>
        {/if}
        <div class="foot">
          <span></span>
          <a class="link" href="/node">Details <span class="arr">→</span></a>
        </div>
      </article>

      <!-- Critical / failing -->
      <article class="card card-wide">
        <div class="head">
          <span class="title">Critical</span>
          <span class="pill pill-{failingDeployments.length > 0 ? 'err' : 'ok'}">
            <span class="dot"></span>
            {failingDeployments.length > 0 ? `${failingDeployments.length} failing` : 'all clear'}
          </span>
        </div>
        {#if failingDeployments.length === 0}
          <div class="value"><span class="value-ok">0</span><small>events</small></div>
          <div class="sub">No failed or crash-looping deployments. Everything reconciled.</div>
          <div class="foot">
            <span></span>
            <a class="link" href="/deployments">Deployments <span class="arr">→</span></a>
          </div>
        {:else}
          <ul class="fail-list">
            {#each failingDeployments as d (d.id)}
              <li>
                <a href="/deployments/{d.id}">
                  <span class="fail-name">{d.name}</span>
                  <span class="fail-ns">{d.namespace}</span>
                  <span class="fail-status">{d.status}</span>
                </a>
              </li>
            {/each}
          </ul>
        {/if}
      </article>
    </section>
  {/if}
</main>

<style>
  .overview {
    padding: 8px 0 16px;
    max-width: 1440px;
    display: flex;
    flex-direction: column;
    gap: 16px;
  }
  .topbar {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    margin-bottom: 4px;
  }
  h1 {
    margin: 0;
    font-size: 1.4rem;
    letter-spacing: -0.02em;
  }
  .topbar-right {
    display: flex;
    align-items: center;
    gap: 14px;
    font-size: 12px;
    color: var(--fg-2);
  }
  .updated {
    color: var(--fg-3);
  }
  .sub {
    color: var(--fg-2);
    font-size: 13px;
    margin-top: 4px;
  }

  .status-pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border-radius: 999px;
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }
  .status-pill .status-ind {
    width: 7px;
    height: 7px;
    border-radius: 50%;
  }
  .status-ok {
    color: var(--success);
    background: var(--success-bg);
  }
  .status-ok .status-ind {
    background: var(--success);
  }
  .status-warn {
    color: var(--warning);
    background: var(--warning-bg);
  }
  .status-warn .status-ind {
    background: var(--warning);
  }
  .status-err {
    color: var(--danger);
    background: var(--danger-bg);
  }
  .status-err .status-ind {
    background: var(--danger);
  }

  .grid {
    display: grid;
    grid-template-columns: repeat(4, 1fr);
    gap: 14px;
  }
  .card {
    background: var(--bg-1);
    border: 1px solid var(--border);
    border-radius: 10px;
    padding: 18px 20px 16px;
    min-height: 132px;
    display: flex;
    flex-direction: column;
    gap: 12px;
    transition:
      background 160ms ease,
      border-color 160ms ease;
  }
  .card-wide {
    grid-column: span 2;
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .title {
    font-size: 11px;
    text-transform: uppercase;
    letter-spacing: 1.6px;
    color: var(--fg-2);
    font-weight: 500;
  }
  .pill {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 1.2px;
    color: var(--fg-3);
  }
  .pill-ok {
    color: var(--success);
  }
  .pill-err {
    color: var(--danger);
  }
  .pill .dot {
    width: 6px;
    height: 6px;
    border-radius: 50%;
    background: var(--fg-3);
  }
  .pill-ok .dot {
    background: var(--success);
    box-shadow: 0 0 6px rgba(63, 185, 80, 0.5);
  }
  .pill-err .dot {
    background: var(--danger);
    box-shadow: 0 0 6px rgba(248, 81, 73, 0.5);
  }
  .value {
    font-size: 34px;
    font-weight: 500;
    letter-spacing: -0.8px;
    line-height: 1;
    color: var(--fg-0);
    font-variant-numeric: tabular-nums;
    display: flex;
    align-items: baseline;
    gap: 8px;
  }
  .value small {
    font-size: 12px;
    color: var(--fg-2);
    font-weight: 400;
    letter-spacing: 0;
  }
  .value-ok {
    color: var(--success);
  }
  .foot {
    margin-top: auto;
    display: flex;
    align-items: flex-end;
    justify-content: space-between;
    padding-top: 6px;
  }
  .link {
    color: var(--fg-2);
    text-decoration: none;
    font-size: 12px;
  }
  .link:hover {
    color: var(--fg-0);
  }
  .link .arr {
    display: inline-block;
    transition: transform 120ms ease;
    margin-left: 4px;
  }
  .link:hover .arr {
    transform: translateX(3px);
    color: var(--accent);
  }

  /* Node card: detailed key/value layout instead of a single big value. */
  .node-card {
    gap: 14px;
  }
  .node-dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    column-gap: 1.5rem;
    row-gap: 0.5rem;
    margin: 0;
  }
  .node-dl dt {
    color: var(--fg-2);
    font-size: 0.78rem;
    text-transform: uppercase;
    letter-spacing: 0.05em;
  }
  .node-dl dd {
    margin: 0;
    color: var(--fg-0);
    font-size: 0.85rem;
  }
  .mono {
    font-family: var(--font-mono);
  }
  .node-sub {
    color: var(--fg-3);
    font-size: 0.72rem;
  }
  .node-empty {
    color: var(--fg-2);
    font-size: 0.85rem;
    margin: 0;
  }

  /* Critical card: a list of failing deployments instead of a single value. */
  .fail-list {
    list-style: none;
    margin: 0;
    padding: 0;
    display: flex;
    flex-direction: column;
  }
  .fail-list li a {
    display: grid;
    grid-template-columns: 1fr auto auto;
    gap: 0.75rem;
    align-items: center;
    padding: 0.45rem 0;
    border-bottom: 1px solid var(--border);
    font-size: 0.82rem;
    color: var(--fg-0);
  }
  .fail-list li:last-child a {
    border-bottom: none;
  }
  .fail-list li a:hover .fail-name {
    color: var(--accent);
  }
  .fail-name {
    font-weight: 500;
  }
  .fail-ns {
    color: var(--fg-2);
    font-size: 0.75rem;
  }
  .fail-status {
    color: var(--danger);
    font-size: 0.72rem;
    font-family: var(--font-mono);
  }

  .spinner {
    width: 12px;
    height: 12px;
    border: 2px solid var(--border);
    border-top-color: var(--accent);
    border-radius: 50%;
    animation: spin 0.6s linear infinite;
  }
  @keyframes spin {
    to {
      transform: rotate(360deg);
    }
  }

  /* Skeleton shimmer until the first payload lands. */
  .card-skeleton {
    gap: 14px;
  }
  .skeleton-line {
    height: 12px;
    border-radius: 4px;
    background: var(--bg-2);
  }
  .skeleton-title {
    width: 40%;
  }
  .skeleton-value {
    width: 55%;
    height: 28px;
  }
  .skeleton-sub {
    width: 75%;
  }

  @media (max-width: 1100px) {
    .grid {
      grid-template-columns: repeat(2, 1fr);
    }
  }
</style>

<script lang="ts">
  /**
   * The log viewer of the deployment detail page: snapshot load, live SSE
   * stream, follow/pause, tail size, auto-scroll and level colouring.
   *
   * It owns its own stream lifecycle — starting on mount, restarting when the
   * deployment id changes, and closing on destroy — so the parent page only
   * has to place it and pass an id.
   */
  import { onDestroy, untrack } from 'svelte';
  import { fetchLogsSnapshot, streamLogs, type LogEntry, type LogStreamHandle } from '$lib/api';

  interface Props {
    /** Deployment whose logs to show. Changing it reloads the viewer. */
    id: string;
  }

  let { id }: Props = $props();

  /** Hard cap to bound memory when a chatty deployment streams for hours.
   *  Older entries fall off the top once we cross this. */
  const LOG_RING_MAX = 2000;

  let logs = $state<LogEntry[]>([]);
  let logsLoading = $state(false);
  let logsError = $state<string | null>(null);
  let logsFollow = $state(true);
  let logsTail = $state(200);
  let logsAutoScroll = $state(true);
  let logsContainerFilter = $state('');
  let logsStream: LogStreamHandle | null = null;
  let logsEl: HTMLDivElement | null = $state(null);

  /** Incremented on every reload. A snapshot or stream that resolves after a
   *  newer reload started belongs to a superseded deployment (or tail size) and
   *  must not touch the state — otherwise a slow response for the previous
   *  deployment could overwrite the current one's logs. */
  let generation = 0;

  function pushLog(entry: LogEntry) {
    logs.push(entry);
    if (logs.length > LOG_RING_MAX) {
      logs.splice(0, logs.length - LOG_RING_MAX);
    }
  }

  function stopStream() {
    if (logsStream) {
      logsStream.close();
      logsStream = null;
    }
  }

  async function loadSnapshot(gen: number) {
    if (!id) {
      return;
    }
    logsLoading = true;
    logsError = null;
    try {
      const snap = await fetchLogsSnapshot(id, {
        tail: logsTail,
        container: logsContainerFilter || undefined
      });
      if (gen !== generation) {
        return;
      }
      logs = snap;
    } catch (e) {
      if (gen !== generation) {
        return;
      }
      logsError = e instanceof Error ? e.message : String(e);
    } finally {
      if (gen === generation) {
        logsLoading = false;
      }
    }
  }

  async function startStream(gen: number) {
    if (!id) {
      return;
    }
    stopStream();
    logsError = null;
    try {
      const handle = await streamLogs(
        id,
        { tail: logsTail, container: logsContainerFilter || undefined },
        (entry) => {
          if (gen !== generation) {
            return;
          }
          pushLog(entry);
        },
        () => {
          if (gen !== generation) {
            return;
          }
          // EventSource fires a generic Event on disconnect; surface a hint
          // without killing the page. The browser will auto-reconnect using
          // the same URL while the ticket is still within its TTL.
          logsError = 'stream interrupted, reconnecting…';
        }
      );
      if (gen !== generation) {
        // A newer reload started while the ticket was in flight; this stream
        // belongs to the previous deployment, so close it instead of letting
        // it leak alongside the current one.
        handle.close();
        return;
      }
      logsStream = handle;
    } catch (e) {
      if (gen !== generation) {
        return;
      }
      logsError = e instanceof Error ? e.message : String(e);
    }
  }

  async function refreshLogs() {
    stopStream();
    const gen = ++generation;
    // Don't clear `logs` here: emptying the array collapses the viewport,
    // which makes the page jump to the top because the scroll position
    // becomes out of range. Keep the old lines until the snapshot lands
    // and then swap atomically inside loadSnapshot().
    await loadSnapshot(gen);
    if (gen !== generation) {
      return;
    }
    if (logsFollow) {
      await startStream(gen);
    }
  }

  async function toggleFollow() {
    logsFollow = !logsFollow;
    if (logsFollow) {
      await startStream(++generation);
    } else {
      stopStream();
    }
  }

  function levelClass(level: string): string {
    const l = level.toLowerCase();
    if (l === 'error' || l === 'err' || l === 'critical' || l === 'fatal') {
      return 'log-error';
    }
    if (l === 'warn' || l === 'warning') {
      return 'log-warn';
    }
    if (l === 'info' || l === 'notice') {
      return 'log-info';
    }
    if (l === 'debug') {
      return 'log-debug';
    }
    return 'log-unknown';
  }

  $effect(() => {
    if (logsAutoScroll && logsEl && logs.length > 0) {
      // Read .length to register the dependency, then defer the scroll
      // until after the DOM has the new row.
      const _ = logs.length;
      void _;
      queueMicrotask(() => {
        if (logsEl) {
          logsEl.scrollTop = logsEl.scrollHeight;
        }
      });
    }
  });

  // Reload whenever the deployment changes, not just on mount: SvelteKit reuses
  // this page across `/deployments/[id]` navigations (the detail page links to
  // a deployment's parent), so `onMount` would not fire again and the viewer
  // would keep streaming the previous deployment's logs.
  $effect(() => {
    const target = id;
    if (!target) {
      return;
    }
    // `id` above is the only intended dependency. The reload itself reads
    // `logsTail`/`logsContainerFilter` and writes `logs`, all of which are
    // reactive — without `untrack` this effect would re-subscribe to them and
    // re-run on its own writes.
    untrack(() => {
      stopStream();
      // Drop the previous deployment's lines immediately — unlike a reload of
      // the same deployment, keeping them would show another deployment's
      // output.
      logs = [];
      void refreshLogs();
    });
  });

  onDestroy(stopStream);
</script>

<section class="card">
  <header class="section-head logs-head">
    <h2>Logs</h2>
    <div class="logs-controls">
      <label class="logs-control">
        <span>Tail</span>
        <select bind:value={logsTail} onchange={refreshLogs}>
          <option value={100}>100</option>
          <option value={200}>200</option>
          <option value={500}>500</option>
          <option value={1000}>1000</option>
        </select>
      </label>
      <label class="logs-control checkbox">
        <input type="checkbox" bind:checked={logsAutoScroll} />
        <span>Auto-scroll</span>
      </label>
      <button
        class="btn-secondary small"
        class:active={logsFollow}
        onclick={toggleFollow}
        title={logsFollow ? 'Pause live stream' : 'Resume live stream'}
      >
        {logsFollow ? '⏸ Pause' : '▶ Live'}
      </button>
      <button class="btn-secondary small" onclick={refreshLogs} disabled={logsLoading}>
        Reload
      </button>
    </div>
  </header>
  {#if logsError}
    <div class="logs-error">{logsError}</div>
  {/if}
  <div class="logs-viewport mono" bind:this={logsEl}>
    {#if logsLoading && logs.length === 0}
      <p class="muted pad">Loading logs…</p>
    {:else if logs.length === 0}
      <p class="muted pad">No logs yet.</p>
    {:else}
      {#each logs as entry, i (i)}
        <div class="log-line">
          {#if entry.timestamp}
            <span class="log-ts">{entry.timestamp}</span>
          {/if}
          <span class="log-level {levelClass(entry.level)}">{entry.level}</span>
          <span class="log-instance">{entry.instance}</span>
          <span class="log-msg">{entry.message}</span>
        </div>
      {/each}
    {/if}
  </div>
</section>

<style>
  /* Structural rules the detail page defines for its own cards. Svelte scopes
   * styles per component, so they don't reach this child's DOM — the ones this
   * component's markup actually uses are repeated here rather than made global,
   * keeping the page's scoping intact. */
  .card {
    margin-bottom: 1rem;
  }
  .section-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.85rem 1.125rem;
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .muted.pad {
    padding: 1.25rem;
  }

  .logs-head {
    gap: 0.75rem;
  }
  .logs-controls {
    display: flex;
    align-items: center;
    gap: 0.6rem;
  }
  .logs-control {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.75rem;
    color: var(--fg-2);
  }
  .logs-control select {
    font-size: 0.78rem;
    padding: 0.18rem 0.35rem;
    background: var(--bg-0);
    color: var(--fg-0);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
  }
  .logs-control.checkbox input {
    margin: 0;
  }
  .btn-secondary.small {
    font-size: 0.75rem;
    padding: 0.25rem 0.6rem;
  }
  .btn-secondary.active {
    color: var(--accent);
    border-color: var(--accent);
  }
  .logs-error {
    padding: 0.45rem 1.125rem;
    background: var(--warning-bg);
    color: var(--warning);
    font-size: 0.78rem;
    border-bottom: 1px solid var(--border);
  }
  .logs-viewport {
    max-height: 28rem;
    overflow-y: auto;
    background: var(--bg-0);
    font-size: 0.78rem;
    line-height: 1.45;
  }
  .log-line {
    display: grid;
    grid-template-columns: auto auto auto 1fr;
    column-gap: 0.65rem;
    padding: 0.18rem 1.125rem;
    border-bottom: 1px solid transparent;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .log-line:hover {
    background: var(--bg-1);
  }
  .log-ts {
    color: var(--fg-3);
    font-size: 0.72rem;
  }
  .log-level {
    text-transform: uppercase;
    font-size: 0.68rem;
    letter-spacing: 0.04em;
    padding: 0 0.3rem;
    border-radius: var(--radius-sm);
    align-self: center;
    min-width: 4rem;
    text-align: center;
  }
  .log-error {
    color: var(--danger);
    background: var(--danger-bg);
  }
  .log-warn {
    color: var(--warning);
    background: var(--warning-bg);
  }
  .log-info {
    color: var(--success);
    background: var(--success-bg);
  }
  .log-debug {
    color: var(--fg-2);
    background: var(--bg-2);
  }
  .log-unknown {
    color: var(--fg-3);
  }
  .log-instance {
    color: var(--fg-2);
    font-size: 0.72rem;
  }
  .log-msg {
    color: var(--fg-0);
  }
</style>

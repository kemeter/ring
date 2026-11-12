<script lang="ts">
  /**
   * An interactive shell inside one of the deployment's instances, over the
   * exec WebSocket. Nothing is opened until the user asks: a session is a
   * process in the workload, not something a page view should start.
   */
  import { onDestroy } from 'svelte';
  import type { FitAddon } from '@xterm/addon-fit';
  import type { Terminal } from '@xterm/xterm';
  import '@xterm/xterm/css/xterm.css';
  import { openExec, type DeploymentInstance, type ExecSession } from '#lib/api.ts';

  interface Props {
    /** Deployment to open the session in. */
    id: string;
    /** Running instances, to pick which one to enter. */
    instances: DeploymentInstance[];
  }

  let { id, instances }: Props = $props();

  type Phase = 'idle' | 'connecting' | 'open' | 'ended';

  let phase = $state<Phase>('idle');
  let shell = $state('/bin/sh');
  let instance = $state('');
  let status = $state<string | null>(null);
  let error = $state<string | null>(null);
  let screenEl: HTMLDivElement | null = $state(null);

  let term: Terminal | null = null;
  let fit: FitAddon | null = null;
  let session: ExecSession | null = null;
  let observer: ResizeObserver | null = null;

  function cssVar(name: string): string {
    return getComputedStyle(document.documentElement).getPropertyValue(name).trim();
  }

  async function connect() {
    if (!screenEl) {
      return;
    }
    teardown();
    phase = 'connecting';
    error = null;
    status = null;

    const [{ Terminal }, { FitAddon }] = await Promise.all([
      import('@xterm/xterm'),
      import('@xterm/addon-fit')
    ]);
    term = new Terminal({
      cursorBlink: true,
      fontFamily: cssVar('--font-mono'),
      fontSize: 13,
      theme: {
        background: cssVar('--bg-0'),
        foreground: cssVar('--fg-0'),
        cursor: cssVar('--accent')
      }
    });
    fit = new FitAddon();
    term.loadAddon(fit);
    term.open(screenEl);
    fit.fit();

    try {
      session = await openExec(
        id,
        {
          command: [shell.trim() || '/bin/sh'],
          container: instance || undefined,
          cols: term.cols,
          rows: term.rows
        },
        {
          onOutput: (data) => term?.write(data),
          onExit: (code) => {
            status = code === null ? 'Process exited.' : `Process exited with code ${code}.`;
          },
          onError: (message) => {
            error = message;
          },
          onClose: () => {
            phase = 'ended';
            status ??= 'Session closed.';
            if (term) {
              term.options.cursorBlink = false;
            }
          }
        }
      );
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
      phase = 'ended';
      return;
    }

    phase = 'open';
    term.onData((data) => session?.send(data));
    term.onResize(({ cols, rows }) => session?.resize(cols, rows));
    observer = new ResizeObserver(() => fit?.fit());
    observer.observe(screenEl);
    term.focus();
  }

  function disconnect() {
    session?.close();
  }

  function teardown() {
    observer?.disconnect();
    observer = null;
    session?.close();
    session = null;
    term?.dispose();
    term = null;
    fit = null;
  }

  onDestroy(teardown);
</script>

<section class="card">
  <header class="section-head">
    <h2>Terminal</h2>
    {#if phase === 'open'}
      <span class="count live">connected</span>
    {/if}
  </header>

  <div class="controls">
    {#if instances.length > 1}
      <label>
        <span>Instance</span>
        <select bind:value={instance} disabled={phase === 'connecting' || phase === 'open'}>
          <option value="">first running</option>
          {#each instances as inst (inst.id)}
            <option value={inst.id}>{inst.id.slice(0, 12)}</option>
          {/each}
        </select>
      </label>
    {/if}
    <label>
      <span>Command</span>
      <input
        class="mono"
        type="text"
        bind:value={shell}
        disabled={phase === 'connecting' || phase === 'open'}
        spellcheck="false"
      />
    </label>
    {#if phase === 'open'}
      <button class="btn-secondary" type="button" onclick={disconnect}>Disconnect</button>
    {:else}
      <button
        class="btn-secondary"
        type="button"
        onclick={connect}
        disabled={phase === 'connecting' || instances.length === 0}
      >
        {phase === 'connecting' ? 'Connecting…' : phase === 'ended' ? 'Reconnect' : 'Connect'}
      </button>
    {/if}
    {#if instances.length === 0}
      <span class="muted">No running instance to enter.</span>
    {/if}
  </div>

  {#if error}
    <p class="message error">{error}</p>
  {:else if status}
    <p class="message">{status}</p>
  {/if}

  <div class="screen" class:hidden={phase === 'idle'} bind:this={screenEl}></div>
</section>

<style>
  /* The detail page's card rules don't reach this child (Svelte scopes styles
   * per component), so the ones used here are repeated, as DeploymentLogs does. */
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
  .count {
    font-size: 0.75rem;
  }
  .controls {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-end;
    gap: 0.75rem;
    padding: 0.85rem 1.125rem;
  }
  label {
    display: flex;
    flex-direction: column;
    gap: 0.25rem;
    font-size: 0.7rem;
    color: var(--fg-2);
  }
  input,
  select {
    background: var(--bg-0);
    color: var(--fg-0);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.4rem 0.6rem;
    font-size: 0.8rem;
  }
  input {
    width: 14rem;
    max-width: 100%;
  }
  .message {
    margin: 0;
    padding: 0 1.125rem 0.75rem;
    font-size: 0.8rem;
    color: var(--fg-2);
  }
  .message.error {
    color: var(--danger);
  }
  .screen {
    height: 360px;
    padding: 0.5rem;
    background: var(--bg-0);
    border-top: 1px solid var(--border);
  }
  .screen.hidden {
    display: none;
  }
  .live {
    color: var(--success);
  }
</style>

<script lang="ts">
  /**
   * Create a deployment, or roll out a new version of an existing one (the
   * API treats a POST with an existing name + namespace as a rolling update).
   * The form covers the common fields; "Edit as JSON" opens the whole body
   * for everything else (volumes, health checks, resources, network…).
   */
  import { goto } from '$app/navigation';
  import { RUNTIMES, createDeployment, type DeploymentInput, type EnvValue } from '#lib/api.ts';

  interface EnvRow {
    key: string;
    value: string;
    secret: boolean;
  }

  interface PortRow {
    published: number | null;
    target: number | null;
    protocol: 'tcp' | 'udp';
  }

  let form = $state({
    namespace: 'default',
    name: '',
    image: '',
    runtime: 'docker',
    kind: 'worker' as 'worker' | 'job',
    replicas: 1,
    command: ''
  });
  let env = $state<EnvRow[]>([]);
  let ports = $state<PortRow[]>([]);

  let jsonMode = $state(false);
  let json = $state('');
  let pending = $state(false);
  let error = $state<string | null>(null);

  /** The request body the form describes. */
  function fromForm(): DeploymentInput {
    const body: DeploymentInput = {
      namespace: form.namespace.trim(),
      name: form.name.trim(),
      image: form.image.trim(),
      runtime: form.runtime,
      kind: form.kind,
      replicas: form.replicas
    };
    // One argument per line keeps arguments with spaces intact.
    const command = form.command
      .split('\n')
      .map((arg) => arg.trim())
      .filter(Boolean);
    if (command.length > 0) {
      body.command = command;
    }
    const environment: Record<string, EnvValue> = {};
    for (const row of env) {
      if (row.key.trim()) {
        environment[row.key.trim()] = row.secret ? { secretRef: row.value } : row.value;
      }
    }
    if (Object.keys(environment).length > 0) {
      body.environment = environment;
    }
    const published = ports.filter((p) => p.published && p.target);
    if (published.length > 0) {
      body.ports = published.map((p) => ({
        published: Number(p.published),
        target: Number(p.target),
        protocol: p.protocol
      }));
    }
    return body;
  }

  function toggleJson() {
    if (!jsonMode) {
      json = JSON.stringify(fromForm(), null, 2);
      error = null;
    }
    jsonMode = !jsonMode;
  }

  async function submit(event: SubmitEvent) {
    event.preventDefault();
    error = null;
    let body: DeploymentInput;
    if (jsonMode) {
      try {
        body = JSON.parse(json) as DeploymentInput;
      } catch (e) {
        error = `Invalid JSON: ${e instanceof Error ? e.message : String(e)}`;
        return;
      }
    } else {
      body = fromForm();
    }
    pending = true;
    try {
      const created = await createDeployment(body);
      goto(`/deployments/${created.id}`);
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      pending = false;
    }
  }
</script>

<svelte:head><title>Ring · New deployment</title></svelte:head>

<nav class="breadcrumbs">
  <a href="/deployments">Deployments</a>
  <span class="sep">/</span>
  <span>New</span>
</nav>

<header class="page-header">
  <div>
    <h1>New deployment</h1>
    <p class="subtitle">
      Using an existing name in the namespace rolls out a new version of that deployment.
    </p>
  </div>
  <div class="header-actions">
    <button class="btn-secondary" type="button" onclick={toggleJson}>
      {jsonMode ? 'Back to the form' : 'Edit as JSON'}
    </button>
  </div>
</header>

<form class="card panel" onsubmit={submit}>
  {#if jsonMode}
    <label class="field">
      <span>Request body</span>
      <textarea class="json" bind:value={json} spellcheck="false"></textarea>
      <span class="hint">
        Sent as is to <code>POST /deployments</code>; see the API reference for every field.
        Changes made here are not carried back to the form.
      </span>
    </label>
  {:else}
    <div class="form-grid">
      <label class="field">
        <span>Namespace</span>
        <input bind:value={form.namespace} required autocomplete="off" />
      </label>
      <label class="field">
        <span>Name</span>
        <input bind:value={form.name} required autocomplete="off" />
      </label>
      <label class="field wide">
        <span>Image</span>
        <input class="mono" bind:value={form.image} required placeholder="nginx:1.27" autocomplete="off" />
      </label>
      <label class="field">
        <span>Runtime</span>
        <select bind:value={form.runtime}>
          {#each RUNTIMES as runtime}
            <option value={runtime}>{runtime}</option>
          {/each}
        </select>
        <span class="hint">It must be enabled on the server.</span>
      </label>
      <label class="field">
        <span>Kind</span>
        <select bind:value={form.kind}>
          <option value="worker">worker (kept running)</option>
          <option value="job">job (runs to completion)</option>
        </select>
      </label>
      <label class="field">
        <span>Replicas</span>
        <input type="number" min="0" bind:value={form.replicas} required />
      </label>
      <label class="field wide">
        <span>Command</span>
        <textarea class="command" bind:value={form.command} spellcheck="false"></textarea>
        <span class="hint">Optional. One argument per line; the image's default when empty.</span>
      </label>
    </div>

    <section class="rows">
      <h2>Environment</h2>
      {#each env as row, i}
        <div class="row">
          <input class="mono" bind:value={row.key} placeholder="NAME" aria-label="Variable name" />
          <input
            class="mono"
            bind:value={row.value}
            placeholder={row.secret ? 'secret name' : 'value'}
            aria-label="Variable value"
          />
          <label class="inline"><input type="checkbox" bind:checked={row.secret} /> from a secret</label>
          <button class="btn-secondary" type="button" onclick={() => env.splice(i, 1)}>Remove</button>
        </div>
      {/each}
      <button
        class="btn-secondary"
        type="button"
        onclick={() => env.push({ key: '', value: '', secret: false })}>Add a variable</button
      >
    </section>

    <section class="rows">
      <h2>Ports</h2>
      {#each ports as row, i}
        <div class="row">
          <input type="number" min="1" max="65535" bind:value={row.published} placeholder="host port" aria-label="Published port" />
          <span class="muted">→</span>
          <input type="number" min="1" max="65535" bind:value={row.target} placeholder="container port" aria-label="Target port" />
          <select bind:value={row.protocol} aria-label="Protocol">
            <option value="tcp">tcp</option>
            <option value="udp">udp</option>
          </select>
          <button class="btn-secondary" type="button" onclick={() => ports.splice(i, 1)}>Remove</button>
        </div>
      {/each}
      <button
        class="btn-secondary"
        type="button"
        onclick={() => ports.push({ published: null, target: null, protocol: 'tcp' })}>Add a port</button
      >
    </section>
  {/if}

  {#if error}
    <p class="error">{error}</p>
  {/if}

  <div class="buttons">
    <a class="btn-secondary" href="/deployments">Cancel</a>
    <button class="btn-primary" type="submit" disabled={pending}>
      {pending ? 'Deploying…' : 'Deploy'}
    </button>
  </div>
</form>

<style>
  .breadcrumbs {
    display: flex;
    gap: 0.5rem;
    margin-bottom: 0.75rem;
    font-size: 0.8rem;
    color: var(--fg-2);
  }
  .breadcrumbs .sep {
    color: var(--fg-3);
  }
  .panel {
    padding: 1.25rem;
  }
  .json {
    min-height: 26rem;
  }
  .command {
    min-height: 4.5rem;
  }
  .rows {
    margin-top: 1.25rem;
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.5rem;
  }
  h2 {
    margin: 0 0 0.25rem;
    font-size: 0.9rem;
    font-weight: 600;
  }
  .row {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .row input,
  .row select {
    padding: 0.45rem 0.6rem;
    background: var(--bg-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--fg-0);
    font-size: 0.8rem;
  }
  .row input[type='number'] {
    width: 8rem;
  }
  .inline {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    font-size: 0.78rem;
    color: var(--fg-1);
  }
  .error {
    margin: 1rem 0 0;
    font-size: 0.8rem;
    color: var(--danger);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 1.25rem;
  }
</style>

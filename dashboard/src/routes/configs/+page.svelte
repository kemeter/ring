<script lang="ts">
  import ConfigFilesEditor from '#lib/ConfigFilesEditor.svelte';
  import ConfirmAction from '#lib/ConfirmAction.svelte';
  import CreatePanel from '#lib/CreatePanel.svelte';
  import ResourceTable from '#lib/ResourceTable.svelte';
  import { createConfig, deleteConfig, listConfigs, updateConfig, type Config } from '#lib/api.ts';
  import { formatDate } from '#lib/utils.ts';

  let expanded = $state<Set<string>>(new Set());
  let table: ResourceTable<Config> | undefined = $state();
  let creating = $state(false);
  let form = $state({ namespace: 'default', name: '', data: '', labels: '' });

  /** Drafts of the configs being edited, by id. */
  let drafts = $state<Record<string, { data: string; labels: string }>>({});
  let saveErrors = $state<Record<string, string>>({});
  let saving = $state<string | null>(null);

  async function create() {
    await createConfig({ ...form, labels: form.labels || undefined });
    form = { namespace: form.namespace, name: '', data: '', labels: '' };
    await table?.refresh();
  }

  function edit(c: Config) {
    drafts[c.id] = { data: c.data, labels: c.labels };
    expanded = new Set(expanded).add(c.id);
  }

  function cancelEdit(id: string) {
    delete drafts[id];
    delete saveErrors[id];
  }

  async function save(c: Config) {
    const draft = drafts[c.id];
    saving = c.id;
    delete saveErrors[c.id];
    try {
      await updateConfig(c.id, { name: c.name, data: draft.data, labels: draft.labels });
      delete drafts[c.id];
      await table?.refresh();
    } catch (e) {
      saveErrors[c.id] = e instanceof Error ? e.message : String(e);
    } finally {
      saving = null;
    }
  }

  async function remove(c: Config) {
    await deleteConfig(c.id);
    await table?.refresh();
  }

  const columns = [
    { label: '' },
    { label: 'Name' },
    { label: 'Namespace' },
    { label: 'Size', align: 'right' as const },
    { label: 'Labels' },
    { label: 'Created' },
    { label: '' }
  ];

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

  /** The entries of a config's JSON payload, or null when it is not one. */
  function filesOf(data: string): Array<[string, string]> | null {
    try {
      const parsed: unknown = JSON.parse(data);
      if (parsed && typeof parsed === 'object' && !Array.isArray(parsed)) {
        return Object.entries(parsed as Record<string, unknown>).map(([k, v]) => [
          k,
          typeof v === 'string' ? v : JSON.stringify(v, null, 2)
        ]);
      }
    } catch {
      // Not JSON: shown raw.
    }
    return null;
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
</script>

<ResourceTable
  bind:this={table}
  title="Configs"
  subtitle="Free-form configuration blobs mounted into deployments"
  {columns}
  load={listConfigs}
  key={(c: Config) => c.id}
  namespaceOf={(c: Config) => c.namespace}
>
  {#snippet actions()}
    <button class="btn-primary" type="button" onclick={() => (creating = true)}>New config</button>
  {/snippet}

  {#snippet toolbar()}
    <CreatePanel bind:open={creating} title="New config" submit={create}>
      <div class="form-grid">
        <label class="field">
          <span>Namespace</span>
          <input bind:value={form.namespace} required autocomplete="off" />
        </label>
        <label class="field">
          <span>Name</span>
          <input bind:value={form.name} required autocomplete="off" />
        </label>
        <div class="field wide">
          <ConfigFilesEditor bind:value={form.data} />
        </div>
        <label class="field wide">
          <span>Labels</span>
          <input bind:value={form.labels} autocomplete="off" />
          <span class="hint">Optional, e.g. <code>tier=frontend</code>.</span>
        </label>
      </div>
    </CreatePanel>
  {/snippet}

  {#snippet row(c: Config)}
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
      <td class="actions">
        {#if !drafts[c.id]}
          <button class="btn-secondary" type="button" onclick={() => edit(c)}>Edit</button>
        {/if}
        <ConfirmAction
          label="Delete"
          prompt={`Delete ${c.namespace}/${c.name}?`}
          pendingLabel="Deleting…"
          action={() => remove(c)}
        />
      </td>
    </tr>
    {#if open || drafts[c.id]}
      <tr class="data-row" id={`config-${c.id}-data`}>
        <td></td>
        <td colspan="6">
          {#if drafts[c.id]}
            <div class="editor">
              <ConfigFilesEditor bind:value={drafts[c.id].data} />
              <label class="field">
                <span>Labels</span>
                <input bind:value={drafts[c.id].labels} autocomplete="off" />
              </label>
              {#if saveErrors[c.id]}
                <p class="save-error">{saveErrors[c.id]}</p>
              {/if}
              <div class="editor-buttons">
                <span class="muted"
                  >Mounting deployments react per their <code>on_change</code>: none, live or rollout.</span
                >
                <button
                  class="btn-secondary"
                  type="button"
                  onclick={() => cancelEdit(c.id)}
                  disabled={saving === c.id}>Cancel</button
                >
                <button
                  class="btn-primary"
                  type="button"
                  onclick={() => save(c)}
                  disabled={saving === c.id}
                >
                  {saving === c.id ? 'Saving…' : 'Save'}
                </button>
              </div>
            </div>
          {:else}
            {@const files = filesOf(c.data)}
            {#if files}
              {#each files as [key, content]}
                <div class="file-key mono">{key}</div>
                <pre class="data">{content}</pre>
              {/each}
            {:else}
              <pre class="data">{c.data}</pre>
            {/if}
          {/if}
        </td>
      </tr>
    {/if}
  {/snippet}

  {#snippet empty()}
    <p>No configs yet.</p>
    <p class="muted">
      Create one with <strong>New config</strong>, then mount it into deployments via the
      <code>config:</code> field in your manifest.
    </p>
  {/snippet}
</ResourceTable>

<style>
  td.num {
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
  .editor {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    padding: 0.85rem 1rem;
  }
  .file-key {
    padding: 0.6rem 1rem 0;
    font-size: 0.75rem;
    color: var(--accent);
  }
  .editor-buttons {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 0.5rem;
    font-size: 0.75rem;
  }
  .editor-buttons .muted {
    margin-right: auto;
  }
  .save-error {
    margin: 0;
    font-size: 0.8rem;
    color: var(--danger);
  }
  td.actions .btn-secondary {
    margin-right: 0.4rem;
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

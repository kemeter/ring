<script lang="ts">
  import CreatePanel from '#lib/CreatePanel.svelte';
  import GuardedDelete from '#lib/GuardedDelete.svelte';
  import ResourceTable from '#lib/ResourceTable.svelte';
  import { createVolume, deleteVolume, listVolumes, type Volume } from '#lib/api.ts';
  import { formatBytes, formatDate } from '#lib/utils.ts';

  let table: ResourceTable<Volume> | undefined = $state();
  let creating = $state(false);
  let form = $state({
    namespace: 'default',
    name: '',
    backend_type: 'local' as 'local' | 'directory',
    sizeMib: null as number | null
  });

  async function create() {
    await createVolume({
      namespace: form.namespace,
      name: form.name,
      backend_type: form.backend_type,
      size: form.sizeMib ? Math.round(form.sizeMib * 1024 * 1024) : undefined
    });
    form = { ...form, name: '', sizeMib: null };
    await table?.refresh();
  }

  async function remove(v: Volume, force: boolean) {
    await deleteVolume(v.id, force);
    await table?.refresh();
  }

  const columns = [
    { label: 'Name' },
    { label: 'Namespace' },
    { label: 'Size', align: 'right' as const },
    { label: 'Backend' },
    { label: 'Host path' },
    { label: 'Labels' },
    { label: 'Created' },
    { label: '' }
  ];

  /** `key=value` pairs, matching how the configs page renders its labels. */
  function labelText(labels: Record<string, string>): string {
    const pairs = Object.entries(labels);
    if (pairs.length === 0) {
      return '—';
    }
    return pairs.map(([k, v]) => `${k}=${v}`).join(', ');
  }
</script>

<ResourceTable
  bind:this={table}
  title="Volumes"
  subtitle="Persistent storage attached to deployments, scoped per namespace"
  {columns}
  load={() => listVolumes()}
  key={(v: Volume) => v.id}
  namespaceOf={(v: Volume) => v.namespace}
>
  {#snippet actions()}
    <button class="btn-primary" type="button" onclick={() => (creating = true)}>New volume</button>
  {/snippet}

  {#snippet toolbar()}
    <CreatePanel bind:open={creating} title="New volume" submit={create}>
      <div class="form-grid">
        <label class="field">
          <span>Namespace</span>
          <input bind:value={form.namespace} required autocomplete="off" />
        </label>
        <label class="field">
          <span>Name</span>
          <input bind:value={form.name} required autocomplete="off" />
        </label>
        <label class="field">
          <span>Backend</span>
          <select bind:value={form.backend_type}>
            <option value="local">local (Docker volume)</option>
            <option value="directory">directory (host directory)</option>
          </select>
        </label>
        <label class="field">
          <span>Size (MiB)</span>
          <input type="number" min="1" bind:value={form.sizeMib} />
          <span class="hint">Optional.</span>
        </label>
      </div>
    </CreatePanel>
  {/snippet}

  {#snippet row(v: Volume)}
    <tr>
      <td class="mono">{v.name}</td>
      <td>
        <a class="ns-link" href="/volumes?namespace={v.namespace}">{v.namespace}</a>
      </td>
      <td class="num mono">{v.size === null ? '—' : formatBytes(v.size)}</td>
      <td><span class="status">{v.backend_type}</span></td>
      <td class="mono small path">{v.host_path}</td>
      <td class="mono small">{labelText(v.labels)}</td>
      <td class="muted">{formatDate(v.created_at)}</td>
      <td class="actions">
        <GuardedDelete
          label={`${v.namespace}/${v.name}`}
          consequence="lose the volume"
          remove={(force) => remove(v, force)}
        />
      </td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No volumes yet.</p>
    <p class="muted">
      Create one with <strong>New volume</strong>, then attach it to deployments via the
      <code>volumes:</code> field in your manifest.
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
  /* Host paths can be long; keep them from stretching the table. */
  td.path {
    max-width: 18rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ns-link {
    color: var(--fg-0);
    font-weight: 500;
  }
  .ns-link:hover {
    color: var(--accent);
  }
</style>

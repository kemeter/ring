<script lang="ts">
  import CreatePanel from '#lib/CreatePanel.svelte';
  import ResourceTable from '#lib/ResourceTable.svelte';
  import GuardedDelete from '#lib/GuardedDelete.svelte';
  import { createSecret, deleteSecret, listSecrets, type Secret } from '#lib/api.ts';
  import { formatDate } from '#lib/utils.ts';

  const columns = [
    { label: 'Name' },
    { label: 'Namespace' },
    { label: 'Created' },
    { label: 'Updated' },
    { label: '' }
  ];

  let table: ResourceTable<Secret> | undefined = $state();
  let creating = $state(false);
  let form = $state({ namespace: 'default', name: '', value: '' });
  async function create() {
    await createSecret(form);
    form = { namespace: form.namespace, name: '', value: '' };
    await table?.refresh();
  }

  async function remove(secret: Secret, force: boolean) {
    await deleteSecret(secret.id, force);
    await table?.refresh();
  }
</script>

<ResourceTable
  bind:this={table}
  title="Secrets"
  subtitle="AES-256-GCM encrypted values, scoped per namespace"
  {columns}
  load={listSecrets}
  key={(s: Secret) => s.id}
  namespaceOf={(s: Secret) => s.namespace}
>
  {#snippet actions()}
    <button class="btn-primary" type="button" onclick={() => (creating = true)}>New secret</button>
  {/snippet}

  {#snippet toolbar()}
    <CreatePanel bind:open={creating} title="New secret" submit={create}>
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
          <span>Value</span>
          <textarea bind:value={form.value} required spellcheck="false"></textarea>
          <span class="hint">Encrypted before being stored; the API never returns it.</span>
        </label>
      </div>
    </CreatePanel>
  {/snippet}

  {#snippet row(s: Secret)}
    <tr>
      <td class="mono">{s.name}</td>
      <td>
        <a class="ns-link" href="/secrets?namespace={s.namespace}">{s.namespace}</a>
      </td>
      <td class="muted">{formatDate(s.created_at)}</td>
      <td class="muted">{s.updated_at ? formatDate(s.updated_at) : '—'}</td>
      <td class="actions">
        <GuardedDelete
          label={`${s.namespace}/${s.name}`}
          consequence="will fail to start"
          remove={(force) => remove(s, force)}
        />
      </td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No secrets yet.</p>
    <p class="muted">
      Create one with <strong>New secret</strong>, or with
      <code>ring secret create &lt;name&gt; --namespace &lt;ns&gt; --value &lt;value&gt;</code>.
      The value is encrypted before being stored and is never returned by the API.
    </p>
  {/snippet}
</ResourceTable>

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

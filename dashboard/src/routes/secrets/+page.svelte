<script lang="ts">
  import ResourceTable from '$lib/ResourceTable.svelte';
  import { listSecrets, type Secret } from '$lib/api';
  import { formatDate } from '$lib/utils';

  const columns = [
    { label: 'Name' },
    { label: 'Namespace' },
    { label: 'Created' },
    { label: 'Updated' }
  ];
</script>

<ResourceTable
  title="Secrets"
  subtitle="AES-256-GCM encrypted values, scoped per namespace"
  {columns}
  load={listSecrets}
  key={(s: Secret) => s.id}
  namespaceOf={(s: Secret) => s.namespace}
>
  {#snippet row(s: Secret)}
    <tr>
      <td class="mono">{s.name}</td>
      <td>
        <a class="ns-link" href="/secrets?namespace={s.namespace}">{s.namespace}</a>
      </td>
      <td class="muted">{formatDate(s.created_at)}</td>
      <td class="muted">{s.updated_at ? formatDate(s.updated_at) : '—'}</td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No secrets yet.</p>
    <p class="muted">
      Create one with <code>ring secret create &lt;name&gt; --namespace &lt;ns&gt; --value
      &lt;value&gt;</code>. The value is encrypted before being stored and is never returned by the
      API.
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

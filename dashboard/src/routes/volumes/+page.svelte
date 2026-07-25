<script lang="ts">
  import ResourceTable from '$lib/ResourceTable.svelte';
  import { listVolumes, type Volume } from '$lib/api';
  import { formatBytes, formatDate } from '$lib/utils';

  const columns = [
    { label: 'Name' },
    { label: 'Namespace' },
    { label: 'Size', align: 'right' as const },
    { label: 'Backend' },
    { label: 'Host path' },
    { label: 'Labels' },
    { label: 'Created' }
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
  title="Volumes"
  subtitle="Persistent storage attached to deployments, scoped per namespace"
  {columns}
  load={() => listVolumes()}
  key={(v: Volume) => v.id}
  namespaceOf={(v: Volume) => v.namespace}
>
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
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No volumes yet.</p>
    <p class="muted">
      Volumes are created over the API (<code>POST /volumes</code>) and attached to deployments via
      the <code>volumes:</code> field in your manifest.
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

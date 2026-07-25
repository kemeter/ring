<script lang="ts">
  import ResourceTable from '$lib/ResourceTable.svelte';
  import { listWebhooks, type Webhook } from '$lib/api';
  import { formatDate } from '$lib/utils';

  const columns = [
    { label: 'URL' },
    { label: 'Events' },
    { label: 'Status' },
    { label: 'Created' }
  ];
</script>

<ResourceTable
  title="Webhooks"
  subtitle="HTTP endpoints notified when deployment events occur"
  {columns}
  load={listWebhooks}
  key={(w: Webhook) => w.id}
>
  {#snippet row(w: Webhook)}
    <tr class:revoked={w.revoked_at !== null}>
      <td class="mono url">{w.url}</td>
      <td>
        {#if w.events.length === 0}
          <span class="muted">all events</span>
        {:else}
          <span class="events">
            {#each w.events as ev}
              <span class="status">{ev}</span>
            {/each}
          </span>
        {/if}
      </td>
      <td>
        {#if w.revoked_at}
          <span class="status revoked-badge" title={`Revoked ${formatDate(w.revoked_at)}`}>
            revoked
          </span>
        {:else}
          <span class="status active-badge">active</span>
        {/if}
      </td>
      <td class="muted">{formatDate(w.created_at)}</td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No webhooks yet.</p>
    <p class="muted">
      Webhooks are created over the API (<code>POST /webhooks</code>). Each delivery is signed with
      an HMAC secret that is shown once at creation and never returned again.
    </p>
  {/snippet}
</ResourceTable>

<style>
  td.mono {
    font-family: var(--font-mono);
  }
  /* URLs can be long; keep them from stretching the table. */
  td.url {
    max-width: 26rem;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 0.82rem;
  }
  .events {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 0.25rem;
  }
  .active-badge {
    background: var(--success-bg);
    color: var(--success);
  }
  .revoked-badge {
    background: var(--bg-3);
    color: var(--fg-3);
  }
  /* A revoked webhook is inert — de-emphasise the whole row. */
  tr.revoked td.url {
    text-decoration: line-through;
    color: var(--fg-3);
  }
</style>

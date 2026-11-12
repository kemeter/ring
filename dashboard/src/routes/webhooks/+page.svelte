<script lang="ts">
  import ConfirmAction from '#lib/ConfirmAction.svelte';
  import CreatePanel from '#lib/CreatePanel.svelte';
  import OneTimeValue from '#lib/OneTimeValue.svelte';
  import ResourceTable from '#lib/ResourceTable.svelte';
  import {
    WEBHOOK_EVENTS,
    createWebhook,
    deleteWebhook,
    listWebhooks,
    type Webhook
  } from '#lib/api.ts';
  import { formatDate } from '#lib/utils.ts';

  const columns = [
    { label: 'URL' },
    { label: 'Events' },
    { label: 'Status' },
    { label: 'Created' },
    { label: '' }
  ];

  let table: ResourceTable<Webhook> | undefined = $state();
  let creating = $state(false);
  let form = $state({ url: '', events: [] as string[], secret: '' });
  /** The signing secret of the webhook just created, shown once. */
  let revealed = $state<string | null>(null);

  async function create() {
    const created = await createWebhook({
      url: form.url,
      events: form.events,
      secret: form.secret || undefined
    });
    revealed = created.secret;
    form = { url: '', events: [], secret: '' };
    await table?.refresh();
  }

  async function revoke(w: Webhook) {
    await deleteWebhook(w.id);
    await table?.refresh();
  }
</script>

<ResourceTable
  bind:this={table}
  title="Webhooks"
  subtitle="HTTP endpoints notified when deployment events occur"
  {columns}
  load={listWebhooks}
  key={(w: Webhook) => w.id}
>
  {#snippet actions()}
    <button class="btn-primary" type="button" onclick={() => (creating = true)}>New webhook</button>
  {/snippet}

  {#snippet toolbar()}
    {#if revealed}
      <OneTimeValue title="Signing secret" value={revealed} dismiss={() => (revealed = null)} />
    {/if}
    <CreatePanel bind:open={creating} title="New webhook" submit={create}>
      <div class="form-grid">
        <label class="field wide">
          <span>URL</span>
          <input type="url" bind:value={form.url} required placeholder="https://hooks.example.com/ring" />
        </label>
        <div class="field wide">
          <span>Events</span>
          <div class="checks">
            {#each WEBHOOK_EVENTS as ev}
              <label><input type="checkbox" value={ev} bind:group={form.events} /> {ev}</label>
            {/each}
          </div>
          <span class="hint">None checked subscribes to every event.</span>
        </div>
        <label class="field wide">
          <span>Signing secret</span>
          <input bind:value={form.secret} autocomplete="off" />
          <span class="hint">Optional. Ring generates one when left empty.</span>
        </label>
      </div>
    </CreatePanel>
  {/snippet}

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
      <td class="actions">
        {#if !w.revoked_at}
          <ConfirmAction
            label="Revoke"
            prompt="Stop deliveries to this URL?"
            pendingLabel="Revoking…"
            action={() => revoke(w)}
          />
        {/if}
      </td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No webhooks yet.</p>
    <p class="muted">
      Create one with <strong>New webhook</strong>. Each delivery is signed with an HMAC secret that
      is shown once at creation and never returned again.
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

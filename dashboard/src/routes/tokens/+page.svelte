<script lang="ts">
  import ResourceTable from '$lib/ResourceTable.svelte';
  import { listTokens, type Token } from '$lib/api';
  import { formatDate } from '$lib/utils';

  const columns = [
    { label: 'Name' },
    { label: 'Prefix' },
    { label: 'Scopes' },
    { label: 'Namespaces' },
    { label: 'Status' },
    { label: 'Last used' },
    { label: 'Expires' }
  ];

  type State = 'active' | 'revoked' | 'expired';

  /** Mirrors `Token::is_active()` on the server: revoked wins over expired, and
   *  an unparseable expiry is treated as expired (fail closed) rather than
   *  shown as active. */
  function stateOf(t: Token): State {
    if (t.revoked_at) {
      return 'revoked';
    }
    if (!t.expire_at) {
      return 'active';
    }
    const exp = new Date(t.expire_at).getTime();
    if (Number.isNaN(exp)) {
      return 'expired';
    }
    return exp <= Date.now() ? 'expired' : 'active';
  }
</script>

<ResourceTable
  title="Tokens"
  subtitle="Your personal access tokens — only yours are ever listed"
  {columns}
  load={listTokens}
  key={(t: Token) => t.id}
>
  {#snippet row(t: Token)}
    {@const state = stateOf(t)}
    <tr class:inert={state !== 'active'}>
      <td>{t.name}</td>
      <td class="mono small">{t.token_prefix}…</td>
      <td>
        {#if t.scopes.length === 0}
          <span class="muted">none</span>
        {:else}
          <span class="chips">
            {#each t.scopes as scope}
              <span class="status">{scope}</span>
            {/each}
          </span>
        {/if}
      </td>
      <td>
        {#if t.namespaces.length === 0}
          <span class="muted">all</span>
        {:else}
          <span class="chips">
            {#each t.namespaces as ns}
              <span class="status">{ns}</span>
            {/each}
          </span>
        {/if}
      </td>
      <td>
        {#if state === 'revoked'}
          <span class="status revoked-badge" title={`Revoked ${formatDate(t.revoked_at)}`}>
            revoked
          </span>
        {:else if state === 'expired'}
          <span class="status expired-badge">expired</span>
        {:else}
          <span class="status active-badge">active</span>
        {/if}
      </td>
      <td class="muted">{t.last_used_at ? formatDate(t.last_used_at) : 'never'}</td>
      <td class="muted">{t.expire_at ? formatDate(t.expire_at) : '—'}</td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No tokens yet.</p>
    <p class="muted">
      Create one with <code>ring token create &lt;name&gt;</code>. The token value is shown once at
      creation and never returned again.
    </p>
  {/snippet}
</ResourceTable>

<style>
  td.mono {
    font-family: var(--font-mono);
  }
  td.small {
    font-size: 0.78rem;
  }
  .chips {
    display: inline-flex;
    flex-wrap: wrap;
    gap: 0.25rem;
  }
  .active-badge {
    background: var(--success-bg);
    color: var(--success);
  }
  .expired-badge {
    background: var(--warning-bg);
    color: var(--warning);
  }
  .revoked-badge {
    background: var(--bg-3);
    color: var(--fg-3);
  }
  /* A revoked or expired token can't authenticate — de-emphasise the row. */
  tr.inert td:first-child {
    color: var(--fg-3);
  }
</style>

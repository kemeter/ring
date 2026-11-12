<script lang="ts">
  import ConfirmAction from '#lib/ConfirmAction.svelte';
  import CreatePanel from '#lib/CreatePanel.svelte';
  import OneTimeValue from '#lib/OneTimeValue.svelte';
  import ResourceTable from '#lib/ResourceTable.svelte';
  import {
    TOKEN_SCOPES,
    createToken,
    listTokens,
    revokeToken,
    rotateToken,
    type Token,
    type TokenCreated
  } from '#lib/api.ts';
  import { formatDate } from '#lib/utils.ts';

  let table: ResourceTable<Token> | undefined = $state();
  let creating = $state(false);
  let form = $state({
    name: '',
    scopes: ['deployments:read'] as string[],
    namespaces: '',
    expiresOn: ''
  });
  /** The clear value of the token just created or rotated, shown once. */
  let revealed = $state<TokenCreated | null>(null);

  async function create() {
    revealed = await createToken({
      name: form.name,
      scopes: form.scopes,
      namespaces: form.namespaces
        .split(',')
        .map((ns) => ns.trim())
        .filter(Boolean),
      // End of the chosen day, in UTC: the API takes RFC 3339.
      expire_at: form.expiresOn ? `${form.expiresOn}T23:59:59Z` : undefined
    });
    form = { name: '', scopes: ['deployments:read'], namespaces: '', expiresOn: '' };
    await table?.refresh();
  }

  async function rotate(t: Token) {
    revealed = await rotateToken(t.id);
    await table?.refresh();
  }

  async function revoke(t: Token) {
    await revokeToken(t.id);
    await table?.refresh();
  }

  const columns = [
    { label: 'Name' },
    { label: 'Prefix' },
    { label: 'Scopes' },
    { label: 'Namespaces' },
    { label: 'Status' },
    { label: 'Last used' },
    { label: 'Expires' },
    { label: '' }
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
  bind:this={table}
  title="Tokens"
  subtitle="Your personal access tokens — only yours are ever listed"
  {columns}
  load={listTokens}
  key={(t: Token) => t.id}
>
  {#snippet actions()}
    <button class="btn-primary" type="button" onclick={() => (creating = true)}>New token</button>
  {/snippet}

  {#snippet toolbar()}
    {#if revealed}
      <OneTimeValue
        title={`Token ${revealed.name}`}
        value={revealed.token}
        dismiss={() => (revealed = null)}
      />
    {/if}
    <CreatePanel bind:open={creating} title="New token" submit={create}>
      <div class="form-grid">
        <label class="field">
          <span>Name</span>
          <input bind:value={form.name} required autocomplete="off" />
        </label>
        <label class="field">
          <span>Expires on</span>
          <input type="date" bind:value={form.expiresOn} />
          <span class="hint">Optional. Never expires when empty.</span>
        </label>
        <div class="field wide">
          <span>Scopes</span>
          <div class="checks">
            {#each TOKEN_SCOPES as scope}
              <label><input type="checkbox" value={scope} bind:group={form.scopes} /> {scope}</label>
            {/each}
          </div>
        </div>
        <label class="field wide">
          <span>Namespaces</span>
          <input bind:value={form.namespaces} placeholder="staging, prod" autocomplete="off" />
          <span class="hint">Comma-separated. Every namespace when empty.</span>
        </label>
      </div>
    </CreatePanel>
  {/snippet}

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
      <td class="actions">
        {#if state === 'active'}
          <ConfirmAction
            label="Rotate"
            prompt="Issue a new value and revoke this one?"
            pendingLabel="Rotating…"
            action={() => rotate(t)}
          />
          <ConfirmAction
            label="Revoke"
            prompt={`Revoke ${t.name}?`}
            pendingLabel="Revoking…"
            action={() => revoke(t)}
          />
        {/if}
      </td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No tokens yet.</p>
    <p class="muted">
      Create one with <strong>New token</strong> or <code>ring token create &lt;name&gt;</code>.
      The token value is shown once at creation and never returned again.
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

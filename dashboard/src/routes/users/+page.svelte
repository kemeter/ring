<script lang="ts">
  import ConfirmAction from '#lib/ConfirmAction.svelte';
  import CreatePanel from '#lib/CreatePanel.svelte';
  import ResourceTable from '#lib/ResourceTable.svelte';
  import {
    USER_ROLES,
    createUser,
    deleteUser,
    listUsers,
    setUserRole,
    type User
  } from '#lib/api.ts';
  import { formatDate } from '#lib/utils.ts';

  type Role = (typeof USER_ROLES)[number];

  const columns = [
    { label: 'Username' },
    { label: 'Status' },
    { label: 'Last login' },
    { label: 'Created' },
    { label: '' }
  ];

  let table: ResourceTable<User> | undefined = $state();
  let creating = $state(false);
  let form = $state({ username: '', password: '' });
  /** Role picked in a row, waiting for confirmation. The list does not say
   *  which role a user has, so the picker starts empty. */
  let pickedRole = $state<Record<string, Role | ''>>({});
  /** Role applied in this session, shown since the list cannot say it. */
  let roleSet = $state<Record<string, Role>>({});

  async function create() {
    await createUser(form);
    form = { username: '', password: '' };
    await table?.refresh();
  }

  async function applyRole(u: User, role: Role) {
    await setUserRole(u.id, role);
    roleSet[u.id] = role;
    pickedRole[u.id] = '';
  }

  async function remove(u: User) {
    await deleteUser(u.id);
    await table?.refresh();
  }
</script>

<ResourceTable
  bind:this={table}
  title="Users"
  subtitle="Accounts that can authenticate against this Ring instance"
  {columns}
  load={listUsers}
  key={(u: User) => u.id}
>
  {#snippet actions()}
    <button class="btn-primary" type="button" onclick={() => (creating = true)}>New user</button>
  {/snippet}

  {#snippet toolbar()}
    <CreatePanel bind:open={creating} title="New user" submit={create}>
      <div class="form-grid">
        <label class="field">
          <span>Username</span>
          <input bind:value={form.username} required autocomplete="off" />
        </label>
        <label class="field">
          <span>Password</span>
          <input type="password" bind:value={form.password} required autocomplete="new-password" />
        </label>
      </div>
      <p class="muted note">New accounts start as <code>viewer</code>; change the role afterwards.</p>
    </CreatePanel>
  {/snippet}

  {#snippet row(u: User)}
    <tr>
      <td class="mono">{u.username}</td>
      <td>
        <span class="status" class:active-badge={u.status === 'active'}>{u.status}</span>
      </td>
      <td class="muted">{u.login_at ? formatDate(u.login_at) : 'never'}</td>
      <td class="muted">{formatDate(u.created_at)}</td>
      <td class="actions">
        {#if pickedRole[u.id]}
          {@const role = pickedRole[u.id] as Role}
          <ConfirmAction
            label={`Make ${role}`}
            prompt={`${u.username} becomes ${role}, and its sessions and tokens are revoked.`}
            pendingLabel="Saving…"
            action={() => applyRole(u, role)}
            startConfirming
            oncancel={() => (pickedRole[u.id] = '')}
          />
        {:else}
          {#if roleSet[u.id]}
            <span class="role-set">now {roleSet[u.id]}</span>
          {/if}
          <select class="role-picker" bind:value={pickedRole[u.id]} aria-label="Change role">
            <option value="">Change role…</option>
            {#each USER_ROLES as r}
              <option value={r}>{r}</option>
            {/each}
          </select>
          <ConfirmAction
            label="Delete"
            prompt={`Delete ${u.username}?`}
            pendingLabel="Deleting…"
            action={() => remove(u)}
          />
        {/if}
      </td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No users yet.</p>
    <p class="muted">
      Create one with <strong>New user</strong>. Listing them requires the <code>users:read</code>
      scope.
    </p>
  {/snippet}
</ResourceTable>

<style>
  td.mono {
    font-family: var(--font-mono);
  }
  .role-picker {
    margin-right: 0.4rem;
    background: var(--bg-2);
    color: var(--fg-1);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    padding: 0.45rem 0.5rem;
    font-size: 0.8rem;
  }
  .role-set {
    margin-right: 0.6rem;
    font-size: 0.75rem;
    color: var(--success);
  }
  .note {
    margin: 0.75rem 0 0;
    font-size: 0.78rem;
  }
  .active-badge {
    background: var(--success-bg);
    color: var(--success);
  }
</style>

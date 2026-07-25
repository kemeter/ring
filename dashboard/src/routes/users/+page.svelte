<script lang="ts">
  import ResourceTable from '$lib/ResourceTable.svelte';
  import { listUsers, type User } from '$lib/api';
  import { formatDate } from '$lib/utils';

  const columns = [
    { label: 'Username' },
    { label: 'Status' },
    { label: 'Last login' },
    { label: 'Created' }
  ];
</script>

<ResourceTable
  title="Users"
  subtitle="Accounts that can authenticate against this Ring instance"
  {columns}
  load={listUsers}
  key={(u: User) => u.id}
>
  {#snippet row(u: User)}
    <tr>
      <td class="mono">{u.username}</td>
      <td>
        <span class="status" class:active-badge={u.status === 'active'}>{u.status}</span>
      </td>
      <td class="muted">{u.login_at ? formatDate(u.login_at) : 'never'}</td>
      <td class="muted">{formatDate(u.created_at)}</td>
    </tr>
  {/snippet}

  {#snippet empty()}
    <p>No users yet.</p>
    <p class="muted">
      Users are created over the API (<code>POST /users</code>). Listing them requires the
      <code>users:read</code> scope.
    </p>
  {/snippet}
</ResourceTable>

<style>
  td.mono {
    font-family: var(--font-mono);
  }
  .active-badge {
    background: var(--success-bg);
    color: var(--success);
  }
</style>

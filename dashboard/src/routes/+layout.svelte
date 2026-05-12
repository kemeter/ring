<script lang="ts">
  import '../app.css';
  import { page } from '$app/stores';
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { getCurrentUser, type CurrentUser } from '$lib/api';
  import { clearToken, getToken } from '$lib/auth';

  let { children } = $props();
  let currentUser = $state<CurrentUser | null>(null);

  const nav = [
    { href: './deployments', label: 'Deployments', icon: 'grid' }
  ];

  function isActive(href: string): boolean {
    const current = $page.url.pathname.replace(/\/$/, '');
    const target = href.replace(/^\.\//, '/').replace(/\/$/, '');
    if (target === '') {
      return current === '';
    }
    return current === target || current.startsWith(`${target}/`);
  }

  let onLoginPage = $derived($page.url.pathname === '/' || $page.url.pathname === '');

  onMount(async () => {
    if (onLoginPage) {
      return;
    }
    if (!getToken()) {
      goto('./');
      return;
    }
    try {
      currentUser = await getCurrentUser();
    } catch {
      // Auth interceptor in api.ts handles 401 → redirect. For other
      // errors we just keep the user identity blank — the rest of the
      // dashboard still loads.
      currentUser = null;
    }
  });

  function logout() {
    clearToken();
    goto('./');
  }
</script>

{#if onLoginPage}
  {@render children()}
{:else}
  <div class="shell">
    <aside class="sidebar">
      <div class="brand">
        <div class="logo">r</div>
        <div class="brand-text">
          <div class="brand-name">ring</div>
          <div class="brand-sub">dashboard</div>
        </div>
      </div>

      <nav>
        {#each nav as item}
          <a href={item.href} class="nav-item" class:active={isActive(item.href)}>
            <span class="nav-icon">
              {#if item.icon === 'grid'}
                <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5">
                  <rect x="2" y="2" width="5" height="5" rx="1" />
                  <rect x="9" y="2" width="5" height="5" rx="1" />
                  <rect x="2" y="9" width="5" height="5" rx="1" />
                  <rect x="9" y="9" width="5" height="5" rx="1" />
                </svg>
              {/if}
            </span>
            <span>{item.label}</span>
          </a>
        {/each}
      </nav>

      <div class="sidebar-footer">
        {#if currentUser}
          <div class="user">
            <div class="user-name">{currentUser.username}</div>
            <div class="user-status">{currentUser.status}</div>
          </div>
        {/if}
        <button class="logout" onclick={logout}>Sign out</button>
        <a
          class="doc-link"
          href="https://ring.kemeter.io/documentation"
          target="_blank"
          rel="noopener noreferrer"
        >
          <svg
            viewBox="0 0 16 16"
            fill="none"
            stroke="currentColor"
            stroke-width="1.5"
            stroke-linecap="round"
            stroke-linejoin="round"
          >
            <path d="M2 3h4a2 2 0 0 1 2 2v9a2 2 0 0 0-2-2H2zM14 3h-4a2 2 0 0 0-2 2v9a2 2 0 0 1 2-2h4z" />
          </svg>
          <span>Documentation</span>
        </a>
      </div>
    </aside>

    <main class="main">
      {@render children()}
    </main>
  </div>
{/if}

<style>
  .shell {
    display: grid;
    grid-template-columns: 220px 1fr;
    min-height: 100vh;
  }

  .sidebar {
    background: var(--bg-1);
    border-right: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    padding: 1rem 0.75rem;
    position: sticky;
    top: 0;
    height: 100vh;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding: 0.25rem 0.5rem 1.25rem;
    margin-bottom: 0.5rem;
    border-bottom: 1px solid var(--border);
  }

  .logo {
    width: 32px;
    height: 32px;
    border-radius: 8px;
    background: linear-gradient(135deg, var(--accent), #0f766e);
    color: #042f25;
    display: grid;
    place-items: center;
    font-weight: 700;
    font-size: 1.05rem;
    box-shadow: 0 2px 8px rgba(110, 231, 183, 0.25);
  }

  .brand-name {
    font-weight: 600;
    font-size: 0.95rem;
    letter-spacing: -0.01em;
  }

  .brand-sub {
    font-size: 0.7rem;
    color: var(--fg-2);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  nav {
    display: flex;
    flex-direction: column;
    gap: 2px;
    margin-top: 0.5rem;
  }

  .nav-item {
    display: flex;
    align-items: center;
    gap: 0.625rem;
    padding: 0.5rem 0.625rem;
    border-radius: var(--radius);
    color: var(--fg-1);
    font-size: 0.825rem;
    font-weight: 500;
    transition:
      background 0.1s,
      color 0.1s;
  }
  .nav-item:hover {
    background: var(--bg-hover);
    color: var(--fg-0);
  }
  .nav-item.active {
    background: var(--accent-bg);
    color: var(--accent);
  }

  .nav-icon {
    display: grid;
    place-items: center;
    width: 16px;
    height: 16px;
  }
  .nav-icon :global(svg) {
    width: 16px;
    height: 16px;
  }

  .sidebar-footer {
    margin-top: auto;
    padding: 0.75rem 0.625rem 0.25rem;
    border-top: 1px solid var(--border);
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
  }

  .user {
    display: flex;
    flex-direction: column;
  }
  .user-name {
    font-size: 0.8rem;
    font-weight: 500;
    color: var(--fg-0);
  }
  .user-status {
    font-size: 0.7rem;
    color: var(--fg-2);
    text-transform: uppercase;
    letter-spacing: 0.06em;
  }

  .logout {
    background: var(--bg-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--fg-1);
    font-size: 0.75rem;
    padding: 0.4rem 0.625rem;
    text-align: left;
  }
  .logout:hover {
    background: var(--bg-hover);
    color: var(--fg-0);
  }

  .doc-link {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.625rem;
    border-radius: var(--radius);
    color: var(--fg-2);
    font-size: 0.75rem;
    font-weight: 500;
  }
  .doc-link:hover {
    background: var(--bg-hover);
    color: var(--fg-0);
  }
  .doc-link :global(svg) {
    width: 14px;
    height: 14px;
  }

  .main {
    padding: 2rem 2.5rem;
    max-width: 1400px;
    width: 100%;
  }
</style>

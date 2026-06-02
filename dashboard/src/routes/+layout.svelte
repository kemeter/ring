<script lang="ts">
  import '../app.css';
  import { page } from '$app/stores';
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { getCurrentUser, type CurrentUser } from '$lib/api';
  import { clearToken, getToken } from '$lib/auth';
  import { applyTheme, loadTheme, saveTheme, type Theme } from '$lib/theme';

  let { children } = $props();
  let currentUser = $state<CurrentUser | null>(null);
  let theme = $state<Theme>('dark');

  function toggleTheme() {
    theme = theme === 'dark' ? 'light' : 'dark';
    applyTheme(theme);
    saveTheme(theme);
  }

  const nav = [
    { href: '/deployments', label: 'Deployments', icon: 'grid' },
    { href: '/namespaces', label: 'Namespaces', icon: 'folder' },
    { href: '/secrets', label: 'Secrets', icon: 'key' },
    { href: '/configs', label: 'Configs', icon: 'file' }
  ];

  function isActive(href: string): boolean {
    const current = $page.url.pathname.replace(/\/$/, '');
    const target = href.replace(/\/$/, '');
    return current === target || current.startsWith(`${target}/`);
  }

  let onLoginPage = $derived($page.url.pathname === '/');

  onMount(async () => {
    // Apply the persisted (or system-preferred) theme as early as possible so
    // the dashboard doesn't briefly flash the opposite palette.
    theme = loadTheme();
    applyTheme(theme);

    if (onLoginPage) {
      return;
    }
    if (!getToken()) {
      goto('/');
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
    goto('/');
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
              {:else if item.icon === 'folder'}
                <svg
                  viewBox="0 0 16 16"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                >
                  <path d="M1.5 4.5a1 1 0 0 1 1-1H6l1.5 1.5h6a1 1 0 0 1 1 1V12a1 1 0 0 1-1 1h-11a1 1 0 0 1-1-1V4.5z" />
                </svg>
              {:else if item.icon === 'key'}
                <svg
                  viewBox="0 0 16 16"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                >
                  <circle cx="6" cy="10" r="3" />
                  <path d="M8.1 8.1L14 2.2M11 5l1.5 1.5" />
                </svg>
              {:else if item.icon === 'file'}
                <svg
                  viewBox="0 0 16 16"
                  fill="none"
                  stroke="currentColor"
                  stroke-width="1.5"
                  stroke-linecap="round"
                  stroke-linejoin="round"
                >
                  <path d="M3 1.5h6L13 5.5V14a.5.5 0 0 1-.5.5h-9A.5.5 0 0 1 3 14V1.5z" />
                  <path d="M9 1.5V5.5h4" />
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
        <button
          class="theme-toggle"
          onclick={toggleTheme}
          aria-label={theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode'}
          title={theme === 'dark' ? 'Switch to light mode' : 'Switch to dark mode'}
        >
          {#if theme === 'dark'}
            <!-- Sun: shown in dark mode → click to go light -->
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <circle cx="8" cy="8" r="3" />
              <path d="M8 1v2M8 13v2M1 8h2M13 8h2M3.05 3.05l1.4 1.4M11.55 11.55l1.4 1.4M3.05 12.95l1.4-1.4M11.55 4.45l1.4-1.4" />
            </svg>
            <span>Light mode</span>
          {:else}
            <!-- Moon: shown in light mode → click to go dark -->
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
              <path d="M13.5 9.2A5.5 5.5 0 0 1 6.8 2.5 6 6 0 1 0 13.5 9.2z" />
            </svg>
            <span>Dark mode</span>
          {/if}
        </button>
        <div class="version mono">v{__RING_VERSION__}</div>
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

  .version {
    margin-left: auto;
    align-self: flex-start;
    font-size: 0.65rem;
    color: var(--fg-3);
    background: var(--bg-2);
    border: 1px solid var(--border);
    border-radius: var(--radius-sm);
    padding: 0.1rem 0.35rem;
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

  .theme-toggle {
    appearance: none;
    background: transparent;
    border: 0;
    display: flex;
    align-items: center;
    gap: 0.5rem;
    padding: 0.4rem 0.625rem;
    border-radius: var(--radius);
    color: var(--fg-2);
    font-size: 0.75rem;
    font-weight: 500;
    font-family: inherit;
    cursor: pointer;
    text-align: left;
  }
  .theme-toggle:hover {
    background: var(--bg-hover);
    color: var(--fg-0);
  }
  .theme-toggle :global(svg) {
    width: 14px;
    height: 14px;
  }

  .version {
    color: var(--fg-3);
    font-size: 0.7rem;
    padding: 0.25rem 0.625rem 0;
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

<script lang="ts">
  import { onMount } from 'svelte';
  import { goto } from '$app/navigation';
  import { login } from '$lib/api';
  import { getToken, setToken } from '$lib/auth';

  let username = $state('admin');
  let password = $state('');
  let error = $state('');
  let pending = $state(false);

  onMount(() => {
    if (getToken()) {
      goto('/');
    }
  });

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    error = '';
    pending = true;
    try {
      const token = await login(username, password);
      setToken(token);
      goto('/');
    } catch (err) {
      error = err instanceof Error ? err.message : String(err);
    } finally {
      pending = false;
    }
  }
</script>

<div class="login-shell">
  <form class="card" onsubmit={submit}>
    <div class="brand">
      <div class="logo">r</div>
      <div class="brand-text">
        <div class="brand-name">ring</div>
        <div class="brand-sub">dashboard</div>
      </div>
    </div>

    <h1>Sign in</h1>

    <div class="field">
      <label for="name">Username</label>
      <input id="name" type="text" autocomplete="username" bind:value={username} required />
    </div>

    <div class="field">
      <label for="password">Password</label>
      <input
        id="password"
        type="password"
        autocomplete="current-password"
        bind:value={password}
        required
      />
    </div>

    {#if error}
      <p class="error">{error}</p>
    {/if}

    <button class="btn-primary" type="submit" disabled={pending}>
      {pending ? 'Signing in…' : 'Sign in'}
    </button>
  </form>
</div>

<style>
  .login-shell {
    min-height: 100vh;
    display: grid;
    place-items: center;
    background: var(--bg-0);
    padding: 1rem;
  }

  .card {
    background: var(--bg-1);
    border: 1px solid var(--border);
    border-radius: var(--radius-lg);
    padding: 2rem 2.25rem;
    width: 100%;
    max-width: 380px;
  }

  .brand {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    padding-bottom: 1.25rem;
    margin-bottom: 1.25rem;
    border-bottom: 1px solid var(--border);
  }

  .logo {
    width: 36px;
    height: 36px;
    border-radius: 8px;
    background: linear-gradient(135deg, var(--accent), #0f766e);
    color: white;
    display: grid;
    place-items: center;
    font-weight: 700;
    font-size: 1.1rem;
    box-shadow: 0 2px 8px rgba(110, 231, 183, 0.25);
  }

  .brand-name {
    font-weight: 600;
    font-size: 1rem;
    letter-spacing: -0.01em;
  }

  .brand-sub {
    font-size: 0.7rem;
    color: var(--fg-2);
    text-transform: uppercase;
    letter-spacing: 0.08em;
  }

  h1 {
    margin: 0 0 1.25rem;
    font-size: 1.15rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }

  .field {
    margin-bottom: 1rem;
  }

  label {
    display: block;
    font-size: 0.78rem;
    color: var(--fg-1);
    margin-bottom: 0.4rem;
    font-weight: 500;
  }

  input {
    width: 100%;
    padding: 0.55rem 0.75rem;
    background: var(--bg-2);
    border: 1px solid var(--border);
    border-radius: var(--radius);
    color: var(--fg-0);
    outline: none;
    transition: border-color 0.1s;
  }

  input:focus {
    border-color: var(--accent);
  }

  input::placeholder {
    color: var(--fg-3);
  }

  .error {
    margin: 0 0 0.75rem;
    padding: 0.5rem 0.75rem;
    background: var(--danger-bg);
    border: 1px solid rgba(248, 81, 73, 0.3);
    border-radius: var(--radius);
    color: var(--danger);
    font-size: 0.78rem;
  }

  .btn-primary {
    width: 100%;
    border: none;
    border-radius: var(--radius);
    padding: 0.6rem 1rem;
    font-size: 0.85rem;
    font-weight: 500;
    background: var(--accent);
    color: #042f25;
    margin-top: 0.5rem;
    transition: background 0.1s;
  }

  .btn-primary:hover:not(:disabled) {
    background: var(--accent-hover);
  }

  .btn-primary:disabled {
    opacity: 0.6;
    cursor: not-allowed;
  }
</style>

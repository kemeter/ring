<script lang="ts">
  /**
   * The create form of a list page, shown above the table while `open`.
   * `submit` is awaited: when it throws, its message stays on the form so the
   * user can fix the input; when it succeeds the panel closes.
   */
  import type { Snippet } from 'svelte';

  interface Props {
    open: boolean;
    title: string;
    submitLabel?: string;
    submit: () => Promise<void>;
    children: Snippet;
  }

  let { open = $bindable(), title, submitLabel = 'Create', submit, children }: Props = $props();

  let pending = $state(false);
  let error = $state<string | null>(null);

  async function onsubmit(event: SubmitEvent) {
    event.preventDefault();
    pending = true;
    error = null;
    try {
      await submit();
      open = false;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      pending = false;
    }
  }

  function cancel() {
    open = false;
    error = null;
  }
</script>

{#if open}
  <form class="card panel" {onsubmit}>
    <h2>{title}</h2>
    {@render children()}
    {#if error}
      <p class="error">{error}</p>
    {/if}
    <div class="buttons">
      <button class="btn-secondary" type="button" onclick={cancel} disabled={pending}>Cancel</button>
      <button class="btn-primary" type="submit" disabled={pending}>
        {pending ? 'Saving…' : submitLabel}
      </button>
    </div>
  </form>
{/if}

<style>
  .panel {
    padding: 1.1rem 1.125rem;
    margin-bottom: 1.25rem;
  }
  h2 {
    margin: 0 0 1rem;
    font-size: 0.95rem;
    font-weight: 600;
  }
  .error {
    margin: 0.85rem 0 0;
    font-size: 0.8rem;
    color: var(--danger);
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 1rem;
  }
</style>

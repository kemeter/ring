<script lang="ts">
  /** A destructive action that asks for confirmation inline, in place of the
   *  button, instead of a browser `confirm()` dialog. `action` is awaited;
   *  when it throws, its message is shown next to the buttons and the
   *  confirmation stays open so the user can retry or cancel. */
  let {
    label,
    prompt,
    confirmLabel = label,
    pendingLabel = 'Working…',
    action,
    startConfirming = false,
    oncancel
  }: {
    label: string;
    prompt: string;
    confirmLabel?: string;
    pendingLabel?: string;
    action: () => Promise<void>;
    /** Open on the confirmation, for an action the user already chose
     *  elsewhere (a role picked in a select). */
    startConfirming?: boolean;
    oncancel?: () => void;
  } = $props();

  // svelte-ignore state_referenced_locally
  let confirming = $state(startConfirming);
  let pending = $state(false);
  let error = $state<string | null>(null);

  async function run() {
    pending = true;
    error = null;
    try {
      await action();
      confirming = false;
    } catch (e) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      pending = false;
    }
  }

  function cancel() {
    confirming = false;
    error = null;
    oncancel?.();
  }
</script>

{#if confirming}
  <div class="confirm" role="group" aria-label={prompt}>
    <span class="prompt">{prompt}</span>
    {#if error}
      <span class="error">{error}</span>
    {/if}
    <button class="btn-secondary" type="button" onclick={cancel} disabled={pending}>Cancel</button>
    <button class="btn-danger" type="button" onclick={run} disabled={pending}>
      {pending ? pendingLabel : confirmLabel}
    </button>
  </div>
{:else}
  <button class="btn-danger" type="button" onclick={() => (confirming = true)}>{label}</button>
{/if}

<style>
  .confirm {
    display: inline-flex;
    align-items: center;
    white-space: nowrap;
    gap: 0.5rem;
  }
  .prompt {
    font-size: 0.8rem;
    color: var(--fg-1);
  }
  .error {
    font-size: 0.75rem;
    color: var(--danger);
  }
</style>

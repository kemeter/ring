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
    action
  }: {
    label: string;
    prompt: string;
    confirmLabel?: string;
    pendingLabel?: string;
    action: () => Promise<void>;
  } = $props();

  let confirming = $state(false);
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
    display: flex;
    align-items: center;
    flex-wrap: wrap;
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

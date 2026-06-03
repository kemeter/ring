<script lang="ts">
  /** A small inline button that copies `value` to the clipboard and briefly
   *  shows a checkmark. Used next to IDs and other copy-worthy mono values. */
  let { value, label = 'Copy' }: { value: string; label?: string } = $props();

  let copied = $state(false);
  let timer: ReturnType<typeof setTimeout> | null = null;

  async function copy() {
    try {
      await navigator.clipboard.writeText(value);
      copied = true;
      if (timer) clearTimeout(timer);
      timer = setTimeout(() => {
        copied = false;
      }, 1200);
    } catch {
      // Clipboard can be blocked (insecure context, permissions). Fail quietly
      // — the value is still visible for manual selection.
    }
  }
</script>

<button
  class="copy"
  class:copied
  onclick={copy}
  title={copied ? 'Copied!' : label}
  aria-label={label}
  type="button"
>
  {#if copied}
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <path d="M3 8.5l3.5 3.5L13 5" />
    </svg>
  {:else}
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">
      <rect x="5.5" y="5.5" width="8" height="8" rx="1.5" />
      <path d="M3.5 10.5h-.5a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1h7a1 1 0 0 1 1 1v.5" />
    </svg>
  {/if}
</button>

<style>
  .copy {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.4rem;
    height: 1.4rem;
    padding: 0;
    margin-left: 0.35rem;
    border: 0;
    background: transparent;
    color: var(--fg-3);
    border-radius: var(--radius-sm);
    cursor: pointer;
    vertical-align: middle;
    transition:
      color 0.1s,
      background 0.1s;
  }
  .copy:hover {
    color: var(--fg-0);
    background: var(--bg-hover);
  }
  .copy.copied {
    color: var(--success);
  }
  .copy :global(svg) {
    width: 12px;
    height: 12px;
  }
</style>

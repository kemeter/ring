<script lang="ts">
  /**
   * A credential the API returns once and never again (a token, a webhook
   * secret). Stays on screen until dismissed, with a copy button, since
   * losing it means issuing a new one.
   */
  import CopyButton from '#lib/CopyButton.svelte';

  interface Props {
    title: string;
    value: string;
    dismiss: () => void;
  }

  let { title, value, dismiss }: Props = $props();
</script>

<section class="card reveal" role="status">
  <div class="text">
    <strong>{title}</strong>
    <span class="muted">Copy it now: it will not be shown again.</span>
  </div>
  <div class="value">
    <code>{value}</code>
    <CopyButton {value} label="Copy" />
  </div>
  <button class="btn-secondary" type="button" onclick={dismiss}>Done</button>
</section>

<style>
  .reveal {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem 1rem;
    padding: 0.9rem 1.125rem;
    margin-bottom: 1.25rem;
    border-color: var(--accent);
    background: var(--accent-bg);
  }
  .text {
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    font-size: 0.82rem;
  }
  .value {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex: 1;
    min-width: 0;
  }
  code {
    overflow-wrap: anywhere;
    color: var(--fg-0);
  }
</style>

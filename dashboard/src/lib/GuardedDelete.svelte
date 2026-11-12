<script lang="ts">
  /**
   * Delete for a resource the API protects while deployments use it (secrets,
   * volumes): the first attempt is a plain delete; a 409 naming the
   * deployments in the way turns the button into "Delete anyway", which
   * retries with `force`.
   */
  import ConfirmAction from '#lib/ConfirmAction.svelte';
  import { blockingDeployments } from '#lib/api.ts';

  interface Props {
    /** `namespace/name`, for the prompts. */
    label: string;
    /** What breaks when forced, e.g. "will fail to start". */
    consequence: string;
    remove: (force: boolean) => Promise<void>;
  }

  let { label, consequence, remove }: Props = $props();

  let usedBy = $state<string[] | null>(null);

  async function attempt(force: boolean) {
    try {
      await remove(force);
    } catch (e) {
      const names = blockingDeployments(e);
      if (names && !force) {
        usedBy = names;
      }
      throw e;
    }
  }
</script>

{#if usedBy}
  <span class="blocked">Used by {usedBy.join(', ')}</span>
  <ConfirmAction
    label="Delete anyway"
    prompt={`Deployments using it ${consequence}.`}
    pendingLabel="Deleting…"
    action={() => attempt(true)}
  />
{:else}
  <ConfirmAction
    label="Delete"
    prompt={`Delete ${label}?`}
    pendingLabel="Deleting…"
    action={() => attempt(false)}
  />
{/if}

<style>
  .blocked {
    margin-right: 0.6rem;
    font-size: 0.75rem;
    color: var(--warning);
  }
</style>

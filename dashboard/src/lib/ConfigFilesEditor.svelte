<script lang="ts">
  /**
   * Edits a config's `data`, which is a JSON object mapping a key (usually a
   * file name, picked by a `type: config` volume's `key`) to its contents.
   * Each entry gets a name and a plain textarea, so nobody has to escape a
   * file into a JSON string by hand. A payload that is not such an object
   * (left by an older client) opens as raw text instead.
   */
  interface Props {
    /** The JSON payload, kept in sync as the entries change. */
    value: string;
  }

  let { value = $bindable() }: Props = $props();

  interface Entry {
    key: string;
    content: string;
  }

  function parse(raw: string): Entry[] | null {
    if (raw.trim() === '') {
      return [{ key: '', content: '' }];
    }
    try {
      const parsed: unknown = JSON.parse(raw);
      if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed)) {
        return null;
      }
      const entries = Object.entries(parsed as Record<string, unknown>);
      if (entries.some(([, v]) => typeof v !== 'string')) {
        return null;
      }
      return entries.map(([key, content]) => ({ key, content: content as string }));
    } catch {
      return null;
    }
  }

  // Read once: from here on the entries are the source of truth.
  const initial = parse(value);
  let raw = $state(initial === null);
  let entries = $state<Entry[]>(initial ?? []);

  function sync() {
    value = JSON.stringify(Object.fromEntries(entries.map((e) => [e.key, e.content])));
  }

  function add() {
    entries.push({ key: '', content: '' });
  }

  function removeEntry(index: number) {
    entries.splice(index, 1);
    sync();
  }
</script>

{#if raw}
  <label class="field">
    <span>Data (raw)</span>
    <textarea bind:value spellcheck="false"></textarea>
    <span class="hint">
      Not a JSON object of files. Saving requires a JSON object such as
      <code>{'{"site.conf": "…"}'}</code>.
    </span>
  </label>
{:else}
  <div class="entries">
    {#each entries as entry, i}
      <div class="entry">
        <div class="entry-head">
          <label class="field">
            <span>Key</span>
            <input
              class="mono"
              bind:value={entry.key}
              oninput={sync}
              placeholder="site.conf"
              required
              autocomplete="off"
            />
          </label>
          {#if entries.length > 1}
            <button class="btn-secondary" type="button" onclick={() => removeEntry(i)}>Remove</button>
          {/if}
        </div>
        <label class="field">
          <span>Contents</span>
          <textarea bind:value={entry.content} oninput={sync} spellcheck="false"></textarea>
        </label>
      </div>
    {/each}
    <div>
      <button class="btn-secondary" type="button" onclick={add}>Add a file</button>
      <span class="hint">A <code>type: config</code> volume mounts one entry, picked by its <code>key</code>.</span>
    </div>
  </div>
{/if}

<style>
  .entries {
    display: flex;
    flex-direction: column;
    gap: 0.85rem;
  }
  .entry {
    display: flex;
    flex-direction: column;
    gap: 0.6rem;
    padding: 0.75rem;
    border: 1px solid var(--border);
    border-radius: var(--radius);
  }
  .entry-head {
    display: flex;
    align-items: flex-end;
    gap: 0.6rem;
  }
  .entry-head .field {
    flex: 1;
  }
  .hint {
    margin-left: 0.6rem;
    font-size: 0.72rem;
    color: var(--fg-3);
  }
</style>

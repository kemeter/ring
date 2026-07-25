<script lang="ts" generics="T">
  /**
   * One card of the deployment detail page: a titled header with a count, a
   * table of rows, and a placeholder when there is nothing to show.
   *
   * The detail page repeats this shape for ports, volumes, environment and
   * health checks — same header, same empty state, same table skeleton, only
   * the columns and cells differ.
   */
  import type { Snippet } from 'svelte';

  interface Props {
    title: string;
    /** Column headers, in order. `align: 'right'` gets tabular numerals. */
    columns: Array<{ label: string; align?: 'right' }>;
    items: T[];
    /** Shown in place of the table when `items` is empty. */
    emptyText: string;
    /** Stable key per item; falls back to the index when omitted. */
    key?: (item: T, index: number) => string | number;
    /** One `<tr>` per item. */
    row: Snippet<[T]>;
  }

  let { title, columns, items, emptyText, key, row }: Props = $props();
</script>

<section class="card">
  <header class="section-head">
    <h2>{title}</h2>
    <span class="count">{items.length}</span>
  </header>
  {#if items.length === 0}
    <p class="muted pad">{emptyText}</p>
  {:else}
    <table>
      <thead>
        <tr>
          {#each columns as col}
            <th class:num={col.align === 'right'}>{col.label}</th>
          {/each}
        </tr>
      </thead>
      <tbody>
        {#each items as item, i (key ? key(item, i) : i)}
          {@render row(item)}
        {/each}
      </tbody>
    </table>
  {/if}
</section>

<style>
  /* Structural rules the detail page defines for its own cards. Svelte scopes
   * styles per component, so they don't reach this child's DOM — the ones this
   * component's markup uses are repeated here rather than made global, keeping
   * the page's scoping intact. */
  .card {
    margin-bottom: 1rem;
  }
  .section-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
    padding: 0.85rem 1.125rem;
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: 0.95rem;
    font-weight: 600;
    letter-spacing: -0.01em;
  }
  .count {
    color: var(--fg-3);
    font-size: 0.75rem;
    font-variant-numeric: tabular-nums;
  }
  .muted.pad {
    padding: 1.25rem;
  }

  /* The detail page tables are denser than the global `app.css` ones (smaller
   * type, tighter padding, muted headers). Repeated here for the same scoping
   * reason as above — without them these tables would fall back to the global
   * style and look nothing like the page's other cards.
   *
   * Only the `th` side lives here: the `<td>`s come from the caller's `row`
   * snippet, so they are compiled into the page and keep its own `td` rules. */
  table {
    width: 100%;
    border-collapse: collapse;
  }
  th {
    text-align: left;
    padding: 0.65rem 1rem;
    border-bottom: 1px solid var(--border);
    font-weight: 500;
    font-size: 0.72rem;
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--fg-2);
    background: var(--bg-0);
  }
  th.num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
</style>

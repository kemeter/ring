// Pure formatting helpers shared by every list/detail page.

/** Relative time string for a refresh-meta line. `null` → empty so callers
 *  can bind it directly without guarding. */
export function timeAgo(d: Date | null): string {
  if (!d) {
    return '';
  }
  const s = Math.floor((Date.now() - d.getTime()) / 1000);
  if (s < 5) {
    return 'just now';
  }
  if (s < 60) {
    return `${s}s ago`;
  }
  return `${Math.floor(s / 60)}m ago`;
}

/** Render a server-side timestamp to the user's locale. The Ring API returns
 *  several formats depending on the endpoint:
 *   - RFC3339 (`2026-04-15T10:30:00Z`)
 *   - SQL-style with nanoseconds and a `UTC` suffix
 *     (`2026-05-13 19:26:27.931985780 UTC`) — produced by `chrono::Utc::now().to_string()`
 *  Browsers' `Date` parser accepts the first but rejects the second, so we
 *  normalise the second to ISO before parsing. Anything we can't parse is
 *  returned verbatim. */
export function formatDate(iso: string | null | undefined): string {
  if (!iso) {
    return '—';
  }
  const normalised = normaliseTimestamp(iso);
  const d = new Date(normalised);
  if (Number.isNaN(d.getTime())) {
    return iso;
  }
  return d.toLocaleString();
}

function normaliseTimestamp(input: string): string {
  // SQL-style: `YYYY-MM-DD HH:MM:SS[.nnn...] UTC` → `YYYY-MM-DDTHH:MM:SS[.nnn]Z`.
  // We keep at most 3 fractional digits because `Date` ignores anything beyond
  // milliseconds and some browsers complain about longer precision.
  const m = input.match(
    /^(\d{4}-\d{2}-\d{2}) (\d{2}:\d{2}:\d{2})(\.\d+)? UTC$/
  );
  if (m) {
    const frac = m[3] ? m[3].slice(0, 4) : '';
    return `${m[1]}T${m[2]}${frac}Z`;
  }
  return input;
}

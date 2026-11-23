// How a deployment's status reads in the UI. Mirrors `Status` in
// `src/models/deployments.rs`.

import type { Deployment } from './api';

export type StatusKind = 'success' | 'warn' | 'danger' | 'neutral';

/** Colour bucket of a status: running or done is green, a deployment still
 *  starting is amber, one backing off or given up is red. */
export function statusKind(status: string): StatusKind {
  switch (status) {
    case 'running':
    case 'completed':
      return 'success';
    case 'pending':
    case 'progressing':
      return 'warn';
    case 'backing_off':
    case 'failed':
      return 'danger';
    default:
      return 'neutral';
  }
}

/** The status with its failure reason, when it has one: `backing_off · crash_loop`,
 *  or else why some instances are still waiting: `running · waiting: insufficient_resources`. */
export function statusLabel(d: Pick<Deployment, 'status' | 'failure' | 'waiting'>): string {
  if (d.failure) {
    return `${d.status} · ${d.failure.reason}`;
  }
  if (d.waiting) {
    return `${d.status} · waiting: ${d.waiting.reason}`;
  }
  return d.status;
}

/** The detail behind `statusLabel`, for a tooltip or a line under it. */
export function statusMessage(d: Pick<Deployment, 'failure' | 'waiting'>): string | undefined {
  return d.failure?.message ?? d.waiting?.message ?? undefined;
}

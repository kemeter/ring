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

/** The status with its failure reason, when it has one: `backing_off · crash_loop`. */
export function statusLabel(d: Pick<Deployment, 'status' | 'failure'>): string {
  return d.failure ? `${d.status} · ${d.failure.reason}` : d.status;
}

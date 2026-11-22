/**
 * Ordering check for progress hints.
 *
 * Hints are per operation and strictly monotonic. A duplicate or a gap means
 * delivery was interrupted, so the caller must reload authoritative
 * `status`/`operation`; it must never infer completion or retry a write.
 */

import type { Id, Progress } from './wire';

export type ProgressSignal = 'advance' | 'duplicate' | 'gap';

export class ProgressStream {
  private readonly last = new Map<Id, bigint>();

  observe(progress: Progress): ProgressSignal {
    const sequence = BigInt(progress.sequence);
    const previous = this.last.get(progress.operationId);

    if (previous === undefined) {
      this.last.set(progress.operationId, sequence);
      return 'advance';
    }
    if (sequence <= previous) {
      return 'duplicate';
    }
    if (sequence !== previous + 1n) {
      // Advance the baseline so one interruption does not report forever; the
      // authoritative refresh decides what actually happened.
      this.last.set(progress.operationId, sequence);
      return 'gap';
    }
    this.last.set(progress.operationId, sequence);
    return 'advance';
  }
}

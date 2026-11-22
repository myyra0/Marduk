import type { Detail, WireError } from './transport/wire';

export function describeDetail(detail: Detail): string {
  if ('index' in detail) return `index ${detail.index}`;
  if ('first' in detail) return `indices ${detail.first} and ${detail.second}`;
  if ('kind' in detail) return detail.kind;
  return detail.reason;
}

/** Stable code first; the detail is the typed reason, never prose. */
export function describeBlocker(blocker: WireError): string {
  return blocker.detail === undefined
    ? blocker.code
    : `${blocker.code} (${describeDetail(blocker.detail)})`;
}

export function describeFailure(error: unknown): string {
  if (error instanceof Error && error.message.length > 0) {
    return error.message;
  }
  return 'Transport failed; the workspace state was not changed.';
}

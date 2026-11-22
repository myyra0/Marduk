/**
 * The desktop transport. One method per frozen Tauri command; every result is
 * validated by the wire mirrors before it reaches the UI.
 *
 * There is deliberately no generic invoke wrapper and no path or file argument
 * in this API. Selections are opaque IDs minted by the Rust process.
 */

import { invoke as tauriInvoke } from '@tauri-apps/api/core';

import {
  parseEnvelope,
  parseOperation,
  parsePrepared,
  parsePreview,
  parseRestoreResult,
  parseSelection,
  parseSnapshot,
  parseWorkspaceStatus,
  type Envelope,
  type Id,
  type Operation,
  type Prepared,
  type Preview,
  type RestoreResult,
  type Revision,
  type Selection,
  type Snapshot,
  type WorkspaceStatus,
} from './wire';

/** Exact command names accepted by the Rust handler. */
export const COMMANDS = [
  'pick_installation',
  'pick_zip',
  'setup',
  'status',
  'import_preview',
  'import_apply',
  'order',
  'deploy_preview',
  'deploy_apply',
  'restore',
  'operation',
] as const;

export type Command = (typeof COMMANDS)[number];
export type InvokeFn = (command: Command, args?: Record<string, unknown>) => Promise<unknown>;

export type TransportFaultKind = 'ipc' | 'invalid-response';

/** A failure to obtain or decode a response; never a wire result. */
export class TransportFault extends Error {
  constructor(
    readonly kind: TransportFaultKind,
    message: string,
  ) {
    super(message);
    this.name = 'TransportFault';
  }
}

export interface WorkspaceTransport {
  pickInstallation(): Promise<Envelope<Selection | null>>;
  pickZip(): Promise<Envelope<Selection | null>>;
  setup(selectionId: Id): Promise<Envelope<WorkspaceStatus>>;
  status(): Promise<Envelope<WorkspaceStatus>>;
  importPreview(
    selectionId: Id,
    name: string,
    atRevision: Revision,
  ): Promise<Envelope<Prepared>>;
  importApply(importId: Id): Promise<Envelope<Snapshot>>;
  order(atRevision: Revision, memberIds: readonly Id[]): Promise<Envelope<Snapshot>>;
  deployPreview(atRevision: Revision): Promise<Envelope<Preview>>;
  deployApply(planId: Id): Promise<Envelope<Operation>>;
  restore(): Promise<Envelope<RestoreResult>>;
  operation(operationId: Id): Promise<Envelope<Operation>>;
}

export function createTauriTransport(invoke: InvokeFn = tauriInvoke): WorkspaceTransport {
  async function request<T>(
    command: Command,
    args: Record<string, unknown> | undefined,
    parseResult: (value: unknown, field: string) => T,
  ): Promise<Envelope<T>> {
    let raw: unknown;
    try {
      raw = args === undefined ? await invoke(command) : await invoke(command, args);
    } catch {
      throw new TransportFault('ipc', `${command}: request failed`);
    }
    try {
      return parseEnvelope(raw, parseResult);
    } catch {
      throw new TransportFault('invalid-response', `${command}: refused an invalid response`);
    }
  }

  return {
    pickInstallation: () => request('pick_installation', undefined, parseSelection),
    pickZip: () => request('pick_zip', undefined, parseSelection),
    setup: (selectionId) => request('setup', { selectionId }, parseWorkspaceStatus),
    status: () => request('status', undefined, parseWorkspaceStatus),
    importPreview: (selectionId, name, atRevision) =>
      request('import_preview', { selectionId, name, atRevision }, parsePrepared),
    importApply: (importId) => request('import_apply', { importId }, parseSnapshot),
    order: (atRevision, memberIds) =>
      request('order', { atRevision, memberIds: [...memberIds] }, parseSnapshot),
    deployPreview: (atRevision) => request('deploy_preview', { atRevision }, parsePreview),
    deployApply: (planId) => request('deploy_apply', { planId }, parseOperation),
    restore: () => request('restore', undefined, parseRestoreResult),
    operation: (operationId) => request('operation', { operationId }, parseOperation),
  };
}

export function isTransportFault(error: unknown): error is TransportFault {
  return error instanceof TransportFault;
}

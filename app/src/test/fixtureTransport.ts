/**
 * Test-only transport fixtures. Product code never imports this module, so a
 * fixture can never grant mutation authority or produce a success in the app.
 */

import operationFixture from '../transport/fixtures/operation.json';
import preparedFixture from '../transport/fixtures/prepared.json';
import previewFixture from '../transport/fixtures/preview.json';
import progressFixture from '../transport/fixtures/progress.json';
import restoreFixture from '../transport/fixtures/restore-no-active.json';
import snapshotFixture from '../transport/fixtures/snapshot.json';
import statusFixture from '../transport/fixtures/status.json';
import type { Command, WorkspaceTransport } from '../transport/client';
import {
  parseEnvelope,
  parseOperation,
  parsePrepared,
  parsePreview,
  parseProgress,
  parseRestoreResult,
  parseSnapshot,
  parseWorkspaceStatus,
  type Member,
  type Operation,
  type Prepared,
  type Preview,
  type Progress,
  type RestoreResult,
  type Snapshot,
  type WorkspaceStatus,
} from '../transport/wire';

export type RecordedCall = { command: Command; args?: Record<string, unknown> };

function fixtureResult<T>(
  raw: unknown,
  parse: (value: unknown, field: string) => T,
): T {
  const envelope = parseEnvelope(raw, parse);
  if (!envelope.ok) {
    throw new Error('fixture must be a success envelope');
  }
  return envelope.result;
}

export const fixtureStatus = (): WorkspaceStatus =>
  fixtureResult(statusFixture, parseWorkspaceStatus);
export const fixtureSnapshot = (): Snapshot => fixtureResult(snapshotFixture, parseSnapshot);
export const fixturePrepared = (): Prepared => fixtureResult(preparedFixture, parsePrepared);
export const fixturePreview = (): Preview => fixtureResult(previewFixture, parsePreview);
export const fixtureOperation = (): Operation => fixtureResult(operationFixture, parseOperation);
export const fixtureRestore = (): RestoreResult =>
  fixtureResult(restoreFixture, parseRestoreResult);
export const fixtureProgress = (): Progress => parseProgress(progressFixture, 'progress');

function reordered(members: Member[], memberIds: readonly string[]): Member[] {
  const byId = new Map(members.map((member) => [member.id, member]));
  const ordered = memberIds
    .map((id) => byId.get(id))
    .filter((member): member is Member => member !== undefined);
  return ordered.length === members.length ? ordered : members;
}

export function createStubTransport(overrides: Partial<WorkspaceTransport> = {}): {
  transport: WorkspaceTransport;
  calls: RecordedCall[];
} {
  const calls: RecordedCall[] = [];
  function record(command: Command, args?: Record<string, unknown>) {
    calls.push(args === undefined ? { command } : { command, args });
  }

  const transport: WorkspaceTransport = {
    async pickInstallation() {
      record('pick_installation');
      return { ok: true, result: { selectionId: '1', label: 'Skyrim Special Edition' } };
    },
    async pickZip() {
      record('pick_zip');
      return { ok: true, result: { selectionId: '2', label: 'mods.zip' } };
    },
    async setup(selectionId) {
      record('setup', { selectionId });
      return { ok: true, result: fixtureStatus() };
    },
    async status() {
      record('status');
      return { ok: true, result: fixtureStatus() };
    },
    async importPreview(selectionId, name, atRevision) {
      record('import_preview', { selectionId, name, atRevision });
      return { ok: true, result: fixturePrepared() };
    },
    async importApply(importId) {
      record('import_apply', { importId });
      return { ok: true, result: fixtureSnapshot() };
    },
    async order(atRevision, memberIds) {
      record('order', { atRevision, memberIds: [...memberIds] });
      const snapshot = fixtureSnapshot();
      return {
        ok: true,
        result: { ...snapshot, revision: '43', members: reordered(snapshot.members, memberIds) },
      };
    },
    async deployPreview(atRevision) {
      record('deploy_preview', { atRevision });
      return { ok: true, result: fixturePreview() };
    },
    async deployApply(planId) {
      record('deploy_apply', { planId });
      return { ok: true, result: fixtureOperation() };
    },
    async restore() {
      record('restore');
      return { ok: true, result: fixtureRestore() };
    },
    async operation(operationId) {
      record('operation', { operationId });
      return { ok: true, result: fixtureOperation() };
    },
  };

  return { transport: { ...transport, ...overrides }, calls };
}

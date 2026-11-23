import { describe, expect, it } from 'vitest';

import errorInvalidPath from './fixtures/error-invalid-path.json';
import errorLimit from './fixtures/error-limit.json';
import errorStalePlan from './fixtures/error-stale-plan.json';
import operationFixture from './fixtures/operation.json';
import preparedFixture from './fixtures/prepared.json';
import previewFixture from './fixtures/preview.json';
import progressFixture from './fixtures/progress.json';
import restoreFixture from './fixtures/restore-no-active.json';
import snapshotFixture from './fixtures/snapshot.json';
import statusFixture from './fixtures/status.json';
import {
  parseEnvelope,
  parseOperation,
  parsePrepared,
  parsePreview,
  parseProgress,
  parseRestoreResult,
  parseSnapshot,
  parseWorkspaceStatus,
  WireFormatError,
} from './wire';

const MAX_SAFE = BigInt(Number.MAX_SAFE_INTEGER);

function draft(raw: unknown, mutate: (value: any) => void): unknown {
  const copy = JSON.parse(JSON.stringify(raw)) as any;
  mutate(copy);
  return copy;
}

describe('Rust-generated wire fixtures', () => {
  it('keeps IDs and revisions above 2^53 as exact decimal strings', () => {
    const envelope = parseEnvelope(statusFixture, parseWorkspaceStatus);
    expect(envelope.ok).toBe(true);
    if (!envelope.ok) return;

    const status = envelope.result;
    expect(status.profile.revision).toBe('9223372036854775807');
    expect(status.profile.members[0].id).toBe('9007199254740997');
    expect(BigInt(status.profile.members[0].id)).toBeGreaterThan(MAX_SAFE);
    expect(status.installation?.id).toBe('9007199254740993');
    expect(status.activeGenerationId).toBe('9223372036854775804');

    // A JSON round trip must not turn the decimal text into a number.
    const roundTripped = JSON.parse(JSON.stringify(status)) as typeof status;
    expect(roundTripped.profile.revision).toBe('9223372036854775807');
    expect(typeof roundTripped.profile.members[0].id).toBe('string');
  });

  it('parses every success DTO shape', () => {
    const snapshot = parseEnvelope(snapshotFixture, parseSnapshot);
    expect(snapshot.ok && snapshot.result.members).toHaveLength(2);

    const operation = parseEnvelope(operationFixture, parseOperation);
    expect(operation.ok && operation.result.phase).toBe('completed');

    const prepared = parseEnvelope(preparedFixture, parsePrepared);
    expect(prepared.ok && prepared.result.archiveSha256).toHaveLength(64);

    const preview = parseEnvelope(previewFixture, parsePreview);
    expect(preview.ok && preview.result.blockers).toHaveLength(2);

    const restore = parseEnvelope(restoreFixture, parseRestoreResult);
    expect(restore).toEqual({
      ok: true,
      result: { outcome: 'noActiveDeployment', code: 'NoActiveDeployment' },
    });
  });

  it('parses a reported stage displacement without relabeling the slot', () => {
    const reported = draft(statusFixture, (value) => {
      value.result.pending.retained[0].slot = 'stage';
    });
    const envelope = parseEnvelope(reported, parseWorkspaceStatus);
    expect(envelope.ok && envelope.result.pending?.retained[0].slot).toBe('stage');
  });

  it('parses the bare progress channel payload', () => {
    expect(progressFixture).toEqual({
      operationId: '9223372036854775801',
      sequence: '9007199254741009',
      phase: 'applying',
    });
    expect(parseProgress(progressFixture)).toEqual({
      operationId: '9223372036854775801',
      sequence: '9007199254741009',
      phase: 'applying',
    });
  });

  it('keeps the stable code and typed detail when prose changes', () => {
    const original = parseEnvelope(errorInvalidPath, () => null);
    expect(original).toEqual({
      ok: false,
      error: { code: 'InvalidPath', detail: { reason: 'MissingParent' } },
    });

    const withWording = draft(errorInvalidPath, (value) => {
      value.error.message = 'The selected folder is not usable on this device.';
    });
    expect(parseEnvelope(withWording, () => null)).toEqual(original);
  });

  it('parses limit details and detail-free blockers', () => {
    expect(parseEnvelope(errorLimit, () => null)).toEqual({
      ok: false,
      error: { code: 'LimitExceeded', detail: { kind: 'Entries' } },
    });
    expect(parseEnvelope(errorStalePlan, () => null)).toEqual({
      ok: false,
      error: { code: 'StalePlan' },
    });
  });

  it('refuses invalid DTOs instead of coercing them', () => {
    type Parser = (value: unknown, field: string) => unknown;
    const cases: Array<[string, unknown, Parser]> = [
      [
        'numeric member id',
        draft(statusFixture, (value) => {
          value.result.profile.members[0].id = 9007199254740993;
        }),
        parseWorkspaceStatus,
      ],
      [
        'leading zero',
        draft(statusFixture, (value) => {
          value.result.profile.members[0].id = '09007199254740993';
        }),
        parseWorkspaceStatus,
      ],
      [
        'above i64::MAX',
        draft(statusFixture, (value) => {
          value.result.profile.revision = '9223372036854775808';
        }),
        parseWorkspaceStatus,
      ],
      [
        'negative revision',
        draft(statusFixture, (value) => {
          value.result.profile.revision = '-1';
        }),
        parseWorkspaceStatus,
      ],
      [
        'missing installedId',
        draft(statusFixture, (value) => {
          delete value.result.profile.members[0].installedId;
        }),
        parseWorkspaceStatus,
      ],
      [
        'blocked without a blocker',
        draft(statusFixture, (value) => {
          delete value.result.availability.deploy.blocker;
        }),
        parseWorkspaceStatus,
      ],
      [
        'available with a blocker',
        draft(statusFixture, (value) => {
          value.result.availability.target.blocker = { code: 'Busy' };
        }),
        parseWorkspaceStatus,
      ],
      [
        'missing activeGenerationId',
        draft(statusFixture, (value) => {
          delete value.result.activeGenerationId;
        }),
        parseWorkspaceStatus,
      ],
      [
        'unknown operation phase',
        draft(operationFixture, (value) => {
          value.result.phase = 'sideways';
        }),
        parseOperation,
      ],
      [
        'unknown pending phase',
        draft(statusFixture, (value) => {
          value.result.pending.phase = 'sideways';
        }),
        parseWorkspaceStatus,
      ],
      [
        'fractional ordinal',
        draft(statusFixture, (value) => {
          value.result.pending.retained[0].ordinal = 1.5;
        }),
        parseWorkspaceStatus,
      ],
      [
        'unknown error code',
        draft(errorStalePlan, (value) => {
          value.error.code = 'SomethingElse';
        }),
        () => null,
      ],
      [
        'unknown detail shape',
        draft(errorInvalidPath, (value) => {
          value.error.detail = { path: '/home/user/secret' };
        }),
        () => null,
      ],
      [
        'success envelope carrying an error',
        draft(statusFixture, (value) => {
          value.error = { code: 'Busy' };
        }),
        parseWorkspaceStatus,
      ],
    ];

    for (const [name, value, parse] of cases) {
      expect(() => parseEnvelope(value, parse), name).toThrow(WireFormatError);
    }
  });
});

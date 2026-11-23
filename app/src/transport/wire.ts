/**
 * Runtime mirrors of the frozen Rust client wire types.
 *
 * Every value crosses the boundary as JSON produced by `marduk-workspace`.
 * IDs and revisions stay decimal strings: parsing uses BigInt for range checks
 * only and always returns the original text, so values above `2^53` survive.
 * Unknown shapes are refused instead of being coerced.
 */

export const CODES = [
  'InvalidRequest',
  'InvalidName',
  'InvalidOrder',
  'StalePlan',
  'MemberNotFound',
  'Busy',
  'UnknownImport',
  'ImportNotPrepared',
  'StageChanged',
  'StageUnavailable',
  'Storage',
  'InvalidContainer',
  'UnsupportedContainer',
  'DuplicateRawName',
  'UnsupportedEntry',
  'UnsafeName',
  'ConflictingNames',
  'LimitExceeded',
  'CorruptEntry',
  'MissingEntry',
  'InvalidPath',
  'TargetUnavailable',
  'UnsupportedTarget',
  'ActiveDeployment',
  'RecoveryConflict',
  'UnknownOperation',
  'NoActiveDeployment',
  'LeaseUnreadable',
  'Io',
] as const;

export type Code = (typeof CODES)[number];

export const LIMIT_KINDS = [
  'ArchiveBytes',
  'DirectoryBytes',
  'Entries',
  'FileBytes',
  'TotalBytes',
] as const;

export type LimitKind = (typeof LIMIT_KINDS)[number];

export const REASONS = [
  'MissingParent',
  'AmbiguousName',
  'OutsideBoundary',
  'ObjectKind',
  'Metadata',
  'Sharing',
  'Configuration',
] as const;

export type Reason = (typeof REASONS)[number];

export type Detail =
  | { index: number }
  | { first: number; second: number }
  | { kind: LimitKind }
  | { reason: Reason };

export type WireError = { code: Code; detail?: Detail; message?: string };
export type Blocker = WireError;

export type Envelope<T> = { ok: true; result: T } | { ok: false; error: WireError };

export type Id = string;
export type Revision = string;

export type Member = {
  id: Id;
  installedId: Id;
  enabled: boolean;
  displayName: string;
};

/** Members are ordered low priority to high priority. */
export type Snapshot = { id: Id; revision: Revision; members: Member[] };

export type Availability = { available: true } | { available: false; blocker: Blocker };

export type Installation = { id: Id; label: string };

export type Phase =
  | 'preparing'
  | 'prepared'
  | 'applying'
  | 'rolling_back'
  | 'restoring'
  | 'completed'
  | 'rolled_back'
  | 'cancelled';

export const PHASES: readonly Phase[] = [
  'preparing',
  'prepared',
  'applying',
  'rolling_back',
  'restoring',
  'completed',
  'rolled_back',
  'cancelled',
];

export type Retained = {
  operationId: Id;
  ordinal: number;
  direction: 'forward' | 'undo';
  slot: 'original' | 'step' | 'displaced' | 'alternate' | 'stage';
};

export type Operation = {
  id: Id;
  kind: 'deploy' | 'restore';
  phase: Phase;
  generationId: Id | null;
  blocker: Blocker | null;
  retained: Retained[];
};

export type AvailabilitySet = {
  target: Availability;
  import: Availability;
  deploy: Availability;
  restore: Availability;
};

/** Stable reason tokens for a leftover import the catalog refused to reclaim. */
export const QUARANTINE_REASONS = [
  'ownership_missing',
  'stage_changed',
  'stage_unavailable',
  'published',
] as const;

export type QuarantineReason = (typeof QUARANTINE_REASONS)[number];

export type QuarantinedLeftover = { id: Id; reason: QuarantineReason };

/**
 * The one startup reconciliation. It is computed when the workspace opens, so
 * polling status does not repeat it.
 */
export type Reconciliation = {
  reclaimed: Id[];
  quarantined: QuarantinedLeftover[];
  quarantinedCount: number;
};

/**
 * The installation lease record a status observed, naming the exact file an
 * owner must remove when a crashed deploy left a claim that cannot be cleared
 * safely.
 */
export type LeaseInfo = { path: string };

export type WorkspaceStatus = {
  installation: Installation | null;
  profile: Snapshot;
  availability: AvailabilitySet;
  pending: Operation | null;
  activeGenerationId: Id | null;
  lease: LeaseInfo | null;
  reconciliation: Reconciliation;
};

export type Target = { role: 'data'; path: string };

export type Prepared = {
  id: Id;
  profileId: Id;
  expectedRevision: Revision;
  archiveSha256: string;
  files: Target[];
};

export type PreviewFile = {
  target: Target;
  winner: Id;
  candidates: Id[];
  needsBackup: boolean;
};

export type Preview = {
  id: Id | null;
  profileId: Id;
  revision: Revision;
  installationId: Id;
  label: string;
  files: PreviewFile[];
  blockers: Blocker[];
};

export type RestoreResult =
  | { outcome: 'operation'; operation: Operation }
  | { outcome: 'noActiveDeployment'; code: 'NoActiveDeployment' };

/** Crash reconciliation result: the recorded operation, or nothing pending. */
export type Recover = Operation | null;

export type Selection = { selectionId: Id; label: string };

/**
 * Ordered progress hint on the per-operation channel. It is keyed by operation
 * and monotonic sequence, and is never authoritative state.
 */
export type Progress = {
  operationId: Id;
  sequence: Id;
  phase: Phase;
};

export class WireFormatError extends Error {
  constructor(message: string) {
    super(message);
    this.name = 'WireFormatError';
  }
}

const MAX_WIRE_ID = 9223372036854775807n;

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function record(value: unknown, field: string): Record<string, unknown> {
  if (!isRecord(value)) {
    throw new WireFormatError(`${field}: expected object`);
  }
  return value;
}

function text(value: unknown, field: string): string {
  if (typeof value !== 'string') {
    throw new WireFormatError(`${field}: expected string`);
  }
  return value;
}

function bool(value: unknown, field: string): boolean {
  if (typeof value !== 'boolean') {
    throw new WireFormatError(`${field}: expected boolean`);
  }
  return value;
}

function list(value: unknown, field: string): unknown[] {
  if (!Array.isArray(value)) {
    throw new WireFormatError(`${field}: expected array`);
  }
  return value;
}

function integer(value: unknown, field: string, max = Number.MAX_SAFE_INTEGER): number {
  if (typeof value !== 'number' || !Number.isInteger(value) || value < 0 || value > max) {
    throw new WireFormatError(`${field}: expected nonnegative integer`);
  }
  return value;
}

function oneOf<T extends string>(value: unknown, allowed: readonly T[], field: string): T {
  if (typeof value !== 'string' || !(allowed as readonly string[]).includes(value)) {
    throw new WireFormatError(`${field}: unknown value`);
  }
  return value as T;
}

/** Canonical nonnegative decimal string, at most `i64::MAX`. */
export function parseDecimal(value: unknown, field: string, allowZero: boolean): string {
  const raw = text(value, field);
  if (raw.length === 0 || !/^[0-9]+$/.test(raw)) {
    throw new WireFormatError(`${field}: expected canonical decimal string`);
  }
  if (raw.length > 1 && raw.startsWith('0')) {
    throw new WireFormatError(`${field}: leading zero`);
  }
  const parsed = BigInt(raw);
  if (parsed > MAX_WIRE_ID) {
    throw new WireFormatError(`${field}: out of range`);
  }
  if (!allowZero && parsed === 0n) {
    throw new WireFormatError(`${field}: must be positive`);
  }
  return raw;
}

export function parseId(value: unknown, field = 'id'): Id {
  return parseDecimal(value, field, false);
}

export function parseRevision(value: unknown, field = 'revision'): Revision {
  return parseDecimal(value, field, true);
}

export function parseCode(value: unknown, field = 'code'): Code {
  return oneOf(value, CODES, field);
}

export function parseDetail(value: unknown, field = 'detail'): Detail {
  const detail = record(value, field);
  const keys = Object.keys(detail);

  if ('index' in detail) {
    if (keys.length !== 1) throw new WireFormatError(`${field}: unexpected keys`);
    return { index: integer(detail.index, `${field}.index`) };
  }
  if ('first' in detail || 'second' in detail) {
    if (keys.length !== 2 || !('first' in detail) || !('second' in detail)) {
      throw new WireFormatError(`${field}: incomplete pair`);
    }
    return {
      first: integer(detail.first, `${field}.first`),
      second: integer(detail.second, `${field}.second`),
    };
  }
  if ('kind' in detail) {
    if (keys.length !== 1) throw new WireFormatError(`${field}: unexpected keys`);
    return { kind: oneOf(detail.kind, LIMIT_KINDS, `${field}.kind`) };
  }
  if ('reason' in detail) {
    if (keys.length !== 1) throw new WireFormatError(`${field}: unexpected keys`);
    return { reason: oneOf(detail.reason, REASONS, `${field}.reason`) };
  }
  throw new WireFormatError(`${field}: unknown shape`);
}

export function parseWireError(value: unknown, field = 'error'): WireError {
  const error = record(value, field);
  const code = parseCode(error.code, `${field}.code`);
  const message =
    error.message === undefined ? undefined : text(error.message, `${field}.message`);
  const base: WireError = message === undefined ? { code } : { code, message };
  if (error.detail === undefined) {
    return base;
  }
  return { ...base, detail: parseDetail(error.detail, `${field}.detail`) };
}

export function parseEnvelope<T>(
  value: unknown,
  parseResult: (value: unknown, field: string) => T,
): Envelope<T> {
  const envelope = record(value, 'envelope');
  const keys = Object.keys(envelope);

  if (envelope.ok === true) {
    if (keys.length !== 2 || !('result' in envelope) || 'error' in envelope) {
      throw new WireFormatError('envelope: unexpected success shape');
    }
    return { ok: true, result: parseResult(envelope.result, 'result') };
  }
  if (envelope.ok === false) {
    if (keys.length !== 2 || !('error' in envelope) || 'result' in envelope) {
      throw new WireFormatError('envelope: unexpected failure shape');
    }
    return { ok: false, error: parseWireError(envelope.error) };
  }
  throw new WireFormatError('envelope: missing ok flag');
}

export function parseMember(value: unknown, field = 'member'): Member {
  const member = record(value, field);
  return {
    id: parseId(member.id, `${field}.id`),
    installedId: parseId(member.installedId, `${field}.installedId`),
    enabled: bool(member.enabled, `${field}.enabled`),
    displayName: text(member.displayName, `${field}.displayName`),
  };
}

export function parseSnapshot(value: unknown, field = 'snapshot'): Snapshot {
  const snapshot = record(value, field);
  return {
    id: parseId(snapshot.id, `${field}.id`),
    revision: parseRevision(snapshot.revision, `${field}.revision`),
    members: list(snapshot.members, `${field}.members`).map((member, index) =>
      parseMember(member, `${field}.members[${index}]`),
    ),
  };
}

export function parseAvailability(value: unknown, field = 'availability'): Availability {
  const availability = record(value, field);
  const available = bool(availability.available, `${field}.available`);
  if (available) {
    if (availability.blocker !== undefined) {
      throw new WireFormatError(`${field}: available result carries a blocker`);
    }
    return { available: true };
  }
  return { available: false, blocker: parseWireError(availability.blocker, `${field}.blocker`) };
}

export function parseOperation(value: unknown, field = 'operation'): Operation {
  const operation = record(value, field);
  for (const key of ['generationId', 'blocker']) {
    if (!(key in operation)) {
      throw new WireFormatError(`${field}.${key}: missing`);
    }
  }
  const generationId = operation.generationId;
  const blocker = operation.blocker;
  return {
    id: parseId(operation.id, `${field}.id`),
    kind: oneOf(operation.kind, ['deploy', 'restore'] as const, `${field}.kind`),
    phase: oneOf(operation.phase, PHASES, `${field}.phase`),
    generationId:
      generationId === null || generationId === undefined
        ? null
        : parseId(generationId, `${field}.generationId`),
    blocker:
      blocker === null || blocker === undefined
        ? null
        : parseWireError(blocker, `${field}.blocker`),
    retained: list(operation.retained, `${field}.retained`).map((entry, index) => {
      const retainedField = `${field}.retained[${index}]`;
      const retained = record(entry, retainedField);
      return {
        operationId: parseId(retained.operationId, `${retainedField}.operationId`),
        ordinal: integer(retained.ordinal, `${retainedField}.ordinal`, 4_294_967_295),
        direction: oneOf(
          retained.direction,
          ['forward', 'undo'] as const,
          `${retainedField}.direction`,
        ),
        slot: oneOf(
          retained.slot,
          ['original', 'step', 'displaced', 'alternate', 'stage'] as const,
          `${retainedField}.slot`,
        ),
      };
    }),
  };
}

export function parseReconciliation(value: unknown, field = 'reconciliation'): Reconciliation {
  const reconciliation = record(value, field);
  const quarantined = list(
    reconciliation.quarantined,
    `${field}.quarantined`,
  ).map((entry, index) => {
    const entryField = `${field}.quarantined[${index}]`;
    const leftover = record(entry, entryField);
    return {
      id: parseId(leftover.id, `${entryField}.id`),
      reason: oneOf(leftover.reason, QUARANTINE_REASONS, `${entryField}.reason`),
    };
  });
  const quarantinedCount = integer(
    reconciliation.quarantinedCount,
    `${field}.quarantinedCount`,
  );
  if (quarantinedCount !== quarantined.length) {
    throw new WireFormatError(`${field}.quarantinedCount: does not match quarantined`);
  }
  return {
    reclaimed: list(reconciliation.reclaimed, `${field}.reclaimed`).map((id, index) =>
      parseId(id, `${field}.reclaimed[${index}]`),
    ),
    quarantined,
    quarantinedCount,
  };
}

export function parseWorkspaceStatus(value: unknown, field = 'status'): WorkspaceStatus {
  const status = record(value, field);
  for (const key of [
    'installation',
    'pending',
    'activeGenerationId',
    'lease',
    'reconciliation',
  ]) {
    if (!(key in status)) {
      throw new WireFormatError(`${field}.${key}: missing`);
    }
  }
  const installation = status.installation;
  const pending = status.pending;
  const activeGenerationId = status.activeGenerationId;
  const lease = status.lease;
  const availability = record(status.availability, `${field}.availability`);
  const installed =
    installation === null || installation === undefined
      ? null
      : record(installation, `${field}.installation`);

  return {
    installation:
      installed === null
        ? null
        : {
            id: parseId(installed.id, `${field}.installation.id`),
            label: text(installed.label, `${field}.installation.label`),
          },
    profile: parseSnapshot(status.profile, `${field}.profile`),
    availability: {
      target: parseAvailability(availability.target, `${field}.availability.target`),
      import: parseAvailability(availability.import, `${field}.availability.import`),
      deploy: parseAvailability(availability.deploy, `${field}.availability.deploy`),
      restore: parseAvailability(availability.restore, `${field}.availability.restore`),
    },
    pending:
      pending === null || pending === undefined
        ? null
        : parseOperation(pending, `${field}.pending`),
    activeGenerationId:
      activeGenerationId === null || activeGenerationId === undefined
        ? null
        : parseId(activeGenerationId, `${field}.activeGenerationId`),
    lease:
      lease === null || lease === undefined
        ? null
        : {
            path: text(record(lease, `${field}.lease`).path, `${field}.lease.path`),
          },
    reconciliation: parseReconciliation(status.reconciliation, `${field}.reconciliation`),
  };
}

export function parseTarget(value: unknown, field = 'target'): Target {
  const target = record(value, field);
  return {
    role: oneOf(target.role, ['data'] as const, `${field}.role`),
    path: text(target.path, `${field}.path`),
  };
}

export function parsePrepared(value: unknown, field = 'prepared'): Prepared {
  const prepared = record(value, field);
  const sha = text(prepared.archiveSha256, `${field}.archiveSha256`);
  if (!/^[0-9a-f]{64}$/.test(sha)) {
    throw new WireFormatError(`${field}.archiveSha256: expected lowercase hex`);
  }
  return {
    id: parseId(prepared.id, `${field}.id`),
    profileId: parseId(prepared.profileId, `${field}.profileId`),
    expectedRevision: parseRevision(prepared.expectedRevision, `${field}.expectedRevision`),
    archiveSha256: sha,
    files: list(prepared.files, `${field}.files`).map((target, index) =>
      parseTarget(target, `${field}.files[${index}]`),
    ),
  };
}

export function parsePreview(value: unknown, field = 'preview'): Preview {
  const preview = record(value, field);
  if (!('id' in preview)) {
    throw new WireFormatError(`${field}.id: missing`);
  }
  const id = preview.id;
  return {
    id: id === null || id === undefined ? null : parseId(id, `${field}.id`),
    profileId: parseId(preview.profileId, `${field}.profileId`),
    revision: parseRevision(preview.revision, `${field}.revision`),
    installationId: parseId(preview.installationId, `${field}.installationId`),
    label: text(preview.label, `${field}.label`),
    files: list(preview.files, `${field}.files`).map((entry, index) => {
      const fileField = `${field}.files[${index}]`;
      const file = record(entry, fileField);
      return {
        target: parseTarget(file.target, `${fileField}.target`),
        winner: parseId(file.winner, `${fileField}.winner`),
        candidates: list(file.candidates, `${fileField}.candidates`).map((candidate, candidateIndex) =>
          parseId(candidate, `${fileField}.candidates[${candidateIndex}]`),
        ),
        needsBackup: bool(file.needsBackup, `${fileField}.needsBackup`),
      };
    }),
    blockers: list(preview.blockers, `${field}.blockers`).map((blocker, index) =>
      parseWireError(blocker, `${field}.blockers[${index}]`),
    ),
  };
}

export function parseRestoreResult(value: unknown, field = 'restore'): RestoreResult {
  const restore = record(value, field);
  const outcome = text(restore.outcome, `${field}.outcome`);
  if (outcome === 'operation') {
    return { outcome, operation: parseOperation(restore.operation, `${field}.operation`) };
  }
  if (outcome === 'noActiveDeployment') {
    if (restore.code !== 'NoActiveDeployment') {
      throw new WireFormatError(`${field}.code: expected NoActiveDeployment`);
    }
    return { outcome, code: 'NoActiveDeployment' };
  }
  throw new WireFormatError(`${field}.outcome: unknown value`);
}

export function parseRecover(value: unknown, field = 'recover'): Recover {
  if (value === null) {
    return null;
  }
  return parseOperation(value, field);
}

export function parseProgress(value: unknown, field = 'progress'): Progress {
  const progress = record(value, field);
  return {
    operationId: parseId(progress.operationId, `${field}.operationId`),
    sequence: parseId(progress.sequence, `${field}.sequence`),
    phase: oneOf(progress.phase, PHASES, `${field}.phase`),
  };
}

export function parseSelection(value: unknown, field = 'selection'): Selection | null {
  if (value === null) {
    return null;
  }
  const selection = record(value, field);
  return {
    selectionId: parseId(selection.selectionId, `${field}.selectionId`),
    label: text(selection.label, `${field}.label`),
  };
}

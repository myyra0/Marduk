import { useCallback, useEffect, useRef, useState } from 'react';

import { ModList } from './components/ModList';
import { describeBlocker, describeFailure } from './format';
import type { WorkspaceTransport } from './transport/client';
import { ProgressStream, type ProgressSignal } from './transport/progress';
import type {
  Availability,
  Envelope,
  Id,
  Operation,
  Prepared,
  Preview,
  Progress,
  Revision,
  Snapshot,
  WireError,
  WorkspaceStatus,
} from './transport/wire';

type StatusState =
  | { kind: 'loading' }
  | { kind: 'ready'; status: WorkspaceStatus; stale: boolean }
  | { kind: 'failed'; message: string };

type RecoveryOutcome =
  | { kind: 'reconciled'; operation: Operation }
  | { kind: 'none' }
  | { kind: 'blocked' };

type Props = {
  transport: WorkspaceTransport;
};

export function App({ transport }: Props) {
  const [state, setState] = useState<StatusState>({ kind: 'loading' });
  const [notice, setNotice] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [prepared, setPrepared] = useState<Prepared | null>(null);
  const [importName, setImportName] = useState('');
  const [review, setReview] = useState<Preview | null>(null);
  const [reviewError, setReviewError] = useState<WireError | null>(null);
  const [reviewInvalidated, setReviewInvalidated] = useState(false);
  const [operationState, setOperationState] = useState<Operation | null>(null);
  const [restoreNoActive, setRestoreNoActive] = useState(false);
  const [progressHint, setProgressHint] = useState<Progress | null>(null);
  const [progressIssue, setProgressIssue] = useState<'gap' | 'duplicate' | 'fault' | null>(
    null,
  );
  const [recoveryError, setRecoveryError] = useState<WireError | null>(null);
  const mounted = useRef(true);
  const stream = useRef(new ProgressStream());
  const operationRef = useRef<Operation | null>(null);
  const recoveryInFlight = useRef(false);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refresh = useCallback(
    async (
      announce: string | null = null,
      holdBusy = false,
    ): Promise<WorkspaceStatus | null> => {
      setBusy(true);
      try {
        const envelope = await transport.status();
        if (!mounted.current) return null;
        if (envelope.ok) {
          setState({ kind: 'ready', status: envelope.result, stale: false });
          if (envelope.result.activeGenerationId !== null) {
            setRestoreNoActive(false);
          }
          setNotice(announce);
          return envelope.result;
        }
        const message = describeBlocker(envelope.error);
        setState((previous) =>
          previous.kind === 'ready'
            ? { ...previous, stale: true }
            : { kind: 'failed', message },
        );
        setNotice(message);
        return null;
      } catch (error) {
        if (!mounted.current) return null;
        const message = describeFailure(error);
        setState((previous) =>
          previous.kind === 'ready'
            ? { ...previous, stale: true }
            : { kind: 'failed', message },
        );
        setNotice(message);
        return null;
      } finally {
        if (!holdBusy && mounted.current) setBusy(false);
      }
    },
    [transport],
  );

  const loadOperation = useCallback(
    async (operationId: Id): Promise<Operation | null> => {
      try {
        const envelope = await transport.operation(operationId);
        if (!mounted.current) return null;
        if (envelope.ok) {
          operationRef.current = envelope.result;
          setOperationState(envelope.result);
          return envelope.result;
        }
        setNotice(`Operation refresh failed: ${describeBlocker(envelope.error)}`);
      } catch (error) {
        if (mounted.current) setNotice(describeFailure(error));
      }
      return null;
    },
    [transport],
  );

  /**
   * Reconcile recorded pending work after a crash or reconnect.
   *
   * `none` means the journal held no pending row, so the status that triggered
   * the call was stale. A non-null blocker is a refusal: the envelope is `Ok`,
   * but the recorded work did not settle.
   */
  const recoverPending = useCallback(async (): Promise<RecoveryOutcome> => {
    try {
      const envelope = await transport.recover();
      if (!mounted.current) return { kind: 'blocked' };
      if (envelope.ok) {
        const operation = envelope.result;
        if (operation === null) {
          setRecoveryError(null);
          return { kind: 'none' };
        }
        operationRef.current = operation;
        setOperationState(operation);
        if (operation.blocker !== null) {
          setRecoveryError(operation.blocker);
          setNotice(`Recovery stopped: ${describeBlocker(operation.blocker)}`);
          return { kind: 'blocked' };
        }
        setRecoveryError(null);
        return { kind: 'reconciled', operation };
      }
      setRecoveryError(envelope.error);
      setNotice(`Recovery stopped: ${describeBlocker(envelope.error)}`);
    } catch (error) {
      if (mounted.current) setNotice(describeFailure(error));
    }
    return { kind: 'blocked' };
  }, [transport]);

  /** A gap or duplicate reloads authoritative state; it never retries the write. */
  const onProgress = useCallback(
    (hint: Progress) => {
      if (!mounted.current) return;
      setProgressHint(hint);
      const signal: ProgressSignal = stream.current.observe(hint);
      if (signal === 'advance') return;
      setProgressIssue(signal);
      const message =
        signal === 'gap'
          ? 'Progress gap detected; authoritative state reloaded. No write was retried.'
          : 'Duplicate progress detected; authoritative state reloaded. No write was retried.';
      void refresh(message);
      void loadOperation(hint.operationId);
    },
    [refresh, loadOperation],
  );

  const onProgressFault = useCallback(() => {
    if (!mounted.current) return;
    setProgressIssue('fault');
    void refresh('Progress channel lost; authoritative state reloaded. No write was retried.');
    const current = operationRef.current;
    if (current !== null) {
      void loadOperation(current.id);
    }
  }, [refresh, loadOperation]);

  /**
   * Load authoritative status, then settle recorded pending work once.
   *
   * The in-flight ref survives the mount effect's StrictMode cleanup and re-run,
   * so a double-invoked effect cannot start a second recovery. Status is
   * refreshed after both a null result and a reconciliation, but reconciliation
   * is announced only when the refreshed status has nothing pending.
   */
  const settleRecovery = useCallback(
    async (announce: string | null): Promise<void> => {
      if (recoveryInFlight.current) return;
      recoveryInFlight.current = true;
      setBusy(true);
      try {
        const authoritative = await refresh(announce, true);
        if (!mounted.current || authoritative === null) return;
        if (authoritative.pending === null) {
          const target = operationRef.current?.id;
          if (target !== undefined) await loadOperation(target);
          return;
        }
        const outcome = await recoverPending();
        if (!mounted.current) return;
        if (outcome.kind === 'blocked') {
          // A refusal is reported once and nothing is retried; the recorded
          // operation detail is still an authoritative read.
          await loadOperation(authoritative.pending.id);
          return;
        }
        // `none` means the journal had no pending row, so the status this call
        // started from was stale. Reload it without claiming reconciliation.
        const refreshed = await refresh(null, true);
        if (
          !mounted.current ||
          outcome.kind !== 'reconciled' ||
          refreshed === null ||
          refreshed.pending !== null ||
          outcome.operation.blocker !== null
        ) {
          return;
        }
        setNotice('Interrupted work was reconciled; authoritative state reloaded.');
      } finally {
        recoveryInFlight.current = false;
        if (mounted.current) setBusy(false);
      }
    },
    [refresh, recoverPending, loadOperation],
  );

  useEffect(() => {
    void settleRecovery(null);
  }, [settleRecovery]);

  type PerformOptions<T> = {
    apply?: (result: T) => void | Promise<void>;
    reject?: (error: WireError) => void;
    rejectNotice?: (error: WireError) => string;
    refreshOnReject?: (error: WireError) => string;
  };

  async function perform<T>(
    action: () => Promise<Envelope<T> | null>,
    options: PerformOptions<T>,
  ): Promise<void> {
    setBusy(true);
    setNotice(null);
    try {
      const envelope = await action();
      if (!mounted.current || envelope === null) return;
      if (envelope.ok) {
        await options.apply?.(envelope.result);
        return;
      }
      options.reject?.(envelope.error);
      setNotice(
        options.rejectNotice?.(envelope.error) ?? `Not applied: ${describeBlocker(envelope.error)}`,
      );
      if (options.refreshOnReject !== undefined) {
        await refresh(options.refreshOnReject(envelope.error));
      }
    } catch (error) {
      if (!mounted.current) return;
      setNotice(describeFailure(error));
      await refresh('Connection lost; authoritative state reloaded. No write was retried.');
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  const ready = state.kind === 'ready' ? state : null;
  const status = ready?.status ?? null;
  const statusStale = ready?.stale ?? false;
  const blocked = busy || statusStale;
  const deployAvailable = status?.availability.deploy.available === true;
  const restoreAvailable = status?.availability.restore.available === true;
  const deployBlocker =
    status === null || status.availability.deploy.available
      ? null
      : status.availability.deploy.blocker;
  const restoreBlocker =
    status === null || status.availability.restore.available
      ? null
      : status.availability.restore.blocker;
  const effectsBlocker = deployAvailable ? (restoreAvailable ? null : restoreBlocker) : deployBlocker;
  const pending = status?.pending ?? null;

  function applySnapshot(snapshot: Snapshot) {
    setState((previous) =>
      previous.kind === 'ready'
        ? { ...previous, status: { ...previous.status, profile: snapshot } }
        : previous,
    );
  }

  const profileId = status?.profile.id;
  const profileRevision = status?.profile.revision;
  const installationId = status?.installation?.id;
  useEffect(() => {
    setPrepared((current) => {
      if (current === null) return null;
      return current.profileId === profileId && current.expectedRevision === profileRevision
        ? current
        : null;
    });
    setReview((current) => {
      if (current === null) return null;
      return current.profileId === profileId &&
        current.revision === profileRevision &&
        current.installationId === installationId
        ? current
        : null;
    });
  }, [profileId, profileRevision, installationId]);

  const preparedIsCurrent =
    prepared !== null &&
    status !== null &&
    prepared.profileId === status.profile.id &&
    prepared.expectedRevision === status.profile.revision;

  const reviewIsCurrent =
    review !== null &&
    status !== null &&
    review.profileId === status.profile.id &&
    review.revision === status.profile.revision &&
    review.installationId === status.installation?.id;

  const canApplyDeployment =
    !blocked &&
    review !== null &&
    reviewIsCurrent &&
    review.id !== null &&
    review.blockers.length === 0 &&
    deployAvailable &&
    pending === null;

  async function chooseInstallation() {
    await perform(
      async () => {
        const picked = await transport.pickInstallation();
        if (!mounted.current) return null;
        if (!picked.ok) {
          setNotice(describeBlocker(picked.error));
          return null;
        }
        if (picked.result === null) {
          setNotice('Installation selection cancelled.');
          return null;
        }
        return transport.setup(picked.result.selectionId);
      },
      {
        apply: (next) => {
          setState({ kind: 'ready', status: next, stale: false });
          setPrepared(null);
          setReview(null);
          setReviewInvalidated(false);
        },
        rejectNotice: (error) => describeBlocker(error),
      },
    );
  }

  async function previewImport(current: WorkspaceStatus) {
    if (current.availability.import.available !== true) {
      return;
    }
    if (importName.trim().length === 0) {
      setNotice('Enter a mod name before previewing an archive.');
      return;
    }
    setPrepared(null);
    await perform(
      async () => {
        const picked = await transport.pickZip();
        if (!mounted.current) return null;
        if (!picked.ok) {
          setNotice(describeBlocker(picked.error));
          return null;
        }
        if (picked.result === null) {
          setNotice('Archive selection cancelled.');
          return null;
        }
        return transport.importPreview(
          picked.result.selectionId,
          importName.trim(),
          current.profile.revision,
        );
      },
      {
        apply: (preview) => setPrepared(preview),
        rejectNotice: (error) => describeBlocker(error),
      },
    );
  }

  async function applyImport() {
    if (prepared === null || status === null) return;
    if (
      prepared.profileId !== status.profile.id ||
      prepared.expectedRevision !== status.profile.revision
    ) {
      setPrepared(null);
      setNotice('The profile changed; preview the archive again before applying.');
      return;
    }
    await perform(
      () => transport.importApply(prepared.id),
      {
        // The returned snapshot is the profile at commit time. A replay after
        // another client's change would install stale state, so reload the
        // authoritative status instead of trusting it.
        apply: async () => {
          setPrepared(null);
          await refresh('Import applied; authoritative state reloaded.');
        },
        // A plan tied to another revision is dead, as is the staged archive
        // after a stage change; neither may stay behind an enabled Apply.
        reject: (error) => {
          if (error.code === 'StalePlan' || error.code === 'StageChanged') {
            setPrepared(null);
          }
        },
      },
    );
  }

  async function reorder(memberIds: Id[]) {
    if (status === null) return;
    const revision: Revision = status.profile.revision;
    await perform(() => transport.order(revision, memberIds), { apply: applySnapshot });
  }

  async function reviewDeployment() {
    if (status === null) return;
    setReview(null);
    setReviewError(null);
    setReviewInvalidated(false);
    await perform(
      () => transport.deployPreview(status.profile.revision),
      {
        apply: (next) => setReview(next),
        reject: setReviewError,
        rejectNotice: (error) => `Deployment preview blocked: ${describeBlocker(error)}`,
      },
    );
  }

  async function applyDeployment() {
    if (!canApplyDeployment || review === null || review.id === null) return;
    const planId = review.id;
    // Each write gets a fresh delivery baseline; a prior gap or fault must not
    // affect this operation.
    stream.current = new ProgressStream();
    setProgressHint(null);
    setProgressIssue(null);
    setRestoreNoActive(false);
    await perform(
      () =>
        transport.deployApply(planId, onProgress, onProgressFault).catch((error: unknown) => {
          // The write may have landed. A surviving review would re-enable the
          // same plan ID if the refresh reports an unchanged revision.
          setReview(null);
          setReviewInvalidated(true);
          throw error;
        }),
      {
        apply: async (operation) => {
          operationRef.current = operation;
          setOperationState(operation);
          setReview(null);
          setReviewInvalidated(false);
          await refresh(describeDeployOutcome(operation));
        },
        reject: () => {
          setReview(null);
          setReviewInvalidated(true);
        },
        // The write may have reached the game folder before the error. Only the
        // refreshed authoritative status may say what happened; the typed code
        // still identifies a refusal that arrived before any claim.
        rejectNotice: (error) =>
          `Outcome unknown; refreshing authoritative state. ${describeBlocker(error)}`,
        refreshOnReject: (error) =>
          `Outcome unknown; refreshing authoritative state. ${describeBlocker(error)}`,
      },
    );
  }

  async function restoreDeployment() {
    if (!restoreAvailable || blocked || pending !== null) return;
    stream.current = new ProgressStream();
    setRestoreNoActive(false);
    setProgressHint(null);
    setProgressIssue(null);
    await perform(
      () => transport.restore(onProgress, onProgressFault),
      {
        apply: async (result) => {
          if (result.outcome === 'operation') {
            operationRef.current = result.operation;
            setOperationState(result.operation);
          } else {
            setRestoreNoActive(true);
          }
          await refresh();
        },
        reject: (error) => {
          if (error.code === 'NoActiveDeployment') setRestoreNoActive(true);
        },
        // Restore can fail after moving public files. The refresh decides the
        // outcome; the notice must not assert that nothing happened, and the
        // typed refusal stays visible so a pre-claim block cannot vanish.
        rejectNotice: (error) =>
          `Outcome unknown; refreshing authoritative state. ${describeBlocker(error)}`,
        refreshOnReject: (error) =>
          `Outcome unknown; refreshing authoritative state. ${describeBlocker(error)}`,
      },
    );
  }

  async function reconnect() {
    await settleRecovery('Authoritative state reloaded.');
  }

  return (
    <main className="shell">
      <header className="shell__header">
        <div>
          <h1>Marduk</h1>
          <p className="muted">Workspace client</p>
        </div>
        <div className="shell__actions">
          <button type="button" onClick={() => void reconnect()} disabled={busy}>
            Reconnect
          </button>
          <button type="button" onClick={() => void refresh()} disabled={busy}>
            Refresh status
          </button>
        </div>
      </header>

      {notice !== null && (
        <p role="alert" className="notice">
          {notice}
        </p>
      )}

      {state.kind === 'failed' && (
        <section className="panel">
          <h2>Status unavailable</h2>
          <code>{state.message}</code>
          <p className="muted">No workspace state was loaded, so no action is enabled.</p>
        </section>
      )}

      {state.kind === 'loading' && <p className="muted">Loading authoritative status…</p>}

      {status !== null && statusStale && (
        <p className="hint">
          The last status refresh failed, so this is the previous authoritative state. Mutating
          actions stay disabled until a refresh succeeds.
        </p>
      )}

      {status !== null && (
        <>
          <section className="panel">
            <h2>Installation</h2>
            <div className="panel__row">
              <div>
                {status.installation === null ? (
                  <span className="muted">No installation bound.</span>
                ) : (
                  <>
                    <strong>{status.installation.label}</strong>
                    <code className="muted"> installation {status.installation.id}</code>
                  </>
                )}
              </div>
              <button
                type="button"
                onClick={() => void chooseInstallation()}
                disabled={blocked}
              >
                Choose game folder…
              </button>
            </div>
          </section>

          <section className="panel">
            <h2>Operations</h2>
            <AvailabilityRow label="Game target" availability={status.availability.target} />
            <AvailabilityRow label="Import staging" availability={status.availability.import} />
            <AvailabilityRow label="Deploy" availability={status.availability.deploy} />
            <AvailabilityRow label="Restore" availability={status.availability.restore} />
            {deployAvailable && isWindowsPlatform() && (
              <p className="hint" role="note">
                On Windows, writes use standard Windows flushing. An interrupted deploy may leave
                the game folder needing review; Marduk reports what it found rather than claiming
                success.
              </p>
            )}
            {effectsBlocker !== null && (
              <p className="hint">
                Blocked operations stay disabled. <code>{describeBlocker(effectsBlocker)}</code>
              </p>
            )}
          </section>

          <section className="panel">
            <div className="panel__row">
              <h2>Profile {status.profile.id}</h2>
              <code className="muted">revision {status.profile.revision}</code>
            </div>
            <ModList
              members={status.profile.members}
              disabled={blocked}
              onReorder={(ids) => void reorder(ids)}
            />
          </section>

          <section className="panel">
            <h2>Import</h2>
            {status.availability.import.available === true ? (
              <>
                <div className="panel__row">
                  <label className="field">
                    <span>Mod name</span>
                    <input
                      value={importName}
                      onChange={(event) => setImportName(event.target.value)}
                      disabled={blocked}
                    />
                  </label>
                  <button
                    type="button"
                    onClick={() => void previewImport(status)}
                    disabled={blocked}
                  >
                    Preview ZIP…
                  </button>
                </div>
                {prepared !== null && (
                  <div className="prepared">
                    <p>
                      Prepared import <code>{prepared.id}</code> for revision{' '}
                      <code>{prepared.expectedRevision}</code>
                    </p>
                    <ul>
                      {prepared.files.map((target) => (
                        <li key={target.path}>
                          <code>{target.path}</code>
                        </li>
                      ))}
                    </ul>
                    <button
                      type="button"
                      onClick={() => void applyImport()}
                      disabled={blocked || !preparedIsCurrent}
                    >
                      Apply import
                    </button>
                  </div>
                )}
              </>
            ) : (
              <p>
                <span className="badge badge--blocked">Blocked</span>{' '}
                <code>{describeBlocker(status.availability.import.blocker)}</code>
              </p>
            )}
          </section>

          <section className="panel">
            <div className="panel__row">
              <h2>Deployment review</h2>
              <button
                type="button"
                onClick={() => void reviewDeployment()}
                disabled={blocked}
              >
                Review deployment
              </button>
            </div>
            <p className="hint">
              Review the current revision before applying. Apply sends only the reviewed plan ID.
            </p>

            {reviewError !== null && (
              <p>
                <span className="badge badge--blocked">Preview blocked</span>{' '}
                <code>{describeBlocker(reviewError)}</code>
              </p>
            )}

            {review !== null && (
              <div className="review">
                <p>
                  Reviewed plan{' '}
                  {review.id === null ? (
                    <span className="muted">none issued</span>
                  ) : (
                    <code>{review.id}</code>
                  )}{' '}
                  for profile <code>{review.profileId}</code> revision{' '}
                  <code>{review.revision}</code> on <strong>{review.label}</strong>{' '}
                  <code className="muted">installation {review.installationId}</code>
                </p>
                {review.files.length === 0 ? (
                  <p className="muted">The review contains no target files.</p>
                ) : (
                  <ul className="review__files">
                    {review.files.map((file) => (
                      <li key={file.target.path}>
                        <div>
                          <code>{file.target.path}</code>{' '}
                          <span className="muted">({file.target.role})</span>
                        </div>
                        <div className="review__detail">
                          winner <code>{file.winner}</code> · candidates{' '}
                          {file.candidates.length === 0 ? (
                            <span className="muted">none</span>
                          ) : (
                            file.candidates.map((candidate) => (
                              <code key={candidate} className="review__candidate">
                                {candidate}
                              </code>
                            ))
                          )}{' '}
                          ·{' '}
                          {file.needsBackup
                            ? 'original backup required'
                            : 'no original backup needed'}
                        </div>
                      </li>
                    ))}
                  </ul>
                )}
                {review.blockers.length > 0 && (
                  <div className="review__blockers">
                    <p>
                      <span className="badge badge--blocked">Review blockers</span>
                    </p>
                    <ul>
                      {review.blockers.map((blocker, index) => (
                        <li key={`${blocker.code}-${index}`}>
                          <code>{describeBlocker(blocker)}</code>
                        </li>
                      ))}
                    </ul>
                  </div>
                )}
                <div className="panel__row">
                  <button
                    type="button"
                    onClick={() => void applyDeployment()}
                    disabled={!canApplyDeployment}
                  >
                    Apply deployment
                  </button>
                  {deployBlocker !== null && (
                    <span>
                      <span className="badge badge--blocked">Apply unavailable</span>{' '}
                      <code>{describeBlocker(deployBlocker)}</code>
                    </span>
                  )}
                </div>
                {!reviewIsCurrent && (
                  <p className="hint">
                    The profile changed; review the deployment again before applying.
                  </p>
                )}
                {review.blockers.length > 0 && (
                  <p className="hint">
                    A review with blockers cannot be applied. Resolve them and review again.
                  </p>
                )}
              </div>
            )}

            {reviewInvalidated && (
              <p className="hint">
                The review is no longer valid; review the deployment again before applying.
              </p>
            )}

            {review === null && deployBlocker !== null && (
              <p>
                <span className="badge badge--blocked">Apply unavailable</span>{' '}
                <code>{describeBlocker(deployBlocker)}</code>
              </p>
            )}
          </section>

          <section className="panel">
            <div className="panel__row">
              <h2>Restore</h2>
              <button
                type="button"
                onClick={() => void restoreDeployment()}
                disabled={!restoreAvailable || blocked || pending !== null}
              >
                Restore deployment
              </button>
            </div>
            <p className="muted">
              Recorded active generation:{' '}
              {status.activeGenerationId === null ? (
                'none'
              ) : (
                <code>{status.activeGenerationId}</code>
              )}
            </p>
            {restoreBlocker !== null && (
              <p>
                <span className="badge badge--blocked">Restore unavailable</span>{' '}
                <code>{describeBlocker(restoreBlocker)}</code>
              </p>
            )}
            {restoreNoActive && (
              <p role="status" className="restore-note">
                <span className="badge">NoActiveDeployment</span> No deployment is recorded, so
                nothing was restored or changed.
              </p>
            )}
            <p className="hint">
              Restore targets the recorded generation only and is never retried automatically.
            </p>
          </section>

          {pending !== null && <PendingOperation operation={pending} />}

          {(progressHint !== null || operationState !== null || progressIssue !== null) && (
            <section className="panel">
              <h2>Operation progress</h2>
              {progressHint !== null && (
                <p>
                  Last reported hint: <code>{progressHint.operationId}</code> · sequence{' '}
                  <code>{progressHint.sequence}</code> · phase <code>{progressHint.phase}</code>
                </p>
              )}
              {progressIssue !== null && (
                <p>
                  <span className="badge badge--blocked">Progress {progressIssue}</span>{' '}
                  Authoritative state was reloaded; no write was retried.
                </p>
              )}
              {operationState !== null && (
                <OperationSummary operation={operationState} title="Authoritative operation" />
              )}
              <p className="hint">
                Progress is a delivery hint; completion and conflicts come from status. A started
                write cannot be cancelled here.
              </p>
            </section>
          )}

          <section className="panel">
            <h2>Recovery</h2>
            <p className="muted">
              Active generation:{' '}
              {status.activeGenerationId === null ? 'none' : <code>{status.activeGenerationId}</code>}
            </p>
            {status.deployment !== null && (
              <p className="muted">
                Deployment owner: <code>{status.deployment.owner}</code>
              </p>
            )}
            {(status.reconciliation.reclaimed.length > 0 ||
              status.reconciliation.quarantinedCount > 0) && (
              <p role="status">
                <span className="badge">Startup reconciliation</span>{' '}
                {status.reconciliation.reclaimed.length} reclaimed,{' '}
                {status.reconciliation.quarantinedCount} quarantined
                {status.reconciliation.quarantined.length > 0 && (
                  <>
                    :{' '}
                    <code>
                      {status.reconciliation.quarantined
                        .map((leftover) => `${leftover.id} (${leftover.reason})`)
                        .join(', ')}
                    </code>
                  </>
                )}
              </p>
            )}
            {recoveryError !== null && (
              <p role="alert">
                <span className="badge badge--blocked">Recovery refused</span>{' '}
                <code>{describeBlocker(recoveryError)}</code>
              </p>
            )}
            <p className="hint">
              Recorded pending work is reconciled once per status load after a crash or reconnect.
              A refusal is reported with its typed code and never retried automatically. Conflicts
              come from authoritative status; nothing here claims a restore succeeded.
            </p>
          </section>
        </>
      )}
    </main>
  );
}

// The desktop shell runs on Windows and Linux; the user-agent check only picks
// which platform note to show. It is not a capability decision.
function isWindowsPlatform(): boolean {
  return /Windows/i.test(navigator.userAgent);
}

/**
 * An `Ok` envelope carries the recorded operation, not a verdict: a non-null
 * blocker is a refusal, and a non-terminal phase is still running, so neither
 * may be announced as accepted.
 */
function describeDeployOutcome(operation: Operation): string {
  if (operation.blocker !== null) {
    return `Deployment blocked: ${describeBlocker(operation.blocker)}; authoritative state reloaded. No write was retried.`;
  }
  switch (operation.phase) {
    case 'completed':
      return 'Deployment accepted; authoritative state reloaded.';
    case 'rolled_back':
    case 'cancelled':
      return `Deployment ${operation.phase}; authoritative state reloaded. No write was retried.`;
    default:
      return `Deployment still pending (${operation.phase}); authoritative state reloaded.`;
  }
}

function AvailabilityRow({ label, availability }: { label: string; availability: Availability }) {
  return (
    <div className="availability">
      <span>{label}</span>
      {availability.available ? (
        <span className="badge badge--ok">Available</span>
      ) : (
        <span>
          <span className="badge badge--blocked">Blocked</span>{' '}
          <code>{describeBlocker(availability.blocker)}</code>
        </span>
      )}
    </div>
  );
}

function PendingOperation({ operation }: { operation: Operation }) {
  return (
    <section className="panel">
      <h2>Pending operation</h2>
      <OperationSummary operation={operation} title={null} />
      <p className="hint">Recorded work is reported only. Nothing is retried automatically.</p>
    </section>
  );
}

function OperationSummary({
  operation,
  title,
}: {
  operation: Operation;
  title: string | null;
}) {
  return (
    <div className="operation">
      {title !== null && <h3>{title}</h3>}
      <p>
        <code>{operation.id}</code> · {operation.kind} · {operation.phase}
        {operation.generationId !== null && (
          <>
            {' '}
            · generation <code>{operation.generationId}</code>
          </>
        )}
      </p>
      {operation.blocker !== null && (
        <p>
          <span className="badge badge--blocked">Blocked</span>{' '}
          <code>{describeBlocker(operation.blocker)}</code>
        </p>
      )}
      {operation.retained.length > 0 && (
        <ul className="retained">
          {operation.retained.map((entry) => (
            <li key={`${entry.operationId}-${entry.ordinal}-${entry.direction}-${entry.slot}`}>
              retained <code>{entry.operationId}</code> · ordinal {entry.ordinal} ·{' '}
              {entry.direction} · {entry.slot}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

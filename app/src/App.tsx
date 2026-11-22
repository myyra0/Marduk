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
  const [operationState, setOperationState] = useState<Operation | null>(null);
  const [restoreNoActive, setRestoreNoActive] = useState(false);
  const [progressHint, setProgressHint] = useState<Progress | null>(null);
  const [progressIssue, setProgressIssue] = useState<'gap' | 'duplicate' | 'fault' | null>(
    null,
  );
  const mounted = useRef(true);
  const stream = useRef(new ProgressStream());
  const operationRef = useRef<Operation | null>(null);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refresh = useCallback(
    async (announce: string | null = null): Promise<WorkspaceStatus | null> => {
      setBusy(true);
      try {
        const envelope = await transport.status();
        if (!mounted.current) return null;
        if (envelope.ok) {
          setState({ kind: 'ready', status: envelope.result, stale: false });
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
        if (mounted.current) setBusy(false);
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
   * Progress is delivery ordering, not state. A gap or duplicate reloads
   * `status`/`operation`; it never retries the write or claims completion.
   */
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

  useEffect(() => {
    void refresh();
  }, [refresh]);

  async function perform<T>(
    action: () => Promise<Envelope<T>>,
    apply: (result: T) => void,
  ): Promise<void> {
    setBusy(true);
    setNotice(null);
    try {
      const envelope = await action();
      if (!mounted.current) return;
      if (envelope.ok) {
        apply(envelope.result);
      } else {
        // A plan bound to another revision is dead; a stage change invalidates
        // the archive as well. Neither may survive as an enabled Apply.
        if (envelope.error.code === 'StalePlan' || envelope.error.code === 'StageChanged') {
          setPrepared(null);
        }
        setNotice(`Not applied: ${describeBlocker(envelope.error)}`);
      }
    } catch (error) {
      if (mounted.current) {
        setNotice(describeFailure(error));
        void refresh('Connection lost; authoritative state reloaded. No write was retried.');
      }
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
  useEffect(() => {
    setPrepared((current) => {
      if (current === null) return null;
      return current.profileId === profileId && current.expectedRevision === profileRevision
        ? current
        : null;
    });
    setReview((current) => {
      if (current === null) return null;
      return current.profileId === profileId && current.revision === profileRevision
        ? current
        : null;
    });
  }, [profileId, profileRevision]);

  const preparedIsCurrent =
    prepared !== null &&
    status !== null &&
    prepared.profileId === status.profile.id &&
    prepared.expectedRevision === status.profile.revision;

  const reviewIsCurrent =
    review !== null &&
    status !== null &&
    review.profileId === status.profile.id &&
    review.revision === status.profile.revision;

  const canApplyDeployment =
    !blocked &&
    review !== null &&
    reviewIsCurrent &&
    review.id !== null &&
    review.blockers.length === 0 &&
    deployAvailable &&
    pending === null;

  async function chooseInstallation() {
    setBusy(true);
    setNotice(null);
    try {
      const picked = await transport.pickInstallation();
      if (!mounted.current) return;
      if (!picked.ok) {
        setNotice(describeBlocker(picked.error));
        return;
      }
      if (picked.result === null) {
        setNotice('Installation selection cancelled.');
        return;
      }
      const setup = await transport.setup(picked.result.selectionId);
      if (!mounted.current) return;
      if (!setup.ok) {
        setNotice(describeBlocker(setup.error));
        return;
      }
      setState({ kind: 'ready', status: setup.result, stale: false });
      setPrepared(null);
      setReview(null);
      setNotice(null);
    } catch (error) {
      if (mounted.current) setNotice(describeFailure(error));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  async function previewImport(current: WorkspaceStatus) {
    if (current.availability.import.available !== true) {
      return;
    }
    if (importName.trim().length === 0) {
      setNotice('Enter a mod name before previewing an archive.');
      return;
    }
    setBusy(true);
    setNotice(null);
    setPrepared(null);
    try {
      const picked = await transport.pickZip();
      if (!mounted.current) return;
      if (!picked.ok) {
        setNotice(describeBlocker(picked.error));
        return;
      }
      if (picked.result === null) {
        setNotice('Archive selection cancelled.');
        return;
      }
      const preview = await transport.importPreview(
        picked.result.selectionId,
        importName.trim(),
        current.profile.revision,
      );
      if (!mounted.current) return;
      if (!preview.ok) {
        setNotice(describeBlocker(preview.error));
        return;
      }
      setPrepared(preview.result);
      setNotice(null);
    } catch (error) {
      if (mounted.current) setNotice(describeFailure(error));
    } finally {
      if (mounted.current) setBusy(false);
    }
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
      (snapshot) => {
        applySnapshot(snapshot);
        setPrepared(null);
      },
    );
  }

  async function reorder(memberIds: Id[]) {
    if (status === null) return;
    const revision: Revision = status.profile.revision;
    await perform(() => transport.order(revision, memberIds), applySnapshot);
  }

  async function reviewDeployment() {
    if (status === null) return;
    setBusy(true);
    setNotice(null);
    setReview(null);
    setReviewError(null);
    try {
      const envelope = await transport.deployPreview(status.profile.revision);
      if (!mounted.current) return;
      if (envelope.ok) {
        setReview(envelope.result);
      } else {
        setReviewError(envelope.error);
        setNotice(`Deployment preview blocked: ${describeBlocker(envelope.error)}`);
      }
    } catch (error) {
      if (mounted.current) setNotice(describeFailure(error));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  async function applyDeployment() {
    if (!canApplyDeployment || review === null || review.id === null) return;
    const planId = review.id;
    setBusy(true);
    setNotice(null);
    // A new operation starts a new delivery baseline; a prior gap stays visible
    // until this request replaces it.
    setProgressHint(null);
    setProgressIssue(null);
    setRestoreNoActive(false);
    try {
      const envelope = await transport.deployApply(planId, onProgress, onProgressFault);
      if (!mounted.current) return;
      if (envelope.ok) {
        operationRef.current = envelope.result;
        setOperationState(envelope.result);
        setReview(null);
        await refresh('Deployment accepted; authoritative state reloaded.');
      } else {
        if (envelope.error.code === 'StalePlan') {
          setReview(null);
        }
        setNotice(`Not applied: ${describeBlocker(envelope.error)}`);
      }
    } catch {
      if (mounted.current) {
        setNotice('Connection lost; authoritative state reloaded. No write was retried.');
        await refresh('Connection lost; authoritative state reloaded. No write was retried.');
      }
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  async function restoreDeployment() {
    if (!restoreAvailable || blocked || pending !== null) return;
    setBusy(true);
    setNotice(null);
    setRestoreNoActive(false);
    setProgressHint(null);
    setProgressIssue(null);
    try {
      const envelope = await transport.restore(onProgress, onProgressFault);
      if (!mounted.current) return;
      if (envelope.ok) {
        if (envelope.result.outcome === 'operation') {
          operationRef.current = envelope.result.operation;
          setOperationState(envelope.result.operation);
        } else {
          setRestoreNoActive(true);
        }
        await refresh();
      } else if (envelope.error.code === 'NoActiveDeployment') {
        setRestoreNoActive(true);
        await refresh();
      } else {
        setNotice(`Restore not started: ${describeBlocker(envelope.error)}`);
      }
    } catch {
      if (mounted.current) {
        setNotice('Connection lost; authoritative state reloaded. No write was retried.');
        await refresh('Connection lost; authoritative state reloaded. No write was retried.');
      }
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  async function reconnect() {
    setBusy(true);
    setNotice(null);
    try {
      const authoritative = await refresh('Authoritative state reloaded.');
      if (!mounted.current || authoritative === null) return;
      const target = authoritative.pending?.id ?? operationRef.current?.id;
      if (target !== undefined) {
        await loadOperation(target);
      }
    } finally {
      if (mounted.current) setBusy(false);
    }
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
            <p className="hint">
              Blocked operations stay disabled. No native effect or durability port is connected
              here, so Apply and Restore cannot run.
            </p>
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
              Review reads the exact profile revision and shows the winner, target, candidates and
              original-backup need. Apply sends only the reviewed plan ID, and only while
              authoritative availability permits it.
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
              Restore acts only on the recorded generation; a changed configured path cannot
              retarget it. It is never retried automatically.
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
                Progress events are delivery hints only. Completion and conflicts come from
                authoritative status. No Cancel control is offered once a public write may have
                started.
              </p>
            </section>
          )}

          <section className="panel">
            <h2>Recovery</h2>
            <p className="muted">
              Active generation:{' '}
              {status.activeGenerationId === null ? 'none' : <code>{status.activeGenerationId}</code>}
            </p>
            <p className="hint">
              Recovery conflicts are shown from authoritative status; they never claim a restore
              succeeded.
            </p>
          </section>
        </>
      )}
    </main>
  );
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

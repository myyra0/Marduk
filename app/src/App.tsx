import { useCallback, useEffect, useRef, useState } from 'react';

import { ModList } from './components/ModList';
import { describeBlocker, describeFailure } from './format';
import type { WorkspaceTransport } from './transport/client';
import type {
  Availability,
  Envelope,
  Id,
  Operation,
  Prepared,
  Revision,
  Snapshot,
  WorkspaceStatus,
} from './transport/wire';

type StatusState =
  | { kind: 'loading' }
  | { kind: 'ready'; status: WorkspaceStatus }
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
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refresh = useCallback(async () => {
    setBusy(true);
    try {
      const envelope = await transport.status();
      if (!mounted.current) return;
      if (envelope.ok) {
        setState({ kind: 'ready', status: envelope.result });
        setNotice(null);
      } else {
        const message = describeBlocker(envelope.error);
        setState((previous) =>
          previous.kind === 'ready' ? previous : { kind: 'failed', message },
        );
        setNotice(message);
      }
    } catch (error) {
      if (!mounted.current) return;
      const message = describeFailure(error);
      setState((previous) =>
        previous.kind === 'ready' ? previous : { kind: 'failed', message },
      );
      setNotice(message);
    } finally {
      if (mounted.current) setBusy(false);
    }
  }, [transport]);

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
        setNotice(`Not applied: ${describeBlocker(envelope.error)}`);
      }
    } catch (error) {
      if (mounted.current) setNotice(describeFailure(error));
    } finally {
      if (mounted.current) setBusy(false);
    }
  }

  const status = state.kind === 'ready' ? state.status : null;

  function applySnapshot(snapshot: Snapshot) {
    setState((previous) =>
      previous.kind === 'ready'
        ? { kind: 'ready', status: { ...previous.status, profile: snapshot } }
        : previous,
    );
  }

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
      setState({ kind: 'ready', status: setup.result });
      setPrepared(null);
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
    if (prepared === null) return;
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

  return (
    <main className="shell">
      <header className="shell__header">
        <div>
          <h1>Marduk</h1>
          <p className="muted">Workspace client</p>
        </div>
        <button type="button" onClick={() => void refresh()} disabled={busy}>
          Refresh status
        </button>
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
              <button type="button" onClick={() => void chooseInstallation()} disabled={busy}>
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
              Blocked operations stay disabled. Review, Apply and Restore are wired in the
              workspace console.
            </p>
          </section>

          <section className="panel">
            <div className="panel__row">
              <h2>Profile {status.profile.id}</h2>
              <code className="muted">revision {status.profile.revision}</code>
            </div>
            <ModList members={status.profile.members} disabled={busy} onReorder={(ids) => void reorder(ids)} />
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
                      disabled={busy}
                    />
                  </label>
                  <button
                    type="button"
                    onClick={() => void previewImport(status)}
                    disabled={busy}
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
                    <button type="button" onClick={() => void applyImport()} disabled={busy}>
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

          {status.pending !== null && <PendingOperation operation={status.pending} />}

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
      <p>
        <code>{operation.id}</code> · {operation.kind} · {operation.phase}
      </p>
      {operation.blocker !== null && (
        <p>
          <span className="badge badge--blocked">Blocked</span>{' '}
          <code>{describeBlocker(operation.blocker)}</code>
        </p>
      )}
      <p className="hint">Recorded work is reported only. Nothing is retried automatically.</p>
    </section>
  );
}

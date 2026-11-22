import { describe, expect, it, vi } from 'vitest';

import errorStalePlan from './fixtures/error-stale-plan.json';
import preparedFixture from './fixtures/prepared.json';
import snapshotFixture from './fixtures/snapshot.json';
import statusFixture from './fixtures/status.json';
import { createTauriTransport, type InvokeFn, TransportFault } from './client';

function invokeReturning(value: unknown) {
  return vi.fn<InvokeFn>(async () => value);
}

describe('Tauri transport', () => {
  it('loads authoritative status and preserves exact revisions', async () => {
    const invoke = invokeReturning(statusFixture);
    const transport = createTauriTransport(invoke);

    const envelope = await transport.status();

    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('status');
    expect(envelope.ok).toBe(true);
    if (envelope.ok) {
      expect(envelope.result.profile.revision).toBe('9223372036854775807');
      expect(envelope.result.profile.members[0].id).toBe('9007199254740997');
    }
  });

  it('passes only typed IDs and revisions for reorder', async () => {
    const invoke = invokeReturning(snapshotFixture);
    const transport = createTauriTransport(invoke);
    const ids = ['9007199254741001', '9007199254740997'];

    await transport.order('9223372036854775807', ids);

    expect(invoke).toHaveBeenCalledWith('order', {
      atRevision: '9223372036854775807',
      memberIds: ids,
    });
  });

  it('passes opaque selection handles and no paths', async () => {
    const invoke = vi.fn<InvokeFn>(async (command) =>
      command === 'import_preview' ? preparedFixture : statusFixture,
    );
    const transport = createTauriTransport(invoke);

    await transport.setup('7');
    await transport.importPreview('8', 'Textures', '9223372036854775807');

    expect(invoke).toHaveBeenNthCalledWith(1, 'setup', { selectionId: '7' });
    expect(invoke).toHaveBeenNthCalledWith(2, 'import_preview', {
      selectionId: '8',
      name: 'Textures',
      atRevision: '9223372036854775807',
    });
    const setupArgs = invoke.mock.calls[0][1] as Record<string, unknown>;
    expect(Object.keys(setupArgs)).toEqual(['selectionId']);
  });

  it('returns a typed error envelope without throwing', async () => {
    const invoke = invokeReturning(errorStalePlan);
    const transport = createTauriTransport(invoke);

    const envelope = await transport.status();

    expect(envelope).toEqual({ ok: false, error: { code: 'StalePlan' } });
  });

  it('refuses an invalid response and does not retry or write', async () => {
    const broken = {
      ok: true,
      result: {
        ...(statusFixture as any).result,
        profile: {
          ...(statusFixture as any).result.profile,
          revision: 9223372036854775807,
        },
      },
    };
    const invoke = invokeReturning(broken);
    const transport = createTauriTransport(invoke);

    await expect(transport.status()).rejects.toMatchObject({
      name: 'TransportFault',
      kind: 'invalid-response',
    });
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('status');
  });

  it('reports an IPC failure as a fault, never as success', async () => {
    const invoke = vi.fn<InvokeFn>(async () => {
      throw new Error('sensitive native detail: /home/user/game');
    });
    const transport = createTauriTransport(invoke);

    const failure = await transport.restore().catch((error: unknown) => error);

    expect(failure).toBeInstanceOf(TransportFault);
    expect((failure as TransportFault).kind).toBe('ipc');
    expect((failure as TransportFault).message).toBe('restore: request failed');
    expect((failure as TransportFault).message).not.toContain('/home/user/game');
  });
});

import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { describeBlocker } from './format';
import errorStalePlan from './transport/fixtures/error-stale-plan.json';
import statusFixture from './transport/fixtures/status.json';
import { createTauriTransport, type InvokeFn, type WorkspaceTransport } from './transport/client';
import type { Prepared, WorkspaceStatus } from './transport/wire';
import {
  createStubTransport,
  fixturePrepared,
  fixtureStatus,
} from './test/fixtureTransport';

async function renderReady(overrides: Partial<WorkspaceTransport> = {}) {
  const { transport, calls } = createStubTransport(overrides);
  render(<App transport={transport} />);
  await screen.findByText(/revision 9223372036854775807/);
  return { calls };
}

function statusWithImport(): WorkspaceStatus {
  const status = fixtureStatus();
  return {
    ...status,
    availability: { ...status.availability, import: { available: true } },
  };
}

function preparedFor(status: WorkspaceStatus): Prepared {
  return {
    ...fixturePrepared(),
    profileId: status.profile.id,
    expectedRevision: status.profile.revision,
  };
}

describe('workspace shell', () => {
  it('loads authoritative status and shows exact IDs', async () => {
    await renderReady();

    expect(screen.getByText('Skyrim Special Edition')).toBeInTheDocument();
    expect(screen.getByText(/installation 9007199254740993/)).toBeInTheDocument();
    expect(screen.getByText(/id 9007199254740997/)).toBeInTheDocument();
    expect(screen.getByText(/id 9007199254741001/)).toBeInTheDocument();
  });

  it('shows blocked operations with their stable code and detail', async () => {
    await renderReady();

    expect(screen.getAllByText('UnsupportedTarget (Configuration)')).toHaveLength(2);
    expect(screen.getAllByText('StageUnavailable')).toHaveLength(2);
    expect(screen.queryByRole('button', { name: /Preview ZIP/ })).not.toBeInTheDocument();
  });

  it('shows the recorded pending operation and retained recovery state', async () => {
    await renderReady();

    expect(screen.getByText(/Pending operation/)).toBeInTheDocument();
    expect(screen.getByText('RecoveryConflict')).toBeInTheDocument();
    expect(screen.getByText(/ordinal 2/)).toBeInTheDocument();
    expect(screen.getByText(/forward · original/)).toBeInTheDocument();
    expect(screen.getByText(/Nothing is retried automatically/)).toBeInTheDocument();
  });

  it('refuses an invalid status DTO and does not enable any action', async () => {
    const invoke: InvokeFn = async () => ({
      ok: true,
      result: {
        ...(statusFixture as any).result,
        profile: { ...(statusFixture as any).result.profile, revision: '01' },
      },
    });
    render(<App transport={createTauriTransport(invoke)} />);

    expect(await screen.findByText('Status unavailable')).toBeInTheDocument();
    expect(screen.getAllByText(/status: refused an invalid response/).length).toBeGreaterThan(0);
    expect(screen.queryByText('Available')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Choose game folder/ })).not.toBeInTheDocument();
  });

  it('renders a typed status failure as a blocker, not a success', async () => {
    const invoke: InvokeFn = async () => errorStalePlan;
    render(<App transport={createTauriTransport(invoke)} />);

    expect(await screen.findByText('Status unavailable')).toBeInTheDocument();
    expect(screen.getAllByText('StalePlan').length).toBeGreaterThan(0);
  });

  it('submits the exact membership permutation from the keyboard', async () => {
    const user = userEvent.setup();
    const { calls } = await renderReady();
    const list = screen.getByRole('listbox');

    list.focus();
    await user.keyboard('{Alt>}{ArrowDown}{/Alt}');

    await waitFor(() => {
      expect(calls.some((call) => call.command === 'order')).toBe(true);
    });
    const orderCall = calls.find((call) => call.command === 'order');
    expect(orderCall?.args).toEqual({
      atRevision: '9223372036854775807',
      memberIds: ['9007199254741001', '9007199254740997'],
    });
    await waitFor(() => {
      expect(screen.getByText(/revision 43/)).toBeInTheDocument();
    });
    // The selected row kept its ID and moved with the member, not the index.
    expect(list).toHaveAttribute('aria-activedescendant', 'member-9007199254740997');
  });

  it('binds a successful installation pick through its opaque handle', async () => {
    const user = userEvent.setup();
    const pickInstallation = vi.fn(async () => ({
      ok: true as const,
      result: { selectionId: '7', label: 'Skyrim Special Edition' },
    }));
    const setup = vi.fn(async () => ({ ok: true as const, result: fixtureStatus() }));
    await renderReady({ pickInstallation, setup });

    await user.click(screen.getByRole('button', { name: /Choose game folder/ }));

    await waitFor(() => expect(setup).toHaveBeenCalledWith('7'));
    expect(pickInstallation).toHaveBeenCalledTimes(1);
  });

  it('reports a cancelled installation pick without calling setup', async () => {
    const user = userEvent.setup();
    const pickInstallation = vi.fn(async () => ({ ok: true as const, result: null }));
    const setup = vi.fn(async () => ({ ok: true as const, result: fixtureStatus() }));
    await renderReady({ pickInstallation, setup });

    await user.click(screen.getByRole('button', { name: /Choose game folder/ }));

    expect(await screen.findByText('Installation selection cancelled.')).toBeInTheDocument();
    expect(setup).not.toHaveBeenCalled();
  });

  it('shows a typed installation pick failure and does not call setup', async () => {
    const user = userEvent.setup();
    const pickInstallation = vi.fn(async () => ({
      ok: false as const,
      error: { code: 'InvalidPath' as const, detail: { reason: 'MissingParent' as const } },
    }));
    const setup = vi.fn(async () => ({ ok: true as const, result: fixtureStatus() }));
    await renderReady({ pickInstallation, setup });

    await user.click(screen.getByRole('button', { name: /Choose game folder/ }));

    expect(await screen.findByText(/InvalidPath \(MissingParent\)/)).toBeInTheDocument();
    expect(setup).not.toHaveBeenCalled();
  });

  it('previews an archive at the current revision and enables Apply', async () => {
    const user = userEvent.setup();
    const status = statusWithImport();
    const prepared = preparedFor(status);
    const importPreview = vi.fn(async () => ({ ok: true as const, result: prepared }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      importPreview,
    });

    await user.type(screen.getByLabelText('Mod name'), 'Textures');
    await user.click(screen.getByRole('button', { name: /Preview ZIP/ }));

    expect(await screen.findByText(new RegExp(prepared.id))).toBeInTheDocument();
    expect(importPreview).toHaveBeenCalledWith('2', 'Textures', status.profile.revision);
    expect(screen.getByRole('button', { name: /Apply import/ })).toBeEnabled();
  });

  it('drops a prepared import when a reorder changes the profile revision', async () => {
    const user = userEvent.setup();
    const status = statusWithImport();
    const prepared = preparedFor(status);
    const importPreview = vi.fn(async () => ({ ok: true as const, result: prepared }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      importPreview,
    });

    await user.type(screen.getByLabelText('Mod name'), 'Textures');
    await user.click(screen.getByRole('button', { name: /Preview ZIP/ }));
    await screen.findByRole('button', { name: /Apply import/ });

    screen.getByRole('listbox').focus();
    await user.keyboard('{Alt>}{ArrowDown}{/Alt}');

    await screen.findByText(/revision 43/);
    await waitFor(() => {
      expect(screen.queryByRole('button', { name: /Apply import/ })).not.toBeInTheDocument();
    });
  });

  it('clears a prepared import when Apply reports a stale plan', async () => {
    const user = userEvent.setup();
    const status = statusWithImport();
    const prepared = preparedFor(status);
    const importPreview = vi.fn(async () => ({ ok: true as const, result: prepared }));
    const importApply = vi.fn(async () => ({
      ok: false as const,
      error: { code: 'StalePlan' as const },
    }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      importPreview,
      importApply,
    });

    await user.type(screen.getByLabelText('Mod name'), 'Textures');
    await user.click(screen.getByRole('button', { name: /Preview ZIP/ }));
    await user.click(await screen.findByRole('button', { name: /Apply import/ }));

    expect(await screen.findByText(/Not applied: StalePlan/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Apply import/ })).not.toBeInTheDocument();
  });

  it('disables mutating controls when a status refresh fails', async () => {
    const user = userEvent.setup();
    const status = vi.fn<WorkspaceTransport['status']>(async () => ({
      ok: true,
      result: statusWithImport(),
    }));
    await renderReady({ status });

    status.mockResolvedValueOnce({
      ok: false as const,
      error: { code: 'StalePlan' as const },
    });

    await user.click(screen.getByRole('button', { name: /Refresh status/ }));

    expect(await screen.findByText(/previous authoritative state/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Choose game folder/ })).toBeDisabled();
    expect(screen.getByRole('button', { name: /Preview ZIP/ })).toBeDisabled();

    status.mockResolvedValueOnce({ ok: true as const, result: statusWithImport() });
    await user.click(screen.getByRole('button', { name: /Refresh status/ }));
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Choose game folder/ })).toBeEnabled();
    });
  });

  it('formats typed blockers without free-form error text', () => {
    expect(describeBlocker({ code: 'InvalidPath', detail: { reason: 'MissingParent' } })).toBe(
      'InvalidPath (MissingParent)',
    );
    expect(describeBlocker({ code: 'LimitExceeded', detail: { kind: 'Entries' } })).toBe(
      'LimitExceeded (Entries)',
    );
  });
});

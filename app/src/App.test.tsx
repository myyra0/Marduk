import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { describeBlocker } from './format';
import errorStalePlan from './transport/fixtures/error-stale-plan.json';
import operationFixture from './transport/fixtures/operation.json';
import statusFixture from './transport/fixtures/status.json';
import {
  createTauriTransport,
  TransportFault,
  type InvokeFn,
  type WorkspaceTransport,
} from './transport/client';
import type {
  Operation,
  Prepared,
  Preview,
  Progress,
  WorkspaceStatus,
} from './transport/wire';
import {
  createStubTransport,
  fixtureOperation,
  fixturePreview,
  fixturePrepared,
  fixtureRestore,
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

function statusWithEffects(): WorkspaceStatus {
  const status = fixtureStatus();
  return {
    ...status,
    pending: null,
    availability: {
      ...status.availability,
      deploy: { available: true },
      restore: { available: true },
    },
  };
}

function preparedFor(status: WorkspaceStatus): Prepared {
  return {
    ...fixturePrepared(),
    profileId: status.profile.id,
    expectedRevision: status.profile.revision,
  };
}

function reviewFor(status: WorkspaceStatus, overrides: Partial<Preview> = {}): Preview {
  return {
    ...fixturePreview(),
    profileId: status.profile.id,
    revision: status.profile.revision,
    installationId: status.installation?.id ?? fixturePreview().installationId,
    blockers: [],
    ...overrides,
  };
}

function mutations(calls: Array<{ command: string }>): string[] {
  return calls
    .map((call) => call.command)
    .filter((command) => command === 'deploy_apply' || command === 'restore');
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

    expect(screen.getAllByText('UnsupportedTarget (Configuration)').length).toBeGreaterThanOrEqual(
      2,
    );
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

  it('shows a blocked deployment preview with the exact code and keeps writes disabled', async () => {
    const user = userEvent.setup();
    const deployPreview = vi.fn(async () => ({
      ok: false as const,
      error: {
        code: 'UnsupportedTarget' as const,
        detail: { reason: 'Configuration' as const },
      },
    }));
    const deployApply = vi.fn(async () => ({ ok: true as const, result: fixtureOperation() }));
    const restore = vi.fn(async () => ({ ok: true as const, result: fixtureRestore() }));
    const { calls } = await renderReady({ deployPreview, deployApply, restore });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));

    expect(
      await screen.findByText(/Deployment preview blocked: UnsupportedTarget \(Configuration\)/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Apply unavailable/)).toBeInTheDocument();
    expect(screen.getByText(/Restore unavailable/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Apply deployment/ })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Restore deployment/ })).toBeDisabled();

    await user.click(screen.getByRole('button', { name: /Restore deployment/ }));

    expect(deployApply).not.toHaveBeenCalled();
    expect(restore).not.toHaveBeenCalled();
    expect(mutations(calls)).toEqual([]);
    expect(deployPreview).toHaveBeenCalledWith('9223372036854775807');
  });

  it('renders every review field and applies only the reviewed plan ID', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const preview = reviewFor(status);
    const deployPreview = vi.fn(async () => ({ ok: true as const, result: preview }));
    const deployApply = vi.fn(async () => ({
      ok: true as const,
      result: fixtureOperation(),
    }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview,
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));

    expect(await screen.findByText(/Reviewed plan/)).toBeInTheDocument();
    expect(screen.getByText('textures/winner.dds')).toBeInTheDocument();
    expect(screen.getAllByText('9007199254741001').length).toBeGreaterThanOrEqual(1);
    expect(screen.getAllByText('9007199254740997').length).toBeGreaterThanOrEqual(1);
    expect(screen.getByText(/original backup required/)).toBeInTheDocument();

    const apply = screen.getByRole('button', { name: /Apply deployment/ });
    expect(apply).toBeEnabled();
    await user.click(apply);

    await waitFor(() => {
      expect(deployApply).toHaveBeenCalledTimes(1);
    });
    expect(deployApply).toHaveBeenCalledWith(
      preview.id,
      expect.any(Function),
      expect.any(Function),
    );
    expect(deployApply).toHaveBeenCalledTimes(1);
  });

  it('keeps Apply disabled with the typed blocker when availability forbids it', async () => {
    const user = userEvent.setup();
    const status = fixtureStatus();
    const preview = reviewFor(status);
    const deployApply = vi.fn(async () => ({ ok: true as const, result: fixtureOperation() }));
    await renderReady({
      deployPreview: async () => ({ ok: true, result: preview }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));

    const apply = await screen.findByRole('button', { name: /Apply deployment/ });
    expect(apply).toBeDisabled();
    expect(screen.getByText(/Apply unavailable/)).toBeInTheDocument();
    expect(
      screen.getAllByText('UnsupportedTarget (Configuration)').length,
    ).toBeGreaterThanOrEqual(1);
    await user.click(apply);
    expect(deployApply).not.toHaveBeenCalled();
  });

  it('keeps Apply disabled when a pending operation exists', async () => {
    const user = userEvent.setup();
    const status = {
      ...statusWithEffects(),
      pending: { ...fixtureOperation(), phase: 'applying' as const },
    };
    const deployApply = vi.fn(async () => ({ ok: true as const, result: fixtureOperation() }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview: async () => ({ ok: true, result: reviewFor(status) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));

    const apply = await screen.findByRole('button', { name: /Apply deployment/ });
    expect(apply).toBeDisabled();
    await user.click(apply);
    expect(deployApply).not.toHaveBeenCalled();
  });

  it('keeps Apply disabled when the review issued no plan ID', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const deployApply = vi.fn(async () => ({ ok: true as const, result: fixtureOperation() }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview: async () => ({ ok: true, result: reviewFor(status, { id: null }) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));

    const apply = await screen.findByRole('button', { name: /Apply deployment/ });
    expect(apply).toBeDisabled();
    expect(screen.getByText(/none issued/)).toBeInTheDocument();
    await user.click(apply);
    expect(deployApply).not.toHaveBeenCalled();
  });

  it('keeps Apply disabled when the review has blockers', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const deployApply = vi.fn(async () => ({ ok: true as const, result: fixtureOperation() }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview: async () => ({
        ok: true,
        result: reviewFor(status, { blockers: [{ code: 'Busy' }] }),
      }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));

    const apply = await screen.findByRole('button', { name: /Apply deployment/ });
    expect(apply).toBeDisabled();
    expect(screen.getByText(/A review with blockers cannot be applied/)).toBeInTheDocument();
    await user.click(apply);
    expect(deployApply).not.toHaveBeenCalled();
  });

  it('keeps Apply disabled while a status refresh is stale after a valid review', async () => {
    const user = userEvent.setup();
    const status = vi.fn<WorkspaceTransport['status']>();
    status
      .mockResolvedValueOnce({ ok: true as const, result: statusWithEffects() })
      .mockResolvedValueOnce({ ok: false as const, error: { code: 'StalePlan' as const } });
    const deployApply = vi.fn(async () => ({ ok: true as const, result: fixtureOperation() }));
    await renderReady({
      status,
      deployPreview: async () => ({ ok: true, result: reviewFor(statusWithEffects()) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    expect(await screen.findByRole('button', { name: /Apply deployment/ })).toBeEnabled();

    await user.click(screen.getByRole('button', { name: /Refresh status/ }));

    expect(await screen.findByText(/previous authoritative state/)).toBeInTheDocument();
    const apply = screen.getByRole('button', { name: /Apply deployment/ });
    expect(apply).toBeDisabled();
    await user.click(apply);
    expect(deployApply).not.toHaveBeenCalled();
  });

  it('clears the review when the installation root changes', async () => {
    const user = userEvent.setup();
    const current = statusWithEffects();
    const moved = {
      ...current,
      installation: { id: '9007199254740994', label: 'Other installation' },
    };
    const status = vi.fn<WorkspaceTransport['status']>();
    status
      .mockResolvedValueOnce({ ok: true as const, result: current })
      .mockResolvedValueOnce({ ok: true as const, result: moved });
    const deployApply = vi.fn(async () => ({ ok: true as const, result: fixtureOperation() }));
    await renderReady({
      status,
      deployPreview: async () => ({ ok: true, result: reviewFor(current) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    expect(await screen.findByRole('button', { name: /Apply deployment/ })).toBeEnabled();

    await user.click(screen.getByRole('button', { name: /Refresh status/ }));

    await waitFor(() => {
      expect(screen.queryByRole('button', { name: /Apply deployment/ })).not.toBeInTheDocument();
    });
    expect(deployApply).not.toHaveBeenCalled();
  });

  it('requires a new review after the profile revision changes', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const deployPreview = vi.fn(async () => ({
      ok: true as const,
      result: reviewFor(status),
    }));
    const { calls } = await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    await screen.findByRole('button', { name: /Apply deployment/ });

    screen.getByRole('listbox').focus();
    await user.keyboard('{Alt>}{ArrowDown}{/Alt}');

    await screen.findByText(/revision 43/);
    await waitFor(() => {
      expect(screen.queryByRole('button', { name: /Apply deployment/ })).not.toBeInTheDocument();
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    await waitFor(() => {
      expect(deployPreview).toHaveBeenLastCalledWith('43');
    });
    expect(calls.filter((call) => call.command === 'deploy_apply')).toEqual([]);
  });

  it('clears the review and reloads status when Apply reports a stale plan', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const deployApply = vi.fn(async () => ({
      ok: false as const,
      error: { code: 'StalePlan' as const },
    }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview: async () => ({ ok: true, result: reviewFor(status) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    await user.click(await screen.findByRole('button', { name: /Apply deployment/ }));

    expect(await screen.findByText(/Not applied: StalePlan/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Apply deployment/ })).not.toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Review deployment/ })).toBeEnabled();
  });

  it('clears the review and keeps Apply absent when the Apply transport faults', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const deployApply = vi.fn(async () => {
      throw new TransportFault('ipc', 'deploy_apply: request failed');
    });
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview: async () => ({ ok: true, result: reviewFor(status) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    await user.click(await screen.findByRole('button', { name: /Apply deployment/ }));

    expect(
      await screen.findByText(/Connection lost; authoritative state reloaded\. No write was retried\./),
    ).toBeInTheDocument();
    expect(screen.getByText(/review the deployment again before applying/)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Apply deployment/ })).not.toBeInTheDocument();
    expect(deployApply).toHaveBeenCalledTimes(1);
  });

  it('reloads authoritative status and keeps the review-again hint after Apply fails', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const refreshed = {
      ...status,
      pending: { ...fixtureOperation(), phase: 'applying' as const },
    };
    const statusCall = vi.fn<WorkspaceTransport['status']>();
    statusCall
      .mockResolvedValueOnce({ ok: true as const, result: status })
      .mockResolvedValueOnce({ ok: true as const, result: refreshed });
    const deployApply = vi.fn(async () => ({
      ok: false as const,
      error: { code: 'RecoveryConflict' as const },
    }));
    await renderReady({
      status: statusCall,
      deployPreview: async () => ({ ok: true, result: reviewFor(status) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    await user.click(await screen.findByRole('button', { name: /Apply deployment/ }));

    expect(await screen.findByText(/Not applied: RecoveryConflict/)).toBeInTheDocument();
    expect(screen.getByText(/review the deployment again before applying/)).toBeInTheDocument();
    await waitFor(() => expect(statusCall).toHaveBeenCalledTimes(2));
    expect(screen.queryByRole('button', { name: /Apply deployment/ })).not.toBeInTheDocument();
    expect(deployApply).toHaveBeenCalledTimes(1);
  });

  it('reloads authoritative status and operation on a progress gap without retrying', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const applying: Operation = { ...fixtureOperation(), phase: 'applying', blocker: null };
    const deployApply = vi.fn(
      async (
        _planId: string,
        onProgress?: (progress: Progress) => void,
      ) => {
        onProgress?.({ operationId: '9223372036854775803', sequence: '1', phase: 'applying' });
        onProgress?.({ operationId: '9223372036854775803', sequence: '3', phase: 'completed' });
        return { ok: true as const, result: applying };
      },
    );
    const { calls } = await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview: async () => ({ ok: true, result: reviewFor(status) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    await user.click(await screen.findByRole('button', { name: /Apply deployment/ }));

    expect(await screen.findByText(/Progress gap/)).toBeInTheDocument();
    expect(
      calls.filter((call) => call.command === 'operation' && call.args?.operationId === '9223372036854775803')
        .length,
    ).toBeGreaterThan(0);
    expect(deployApply).toHaveBeenCalledTimes(1);
    expect(screen.getByText(/Authoritative operation/)).toBeInTheDocument();
    expect(screen.getAllByText(/no write was retried/i).length).toBeGreaterThanOrEqual(1);
  });

  it('reloads authoritative state on duplicate progress without a second apply', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const deployApply = vi.fn(
      async (
        _planId: string,
        onProgress?: (progress: Progress) => void,
      ) => {
        onProgress?.({ operationId: '9223372036854775803', sequence: '2', phase: 'applying' });
        onProgress?.({ operationId: '9223372036854775803', sequence: '2', phase: 'applying' });
        return { ok: true as const, result: { ...fixtureOperation(), phase: 'applying' as const } };
      },
    );
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview: async () => ({ ok: true, result: reviewFor(status) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    await user.click(await screen.findByRole('button', { name: /Apply deployment/ }));

    expect(await screen.findByText(/Progress duplicate/)).toBeInTheDocument();
    expect(deployApply).toHaveBeenCalledTimes(1);
  });

  it('reloads authoritative state when the progress channel faults', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const deployApply = vi.fn(
      async (
        _planId: string,
        _onProgress?: (progress: Progress) => void,
        onFault?: () => void,
      ) => {
        onFault?.();
        return { ok: true as const, result: { ...fixtureOperation(), phase: 'applying' as const } };
      },
    );
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      deployPreview: async () => ({ ok: true, result: reviewFor(status) }),
      deployApply,
    });

    await user.click(screen.getByRole('button', { name: /Review deployment/ }));
    await user.click(await screen.findByRole('button', { name: /Apply deployment/ }));

    expect(await screen.findByText(/Progress fault/)).toBeInTheDocument();
    expect(deployApply).toHaveBeenCalledTimes(1);
  });

  it('reconnect loads pending, conflict and retained state from status', async () => {
    const user = userEvent.setup();
    const status = vi.fn<WorkspaceTransport['status']>();
    status
      .mockResolvedValueOnce({
        ok: true as const,
        result: { ...fixtureStatus(), pending: null },
      })
      .mockResolvedValue({ ok: true as const, result: fixtureStatus() });
    const operation = vi.fn(async () => ({
      ok: true as const,
      result: fixtureOperation(),
    }));
    await renderReady({ status, operation });

    expect(screen.queryByText(/Pending operation/)).not.toBeInTheDocument();

    await user.click(screen.getByRole('button', { name: /Reconnect/ }));

    expect(await screen.findByText(/Pending operation/)).toBeInTheDocument();
    expect(screen.getByText('RecoveryConflict')).toBeInTheDocument();
    expect(screen.getByText(/ordinal 2/)).toBeInTheDocument();
    await waitFor(() => {
      expect(operation).toHaveBeenCalledWith('9223372036854775803');
    });
    expect(screen.getByText(/Authoritative state reloaded/)).toBeInTheDocument();
  });

  it('shows NoActiveDeployment cleanly and never retries Restore', async () => {
    const user = userEvent.setup();
    const status = { ...statusWithEffects(), activeGenerationId: null };
    const restore = vi.fn(async () => ({ ok: true as const, result: fixtureRestore() }));
    await renderReady({
      status: async () => ({ ok: true, result: status }),
      restore,
    });

    const button = screen.getByRole('button', { name: /Restore deployment/ });
    expect(button).toBeEnabled();
    await user.click(button);

    expect(await screen.findByText('NoActiveDeployment')).toBeInTheDocument();
    expect(screen.getByText(/nothing was restored or changed/i)).toBeInTheDocument();
    expect(restore).toHaveBeenCalledTimes(1);
  });

  it('reloads authoritative status and lets pending disable Restore after it fails', async () => {
    const user = userEvent.setup();
    const status = statusWithEffects();
    const refreshed = {
      ...status,
      pending: { ...fixtureOperation(), phase: 'applying' as const },
    };
    const statusCall = vi.fn<WorkspaceTransport['status']>();
    statusCall
      .mockResolvedValueOnce({ ok: true as const, result: status })
      .mockResolvedValueOnce({ ok: true as const, result: refreshed });
    const restore = vi.fn(async () => ({
      ok: false as const,
      error: { code: 'RecoveryConflict' as const },
    }));
    await renderReady({ status: statusCall, restore });

    const button = screen.getByRole('button', { name: /Restore deployment/ });
    expect(button).toBeEnabled();
    await user.click(button);

    expect(await screen.findByText(/Restore not started: RecoveryConflict/)).toBeInTheDocument();
    await waitFor(() => expect(statusCall).toHaveBeenCalledTimes(2));
    await waitFor(() => {
      expect(screen.getByRole('button', { name: /Restore deployment/ })).toBeDisabled();
    });
    expect(restore).toHaveBeenCalledTimes(1);
  });

  it('formats typed blockers without free-form error text', () => {
    expect(describeBlocker({ code: 'InvalidPath', detail: { reason: 'MissingParent' } })).toBe(
      'InvalidPath (MissingParent)',
    );
    expect(describeBlocker({ code: 'LimitExceeded', detail: { kind: 'Entries' } })).toBe(
      'LimitExceeded (Entries)',
    );
    expect(describeBlocker(operationFixture.result.blocker ?? { code: 'Busy' })).toBe(
      'Busy',
    );
  });
});

import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it } from 'vitest';

import { App } from './App';
import { describeBlocker } from './format';
import errorStalePlan from './transport/fixtures/error-stale-plan.json';
import statusFixture from './transport/fixtures/status.json';
import { createTauriTransport, type InvokeFn } from './transport/client';
import { createStubTransport } from './test/fixtureTransport';

async function renderReady() {
  const { transport, calls } = createStubTransport();
  render(<App transport={transport} />);
  await screen.findByText(/revision 9223372036854775807/);
  return { calls };
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

  it('shows the recorded pending operation without claiming progress', async () => {
    await renderReady();

    expect(screen.getByText(/Pending operation/)).toBeInTheDocument();
    expect(screen.getByText('RecoveryConflict')).toBeInTheDocument();
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

  it('formats typed blockers without free-form error text', () => {
    expect(describeBlocker({ code: 'InvalidPath', detail: { reason: 'MissingParent' } })).toBe(
      'InvalidPath (MissingParent)',
    );
    expect(describeBlocker({ code: 'LimitExceeded', detail: { kind: 'Entries' } })).toBe(
      'LimitExceeded (Entries)',
    );
  });
});

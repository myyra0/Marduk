import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { ModList } from './ModList';
import { fixtureSnapshot } from '../test/fixtureTransport';

const members = fixtureSnapshot().members;
const [first, second] = members;

describe('ModList keyboard order', () => {
  it('selects by membership ID and reorders the exact selected member', async () => {
    const user = userEvent.setup();
    const onReorder = vi.fn();
    const { rerender } = render(<ModList members={members} onReorder={onReorder} />);
    const list = screen.getByRole('listbox');

    list.focus();
    await user.keyboard('{ArrowDown}');
    expect(list).toHaveAttribute('aria-activedescendant', `member-${second.id}`);

    await user.keyboard('{Alt>}{ArrowUp}{/Alt}');
    expect(onReorder).toHaveBeenCalledTimes(1);
    expect(onReorder).toHaveBeenCalledWith([second.id, first.id]);

    rerender(<ModList members={[second, first]} onReorder={onReorder} />);
    const selected = screen.getByRole('option', { name: new RegExp(second.displayName) });
    expect(selected).toHaveAttribute('aria-selected', 'true');
    expect(list).toHaveAttribute('aria-activedescendant', `member-${second.id}`);
  });

  it('focuses the listbox when a row is clicked so Alt+Arrow reorders it', async () => {
    const user = userEvent.setup();
    const onReorder = vi.fn();
    render(<ModList members={members} onReorder={onReorder} />);
    const list = screen.getByRole('listbox');

    await user.click(screen.getByRole('option', { name: new RegExp(first.displayName) }));
    expect(list).toHaveFocus();

    await user.keyboard('{Alt>}{ArrowDown}{/Alt}');
    expect(onReorder).toHaveBeenCalledTimes(1);
    expect(onReorder).toHaveBeenCalledWith([second.id, first.id]);
  });

  it('does not reorder when only the selection moves', async () => {
    const user = userEvent.setup();
    const onReorder = vi.fn();
    render(<ModList members={members} onReorder={onReorder} />);
    const list = screen.getByRole('listbox');

    list.focus();
    await user.keyboard('{ArrowDown}');
    await user.keyboard('{ArrowDown}');
    expect(onReorder).not.toHaveBeenCalled();
    expect(list).toHaveAttribute('aria-activedescendant', `member-${second.id}`);
  });

  it('refuses to move past either end', async () => {
    const user = userEvent.setup();
    const onReorder = vi.fn();
    render(<ModList members={members} onReorder={onReorder} />);

    screen.getByRole('listbox').focus();
    await user.keyboard('{Alt>}{ArrowUp}{/Alt}');
    expect(onReorder).not.toHaveBeenCalled();
  });

  it('does not treat the pattern typed into a text field as a list hotkey', async () => {
    const user = userEvent.setup();
    const onReorder = vi.fn();
    render(
      <>
        <input aria-label="Mod name" />
        <ModList members={members} onReorder={onReorder} />
      </>,
    );

    await user.click(screen.getByLabelText('Mod name'));
    await user.keyboard('{Alt>}{ArrowUp}{/Alt}');

    expect(onReorder).not.toHaveBeenCalled();
  });
});

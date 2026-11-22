import { useEffect, useState } from 'react';

import type { Id, Member } from '../transport/wire';

type Props = {
  members: Member[];
  disabled?: boolean;
  onReorder: (memberIds: Id[]) => void;
};

/**
 * Mod list in the frozen low-to-high order. Rows are keyed and selected by
 * membership ID, never by their visible position.
 *
 * ArrowUp/ArrowDown move the selection; Alt+ArrowUp/Alt+ArrowDown move the
 * selected member one place and submit the exact membership permutation.
 */
export function ModList({ members, disabled = false, onReorder }: Props) {
  const [selectedId, setSelectedId] = useState<Id | null>(null);
  const selected = members.find((member) => member.id === selectedId) ?? members[0];
  const selectedIndex = selected === undefined ? -1 : members.indexOf(selected);

  useEffect(() => {
    if (selectedId !== null && members.some((member) => member.id === selectedId)) {
      return;
    }
    setSelectedId(members[0]?.id ?? null);
  }, [members, selectedId]);

  function selectAt(index: number) {
    const member = members[index];
    if (member !== undefined) {
      setSelectedId(member.id);
    }
  }

  function move(delta: number) {
    if (disabled || selected === undefined || selectedIndex < 0) {
      return;
    }
    const target = selectedIndex + delta;
    if (target < 0 || target >= members.length) {
      return;
    }
    const memberIds = members.map((member) => member.id);
    const moved = memberIds[selectedIndex];
    memberIds[selectedIndex] = memberIds[target];
    memberIds[target] = moved;
    onReorder(memberIds);
  }

  function onKeyDown(event: React.KeyboardEvent<HTMLUListElement>) {
    if (event.key !== 'ArrowUp' && event.key !== 'ArrowDown') {
      return;
    }
    event.preventDefault();
    const delta = event.key === 'ArrowUp' ? -1 : 1;
    if (event.altKey) {
      move(delta);
    } else {
      selectAt(Math.min(Math.max(selectedIndex + delta, 0), members.length - 1));
    }
  }

  return (
    <div className="mod-list">
      <div className="mod-list__legend">
        <span>Low priority</span>
        <span>High priority</span>
      </div>
      <ul
        role="listbox"
        aria-label="Mod order, low priority to high priority"
        aria-activedescendant={selected === undefined ? undefined : `member-${selected.id}`}
        tabIndex={0}
        onKeyDown={onKeyDown}
      >
        {members.map((member, index) => (
          <li
            key={member.id}
            id={`member-${member.id}`}
            role="option"
            aria-selected={member.id === selected?.id}
            className={member.id === selected?.id ? 'mod-row mod-row--selected' : 'mod-row'}
            onClick={() => setSelectedId(member.id)}
          >
            <span className="mod-row__index">{index + 1}</span>
            <span className="mod-row__name">{member.displayName}</span>
            <code className="mod-row__id">id {member.id}</code>
            <span className={member.enabled ? 'mod-row__state' : 'mod-row__state mod-row__state--off'}>
              {member.enabled ? 'enabled' : 'disabled'}
            </span>
          </li>
        ))}
      </ul>
      <p className="hint">
        Arrow keys select. Alt+Arrow reorders the selected member by membership ID.
      </p>
    </div>
  );
}

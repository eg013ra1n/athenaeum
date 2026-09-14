import { useEffect, useMemo, useState } from 'react';

/** Selection is limited to objects currently shown by the tab and filters. */
export function useObjectSelection(visibleIds: number[], disabled: boolean) {
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const visible = useMemo(() => new Set(visibleIds), [visibleIds]);
  const selectedIds = useMemo(
    () => (disabled ? [] : visibleIds.filter(id => selected.has(id))),
    [visibleIds, selected, disabled],
  );
  useEffect(() => {
    setSelected(previous => {
      const next = new Set([...previous].filter(id => !disabled && visible.has(id)));
      return next.size === previous.size ? previous : next;
    });
  }, [visible, disabled]);
  const toggle = (id: number) => {
    if (disabled || !visible.has(id)) return;
    setSelected(previous => {
      const next = new Set(previous);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });
  };
  return {
    selectedIds,
    toggle,
    selectAll: () => {
      if (!disabled) setSelected(new Set(visibleIds));
    },
    clear: () => setSelected(new Set()),
  };
}

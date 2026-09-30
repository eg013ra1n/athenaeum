/**
 * One Escape/Tab owner. Overlays register here; Escape belongs to the top
 * overlay, Tab to the top dialog. A docked panel is non-modal and always sits
 * at the bottom so it never outranks a dialog or popover.
 */
export type OverlayKind = 'dialog' | 'popover' | 'panel';

interface Entry { id: number; kind: OverlayKind }

const stack: Entry[] = [];
let next = 1;

export function pushOverlay(kind: OverlayKind): number {
  const id = next++;
  if (kind === 'panel') {
    const at = stack.findIndex((e) => e.kind !== 'panel');
    stack.splice(at < 0 ? stack.length : at, 0, { id, kind });
  } else {
    stack.push({ id, kind });
  }
  return id;
}

export function popOverlay(id: number): void {
  const i = stack.findIndex((e) => e.id === id);
  if (i >= 0) stack.splice(i, 1);
}

export function isTopOverlay(id: number): boolean {
  return stack[stack.length - 1]?.id === id;
}

export function isTopDialog(id: number): boolean {
  for (let i = stack.length - 1; i >= 0; i--) {
    if (stack[i].kind === 'dialog') return stack[i].id === id;
  }
  return false;
}

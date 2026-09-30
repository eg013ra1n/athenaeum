/** One Escape/Tab owner: overlays register here and only the top one acts on keys. */
const stack: number[] = [];
let next = 1;

export function pushOverlay(): number {
  const id = next++;
  stack.push(id);
  return id;
}

export function popOverlay(id: number): void {
  const i = stack.lastIndexOf(id);
  if (i >= 0) stack.splice(i, 1);
}

export function isTopOverlay(id: number): boolean {
  return stack[stack.length - 1] === id;
}

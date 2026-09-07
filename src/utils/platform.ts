// Platform detection — build-time switch between Tauri desktop and web modes
export const isTauri = import.meta.env.VITE_TARGET !== 'web';

/** True on macOS, in both the desktop shell and the web build. Only drives the
 * spelling of keyboard shortcuts shown to the user (⌘ vs Alt) — never
 * behaviour, so a wrong guess in an exotic user agent costs a tooltip. */
export const isMac =
  typeof navigator !== 'undefined' && /Mac|iPhone|iPad/i.test(navigator.userAgent);

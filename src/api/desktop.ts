// Desktop-only features — Tauri plugins that have no web equivalent.
// In web mode these are safe no-ops or use browser fallbacks.

import { isTauri } from '../utils/platform';

/**
 * Open a URL in the default browser.
 * Tauri: uses plugin-opener. Web: uses window.open.
 */
export async function openUrl(url: string): Promise<void> {
  if (isTauri) {
    const { openUrl: tauriOpenUrl } = await import('@tauri-apps/plugin-opener');
    await tauriOpenUrl(url);
  } else {
    window.open(url, '_blank', 'noopener');
  }
}

/**
 * Reveal a file/directory in the system file explorer.
 * Only works in Tauri desktop mode — no-op in web mode.
 */
export async function revealItemInDir(path: string): Promise<void> {
  if (isTauri) {
    const { revealItemInDir: tauriReveal } = await import('@tauri-apps/plugin-opener');
    await tauriReveal(path);
  }
  // Web: no-op — there's no system file explorer to open
}

/**
 * Open a native directory picker dialog.
 * Only works in Tauri desktop mode — returns null in web mode.
 */
export async function pickDirectory(): Promise<string | null> {
  if (isTauri) {
    const { open } = await import('@tauri-apps/plugin-dialog');
    return open({ directory: true, multiple: false }) as Promise<string | null>;
  }
  return null;
}

/**
 * Open a native file picker dialog for relocating a file.
 * Only works in Tauri desktop mode — returns null in web mode.
 */
export async function pickFile(options?: {
  title?: string;
  filters?: { name: string; extensions: string[] }[];
}): Promise<string | null> {
  if (isTauri) {
    const { open } = await import('@tauri-apps/plugin-dialog');
    return open({
      directory: false,
      multiple: false,
      title: options?.title,
      filters: options?.filters,
    }) as Promise<string | null>;
  }
  return null;
}

// Desktop-only features — Tauri plugins that have no web equivalent.
// In web mode these are safe no-ops or use browser fallbacks.

import { api } from './index';
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
 * Web mode copies the path to the clipboard.
 */
export async function revealItemInDir(path: string): Promise<void> {
  if (isTauri) {
    await api.invoke<boolean>('is_existing_directory', { path });
    const { revealItemInDir: tauriReveal } = await import('@tauri-apps/plugin-opener');
    await tauriReveal(path);
  } else {
    await copyPaths([path]);
  }
}

/**
 * Open a directory directly with the system's default handler —
 * for a folder this opens the folder itself, unlike `revealItemInDir` which
 * reveals an item within its parent's explorer window.
 * Web mode copies the path to the clipboard.
 */
export async function openPath(path: string): Promise<void> {
  if (isTauri) {
    if (!(await api.invoke<boolean>('is_existing_directory', { path })))
      throw new Error('The path is not a directory');
    const { openPath: tauriOpenPath } = await import('@tauri-apps/plugin-opener');
    await tauriOpenPath(path);
  } else {
    await copyPaths([path]);
  }
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

/** Browser clipboard errors propagate so callers can offer selectable path text. */
export async function copyPaths(paths: string[]): Promise<void> {
  if (!navigator.clipboard?.writeText) throw new Error('Clipboard access is unavailable');
  await navigator.clipboard.writeText(paths.join('\n'));
}

// Tauri API implementation — wraps @tauri-apps/api for desktop mode
import { invoke as tauriInvoke } from '@tauri-apps/api/core';
import { listen as tauriListen, type UnlistenFn } from '@tauri-apps/api/event';

export type { UnlistenFn };

export interface ApiBackend {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T>;
  listen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn>;
}

export const tauriApi: ApiBackend = {
  invoke<T>(command: string, args?: Record<string, unknown>): Promise<T> {
    return tauriInvoke<T>(command, args);
  },

  listen<T>(event: string, handler: (payload: T) => void): Promise<UnlistenFn> {
    return tauriListen<T>(event, (e) => handler(e.payload));
  },
};

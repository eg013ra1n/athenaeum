// API abstraction layer — switches between Tauri (desktop) and HTTP (web) backends
//
// All app code should import { api } from '@/api' instead of @tauri-apps/* directly.
// The active backend is selected at build time via VITE_TARGET.

import type { ApiBackend } from './tauri';

export type { ApiBackend };
export type { UnlistenFn } from './tauri';

import { tauriApi } from './tauri';
import { httpApi } from './http';

const isTauri = import.meta.env.VITE_TARGET !== 'web';

export const api: ApiBackend = isTauri ? tauriApi : httpApi;

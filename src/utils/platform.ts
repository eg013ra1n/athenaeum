// Platform detection — build-time switch between Tauri desktop and web modes
export const isTauri = import.meta.env.VITE_TARGET !== 'web';

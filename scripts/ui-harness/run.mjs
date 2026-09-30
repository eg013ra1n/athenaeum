// Starts the harness API and a web-mode Vite on :1430; Ctrl-C stops both.
import { spawn } from 'node:child_process';

const api = spawn(process.execPath, ['scripts/ui-harness/server.mjs'], { stdio: 'inherit', env: process.env });
const vite = spawn('npx', ['vite', '--port', '1430', '--strictPort'], {
  stdio: 'inherit',
  env: { ...process.env, VITE_TARGET: 'web', VITE_API_BASE_URL: 'http://127.0.0.1:8790' },
});
const stop = () => { api.kill(); vite.kill(); process.exit(0); };
process.on('SIGINT', stop);
process.on('SIGTERM', stop);
console.log('[harness] open http://localhost:1430/projects/p-m31');

// Dev-only mock of the web API for the UI harness: POST /api/<command> from
// fixtures.mjs, an idle SSE channel, CORS for the Vite origin. An unknown
// command is logged once and answered with [] (list_*) or null.
import http from 'node:http';
import { buildFixtures } from './fixtures.mjs';

const scenario = process.env.HARNESS_SCENARIO || 'default';
const { handlers } = buildFixtures(scenario);
const unknown = new Set();
const CORS = { 'Access-Control-Allow-Origin': '*', 'Access-Control-Allow-Headers': 'Content-Type, X-API-Key', 'Access-Control-Allow-Methods': 'POST, GET, OPTIONS' };

http.createServer((req, res) => {
  if (req.method === 'OPTIONS') { res.writeHead(204, CORS); res.end(); return; }
  const url = new URL(req.url, 'http://harness');
  if (url.pathname === '/api/events') {
    res.writeHead(200, { ...CORS, 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache', Connection: 'keep-alive' });
    res.write(': harness\n\n');
    const t = setInterval(() => res.write(': ping\n\n'), 15000);
    req.on('close', () => clearInterval(t));
    return;
  }
  const cmd = url.pathname.replace(/^\/api\//, '');
  let body = '';
  req.on('data', (c) => { body += c; });
  req.on('end', () => {
    const args = body ? JSON.parse(body) : {};
    const h = handlers[cmd];
    let out;
    if (h) out = h(args);
    else {
      if (!unknown.has(cmd)) { unknown.add(cmd); console.log(`[harness] unknown command ${cmd} ${body}`); }
      out = /^list_/.test(cmd) ? [] : null;
    }
    res.writeHead(200, { ...CORS, 'Content-Type': 'application/json' });
    res.end(JSON.stringify(out ?? null));
  });
}).listen(8790, '127.0.0.1', () => console.log(`[harness] api on :8790 (scenario ${scenario})`));

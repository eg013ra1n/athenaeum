#!/usr/bin/env node
// Refresh the dev catalog from the production one:
//   npm run dev:db-refresh
//
// Copies <prod>/athenaeum.db into the .dev sibling app-data dir via
// `sqlite3 .backup` (WAL-safe even while the production app runs), wipes the
// identity-bound transfer tables in the copy (the dev tree has its own sync
// identity — inherited outbound rows would be resurrected at startup pointing
// at payload dirs that don't exist there), and symlinks the multi-GB
// catalogs/ dir instead of duplicating it. Never touches sync/, account/,
// or logs/. Requires the sqlite3 CLI (ships with macOS; `apt install sqlite3`
// on Debian).
// Spec: docs/superpowers/specs/2026-08-09-dev-prod-data-separation-design.md

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';

const IDENT = 'com.vsharifov.athenaeum';

// The same 8 tables as sync::store::TRANSFER_TABLES (see schema.rs test
// batch_upgrade_wipes_transfer_tables_once_and_spares_catalog). Order is not
// FK-significant (that store declares no FKs between them); kept stable for diffability.
const TRANSFER_TABLES = [
  'sync_outbound_files',
  'sync_inbound_files',
  'sync_events',
  'sync_receipts',
  'sync_sources',
  'sync_history',
  'sync_outbound',
  'sync_inbound',
];

// Settings precedence is runtime > DB > default, so a copied `account.hub_url`
// row outranks the debug build's test-hub default and would silently point dev
// at the PRODUCTION hub. Deleted in the same transaction as the table wipe.
const HUB_URL_KEY = 'account.hub_url';

function appDataRoot() {
  if (process.platform === 'darwin') return path.join(os.homedir(), 'Library', 'Application Support');
  if (process.platform === 'win32') return process.env.APPDATA;
  return process.env.XDG_DATA_HOME || path.join(os.homedir(), '.local', 'share');
}

function sqlite3(...args) {
  const r = spawnSync('sqlite3', args, { encoding: 'utf8' });
  if (r.error?.code === 'ENOENT') {
    console.error('sqlite3 CLI not found — install it (ships with macOS; `apt install sqlite3` on Debian) and re-run.');
    process.exit(1);
  }
  if (r.status !== 0 || r.error) {
    console.error(`sqlite3 failed (args: ${args.join(' ')}):\n${r.error?.message ?? r.stderr}`);
    process.exit(1);
  }
  return r.stdout;
}

// sqlite3 dot-commands resolve C-style backslash escapes inside double quotes,
// which mangles Windows paths (`\U` dropped, `\a` → BEL). Forward slashes are
// accepted by SQLite on Windows; POSIX paths are passed through untouched.
const q = (p) => (process.platform === 'win32' ? p.replace(/\\/g, '/') : p);

if (process.env.ATHENAEUM_APP_DATA_DIR) {
  console.warn(
    'note: ATHENAEUM_APP_DATA_DIR is set — this script ignores it and always refreshes the platform .dev tree; an app started with that override reads a different directory.',
  );
}

const root = appDataRoot();
if (!root) {
  console.error('could not resolve the app-data root (APPDATA unset?) — set it and re-run.');
  process.exit(1);
}
const prodDir = path.join(root, IDENT);
const devDir = path.join(root, `${IDENT}.dev`);
const prodDb = path.join(prodDir, 'athenaeum.db');
const devDb = path.join(devDir, 'athenaeum.db');

if (!fs.existsSync(prodDb)) {
  console.error(`no production DB at ${prodDb} — nothing to snapshot`);
  process.exit(1);
}
fs.mkdirSync(devDir, { recursive: true });

// A stale WAL/SHM pair from a previous dev run must not shadow the fresh copy.
for (const suffix of ['', '-wal', '-shm']) fs.rmSync(devDb + suffix, { force: true });

sqlite3(prodDb, `.backup "${q(devDb)}"`);
console.log(`snapshot: ${prodDb} -> ${devDb}`);

// A catalog predating the batch model lacks some of these tables; wiping only
// what exists keeps the transaction from aborting and leaving an unwiped copy
// (mirrors the table_present guard in sync::store::wipe_transfer_bookkeeping_on_upgrade).
const present = new Set(
  sqlite3(devDb, "SELECT name FROM sqlite_master WHERE type='table';").split('\n').map((s) => s.trim()),
);
const toWipe = TRANSFER_TABLES.filter((t) => present.has(t));

const wipe = [
  'BEGIN;',
  ...toWipe.map((t) => `DELETE FROM ${t};`),
  `DELETE FROM settings WHERE key = '${HUB_URL_KEY}';`,
  'COMMIT;',
].join(' ');
sqlite3(devDb, wipe);
console.log(`wiped transfer state: ${[...toWipe, `settings:${HUB_URL_KEY}`].join(', ')}`);

const prodCatalogs = path.join(prodDir, 'catalogs');
const devCatalogs = path.join(devDir, 'catalogs');
if (!fs.existsSync(prodCatalogs)) {
  console.log('no prod catalogs/ dir — skipping link');
} else if (fs.lstatSync(devCatalogs, { throwIfNoEntry: false })) {
  console.log('dev catalogs/ already present — leaving as is');
} else if (process.platform === 'win32') {
  console.warn('catalogs symlink skipped on Windows — copy the dir manually if plate-solving is needed in dev');
} else {
  fs.symlinkSync(prodCatalogs, devCatalogs);
  console.log(`linked catalogs: ${devCatalogs} -> ${prodCatalogs}`);
}
console.log('done — dev catalog refreshed (fresh sync identity, signed-out account) — close the dev app before re-running');

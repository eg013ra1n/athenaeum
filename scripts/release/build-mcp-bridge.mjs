#!/usr/bin/env node
// Build the `athenaeum-mcp` bridge for one target triple and put it where Tauri
// looks for a sidecar:
//   node scripts/release/build-mcp-bridge.mjs [--target <triple>] [--debug]
//
// Tauri's `bundle.externalBin: ["binaries/athenaeum-mcp"]` (listed only in the
// overlay crates/athenaeum-tauri/tauri.mcp-bridge.json, passed with `--config`
// by the release pipeline: tauri-build fails every `cargo build/test -p
// athenaeum-tauri` whose listed sidecar file is missing) wants the file
// `crates/athenaeum-tauri/binaries/athenaeum-mcp-<target triple>[.exe]`.
//
// The triple is `--target`, else the `host:` line of `rustc -vV`. A Windows
// triple gets `.exe`. `--debug` builds the debug profile (a local overlay
// check); the pipeline builds the release profile. The built file is found from
// cargo's own JSON messages, so a CARGO_TARGET_DIR or a configured target-dir is
// honoured. Exits non-zero with a message when cargo fails.
// Spec: docs/superpowers/plans/2026-11-19-athenaeum-mcp-v1-plan.md (D2).

import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..', '..');

function fail(message) {
  console.error(`build-mcp-bridge: ${message}`);
  process.exit(1);
}

function parseArgs(argv) {
  const out = { target: null, debug: false };
  for (let i = 0; i < argv.length; i++) {
    const a = argv[i];
    if (a === '--debug') {
      out.debug = true;
    } else if (a === '--target') {
      const v = argv[++i];
      if (!v || v.startsWith('--')) fail('--target needs a target triple');
      out.target = v;
    } else if (a.startsWith('--target=')) {
      out.target = a.slice('--target='.length);
      if (!out.target) fail('--target needs a target triple');
    } else {
      fail(`unknown argument '${a}' (usage: [--target <triple>] [--debug])`);
    }
  }
  return out;
}

function hostTriple() {
  const r = spawnSync('rustc', ['-vV'], { encoding: 'utf8' });
  if (r.error || r.status !== 0) {
    fail(`could not run 'rustc -vV' to learn the host triple: ${r.error ?? r.stderr}`);
  }
  const line = r.stdout.split('\n').find((l) => l.startsWith('host: '));
  if (!line) fail("'rustc -vV' printed no 'host:' line");
  return line.slice('host: '.length).trim();
}

const args = parseArgs(process.argv.slice(2));
const triple = args.target ?? hostTriple();
const exe = triple.includes('windows') ? '.exe' : '';

// --target only when asked for: without it cargo writes to target/<profile>,
// where the host build of the app already is.
const cargoArgs = ['build', '-p', 'athenaeum-mcp', '--message-format=json-render-diagnostics'];
if (!args.debug) cargoArgs.push('--release');
if (args.target) cargoArgs.push('--target', args.target);

console.error(`build-mcp-bridge: cargo ${cargoArgs.slice(0, -1).join(' ')} (${triple})`);
const build = spawnSync('cargo', cargoArgs, {
  cwd: root,
  encoding: 'utf8',
  maxBuffer: 256 * 1024 * 1024,
  stdio: ['ignore', 'pipe', 'inherit'],
});
if (build.error) fail(`could not run cargo: ${build.error.message}`);
if (build.status !== 0) fail(`cargo build -p athenaeum-mcp failed (exit ${build.status})`);

let built = null;
for (const line of build.stdout.split('\n')) {
  if (!line.startsWith('{')) continue;
  let msg;
  try {
    msg = JSON.parse(line);
  } catch {
    continue;
  }
  if (
    msg.reason === 'compiler-artifact' &&
    msg.target?.name === 'athenaeum-mcp' &&
    msg.target.kind?.includes('bin') &&
    msg.executable
  ) {
    built = msg.executable;
  }
}
if (!built || !fs.existsSync(built)) fail('cargo built no athenaeum-mcp executable');

const destDir = path.join(root, 'crates', 'athenaeum-tauri', 'binaries');
const dest = path.join(destDir, `athenaeum-mcp-${triple}${exe}`);
fs.mkdirSync(destDir, { recursive: true });
fs.copyFileSync(built, dest);
fs.chmodSync(dest, 0o755);
console.error(`build-mcp-bridge: ${path.relative(root, dest)} (${fs.statSync(dest).size} bytes)`);

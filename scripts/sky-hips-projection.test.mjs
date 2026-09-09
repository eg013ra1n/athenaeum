import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import ts from 'typescript';

// Compare the bridge against the actual bundled D3 projection, not a second
// implementation of its scale formula. Transpile with the project's compiler
// so this check also runs on supported Node versions without native TS loading.
const source = readFileSync(new URL('../src/hips/view.ts', import.meta.url), 'utf8');
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ES2022 },
}).outputText;
const { stereographicFov, isHipsView } = await import(
  `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
);
const context = {};
vm.runInNewContext(
  readFileSync(new URL('../public/lib/d3.min.js', import.meta.url), 'utf8'),
  context,
);

test('HiPS horizontal FOV matches the bundled stereographic projection', () => {
  for (const width of [320, 1200, 1441]) {
    for (const scale of [100, 703.125, 5000, 100000]) {
      const projection = context.d3.geo
        .stereographic()
        .scale(scale)
        .translate([width / 2, 400]);
      const edge = projection.invert([width, 400]);
      // At the equator with no roll, twice the edge longitude is the full FOV.
      assert.ok(Math.abs(stereographicFov(width, scale) - 2 * Math.abs(edge[0])) < 1e-9);
    }
  }
});

test('reject invalid geometry and cross-document payloads', () => {
  for (const scale of [0, -1, NaN, Infinity]) assert.throws(() => stereographicFov(1200, scale));
  for (const width of [0, -1, NaN, Infinity]) assert.throws(() => stereographicFov(width, 500));
  assert.equal(isHipsView({ ra: 359, dec: -80, rotation: 35, scale: 703 }), true);
  for (const value of [
    null,
    {},
    { ra: 0, dec: 91, rotation: 0, scale: 1 },
    { ra: '0', dec: 0, rotation: 0, scale: 1 },
  ]) {
    assert.equal(isHipsView(value), false);
  }
});

import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
const require = createRequire(import.meta.url);
const exports = {};
const source = readFileSync(
  new URL('../src/components/observing/ObservingProgressTable.tsx', import.meta.url),
  'utf8',
);
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.CommonJS,
    target: ts.ScriptTarget.ES2022,
    jsx: ts.JsxEmit.ReactJSX,
  },
}).outputText;
vm.runInNewContext(compiled, { exports, require });
const goal = {
  frameSetId: 1,
  filter: 'H',
  revision: 1,
  targetSeconds: 3600,
  requireAnalysis: true,
  maxFwhmPx: 4,
  maxEccentricity: 0.6,
  rejectTrailed: true,
};
function render(rows) {
  return renderToStaticMarkup(
    React.createElement(exports.ObservingProgressTable, {
      rows,
      busy: false,
      onEdit: () => {
        throw Error('must not save implicitly');
      },
      onRemove: () => {
        throw Error('must not remove implicitly');
      },
    }),
  );
}
test('no goal means no fabricated completion percentage', () => {
  const html = render([
    { filter: 'G', goal: null, accepted: 2, rejected: 0, unknown: 1, acceptedSeconds: 600 },
  ]);
  assert.match(html, /Not set/);
  assert.doesNotMatch(html, /%/);
  assert.match(html, /2 \/ 0 \/ 1/);
});
test('empty-filter observations remain zero and surplus stays visible', () => {
  const html = render([
    { filter: 'H', goal, accepted: 0, rejected: 0, unknown: 0, acceptedSeconds: 0 },
    {
      filter: 'G',
      goal: { ...goal, filter: 'G' },
      accepted: 15,
      rejected: 2,
      unknown: 3,
      acceptedSeconds: 4500,
    },
  ]);
  assert.match(html, /0.0%/);
  assert.match(html, /125.0%/);
  assert.match(html, /FWHM ≤ 4 px/);
  assert.match(html, /15 \/ 2 \/ 3/);
});

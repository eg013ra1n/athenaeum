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
  new URL('../src/components/equipment/EquipmentEvidenceTable.tsx', import.meta.url),
  'utf8',
);
const compiled = ts.transpileModule(source, {
  compilerOptions: {
    module: ts.ModuleKind.CommonJS,
    target: ts.ScriptTarget.ES2022,
    jsx: ts.JsxEmit.ReactJSX,
  },
}).outputText;
vm.runInNewContext(compiled, {
  exports,
  require: module => {
    if (module === '../FileLocationActions') return { FileLocationActions: () => null };
    if (module === '../../utils/dateFormatting') return { formatTimestamp: s => s };
    return require(module);
  },
});
const row = {
  frameId: 9,
  filename: 'fixture.fits',
  path: '/synthetic/fixture.fits',
  camera: 'Synthetic',
  binningX: 1,
  binningY: 1,
  solvedScale: 1.9,
  solvedAt: 'fixture-time',
  confirmedProfileId: null,
  confirmationStale: false,
  candidates: [1, 2].map(id => ({
    profile: { id, name: 'Profile ' + id },
    expectedScale: 1.9,
    differencePercent: 0,
  })),
};
function render(value) {
  return renderToStaticMarkup(
    React.createElement(exports.EquipmentEvidenceTable, {
      rows: [value],
      busy: false,
      onConfirm: () => {
        throw Error('must not confirm on render');
      },
      onClear: () => {
        throw Error('must not clear on render');
      },
    }),
  );
}
test('ambiguous equal-scale candidates both require explicit confirmation', () => {
  const html = render(row);
  assert.equal((html.match(/Confirm match/g) || []).length, 2);
  assert.match(html, /Unconfirmed/);
});
test('changed evidence is visibly stale and unknown binning has no misleading match', () => {
  assert.match(render({ ...row, confirmedProfileId: 1, confirmationStale: true }), /Needs review/);
  const html = render({ ...row, binningX: null, binningY: null, candidates: [] });
  assert.match(html, /No compatible profile/);
  assert.doesNotMatch(html, /Confirm match/);
});

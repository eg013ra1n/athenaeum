import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
const require = createRequire(import.meta.url),
  exports = {};
const source = readFileSync(
  new URL('../src/components/folders/FolderTypeBreakdown.tsx', import.meta.url),
  'utf8',
);
vm.runInNewContext(
  ts.transpileModule(source, {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText,
  {
    exports,
    require: name => {
      if (name === '../../api') return { api: {} };
      if (name === '../Toolbar') return { ToolbarButton: 'button' };
      return require(name);
    },
  },
);
test('table exposes direct, recursive and child counts without navigating implicitly', () => {
  const direct = {
    total: 1,
    lights: 1,
    darks: 0,
    flats: 0,
    bias: 0,
    darkFlats: 0,
    masters: 0,
    unknown: 0,
  };
  const recursive = { ...direct, total: 3, darks: 1, bias: 1 };
  const html = renderToStaticMarkup(
    React.createElement(exports.FolderCountsTable, {
      data: {
        direct,
        recursive,
        children: [
          { path: '/fixture/night', counts: { ...direct, total: 2, lights: 0, darks: 1, bias: 1 } },
        ],
      },
      onFolder() {
        throw Error('No implicit navigation');
      },
    }),
  );
  for (const label of [
    'This folder only',
    'Including subfolders',
    'Lights',
    'Darks',
    'Flats',
    'Bias',
    'Dark flats',
    'Masters',
    'Unknown',
    'night /',
  ])
    assert.ok(html.includes(label));
  assert.ok(html.includes('title="/fixture/night"'));
  assert.equal((html.match(/<tr/g) || []).length, 4);
});

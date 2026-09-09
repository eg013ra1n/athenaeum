import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import ts from 'typescript';
import { createRequire } from 'node:module';
import React from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
const require = createRequire(import.meta.url);

function compile(file) {
  return ts.transpileModule(readFileSync(new URL(file, import.meta.url), 'utf8'), {
    compilerOptions: {
      module: ts.ModuleKind.CommonJS,
      target: ts.ScriptTarget.ES2022,
      jsx: ts.JsxEmit.ReactJSX,
    },
  }).outputText;
}
const locationExports = {};
vm.runInNewContext(compile('../src/utils/fileLocations.ts'), { exports: locationExports });
const { containingFolder, containingFolders } = locationExports;

test('actual containing folders preserve sibling, Unicode, case and special characters', () => {
  const paths = [
    '/Volumes/A/M31/a.fits',
    '/Volumes/A/M31/b.xisf',
    '/Volumes/B/M31/c.fits',
    '/Volumes/A/M31-extra/d.fits',
    '/星空/50%_#/x.fits',
  ];
  assert.deepEqual(
    [...containingFolders(paths)],
    ['/Volumes/A/M31', '/Volumes/B/M31', '/Volumes/A/M31-extra', '/星空/50%_#'],
  );
  assert.equal(containingFolder('/file.fits'), '/');
  assert.equal(containingFolder('local.fits'), '.');
  assert.equal(containingFolder('/literal\\name/file.fits'), '/literal\\name');
});

test('Windows drive and UNC paths retain valid roots', () => {
  assert.equal(containingFolder('C:\\file.fits'), 'C:\\');
  assert.equal(containingFolder('C:/file.fits'), 'C:/');
  assert.equal(containingFolder('C:\\night\\file.fits'), 'C:\\night');
  assert.equal(containingFolder('\\\\server\\share\\file.fits'), '\\\\server\\share');
});

function desktopFixture(native, { access = true, clipboardError = false } = {}) {
  const calls = [];
  const exports = {};
  const opener = {
    revealItemInDir: async path => calls.push(['reveal', path]),
    openPath: async path => calls.push(['open', path]),
  };
  vm.runInNewContext(compile('../src/api/desktop.ts'), {
    exports,
    navigator: {
      clipboard: {
        writeText: async text => {
          if (clipboardError) throw Error('clipboard denied');
          calls.push(['copy', text]);
        },
      },
    },
    require: name => {
      if (name === '../utils/platform') return { isTauri: native };
      if (name === './index')
        return {
          api: {
            invoke: async (command, args) => {
              calls.push([command, args.path]);
              if (access === 'missing') throw Error('offline');
              return access;
            },
          },
        };
      if (name === '@tauri-apps/plugin-opener') return opener;
      throw Error('Unexpected import ' + name);
    },
  });
  return { ...exports, calls };
}

test('web fallback copies exact paths and propagates clipboard denial', async () => {
  const api = desktopFixture(false);
  await api.revealItemInDir('/server/a.fits');
  await api.openPath('/server');
  assert.deepEqual(api.calls, [
    ['copy', '/server/a.fits'],
    ['copy', '/server'],
  ]);
  await assert.rejects(
    desktopFixture(false, { clipboardError: true }).revealItemInDir('/server/a.fits'),
    /clipboard denied/,
  );
});

test('desktop preflight prevents reveal on missing paths and opening files as folders', async () => {
  const missing = desktopFixture(true, { access: 'missing' });
  await assert.rejects(missing.revealItemInDir('/offline/a.fits'), /offline/);
  assert.equal(missing.calls.length, 1);
  const file = desktopFixture(true, { access: false });
  await assert.rejects(file.openPath('/a.fits'), /not a directory/);
  await file.revealItemInDir('/a.fits');
  assert.deepEqual(file.calls.at(-1), ['reveal', '/a.fits']);
  const folder = desktopFixture(true);
  await folder.openPath('/night');
  assert.deepEqual(folder.calls.at(-1), ['open', '/night']);
});

test('night, camera and filter controls receive their complete distinct memberships', () => {
  for (const merged of [false, true]) {
    const name = merged ? 'MergedCameraFilterTree' : 'CameraFilterTree';
    const exports = {};
    const locations = [];
    vm.runInNewContext(compile(`../src/components/calibration/${name}.tsx`), {
      exports,
      require: module =>
        module === '../FileLocationActions'
          ? {
              FileLocationActions: props => {
                locations.push(props);
                return null;
              },
            }
          : require(module),
    });
    const camera = {
      camera: 'Fixture camera',
      totalFrameCount: 3,
      filters: [
        { key: 'a', label: 'Ha', frameCount: 2 },
        { key: 'b', label: 'OIII', frameCount: 1 },
      ],
    };
    renderToStaticMarkup(
      React.createElement(exports[name], {
        nodes: merged
          ? [camera]
          : [
              {
                dateKey: '2026-01-01',
                dateDisplay: 'Jan 1',
                totalFrameCount: 3,
                cameras: [camera],
              },
            ],
        checkedKeys: new Set(),
        onCheckedChange: () => {},
        locationPathsByKey: new Map([
          ['a', ['/night-a/1.fits', '/night-b/2.fits']],
          ['b', ['/night-a/3.fits']],
        ]),
      }),
    );
    assert.equal(locations.length, merged ? 3 : 4);
    assert.equal(locations.find(p => p.label === 'Camera file locations').paths.length, 3);
    assert.equal(
      locations.filter(p => p.label === 'Filter file locations')[0].paths[1],
      '/night-b/2.fits',
    );
    if (!merged)
      assert.equal(locations.find(p => p.label === 'Night file locations').paths.length, 3);
  }
});

test('object locations resolve every selected object and preserve offline and ZIP paths', async () => {
  const exports = {};
  const calls = [];
  vm.runInNewContext(compile('../src/components/ObjectFileLocations.tsx'), {
    exports,
    require: module => {
      if (module === '../api')
        return {
          api: {
            invoke: async (command, args) => {
              calls.push([command, args.framesSetId]);
              return {
                nights: [
                  {
                    sessions: [
                      {
                        frames: [
                          { file: { path: '/offline/' + args.framesSetId + '.fits' } },
                          {
                            file: {
                              path: '/old/source.fits',
                              archive_zip_path: '/archive/lights.zip',
                            },
                          },
                        ],
                      },
                    ],
                  },
                ],
              };
            },
          },
        };
      if (module === './FileLocationActions') return { FileLocationActions: () => null };
      return require(module);
    },
  });
  const element = exports.ObjectFileLocations({ ids: [1, 2] });
  const paths = await element.props.loadPaths();
  assert.deepEqual(
    [...paths],
    ['/offline/1.fits', '/archive/lights.zip', '/offline/2.fits', '/archive/lights.zip'],
  );
  assert.deepEqual(calls, [
    ['get_frame_set_detail', 1],
    ['get_frame_set_detail', 2],
  ]);
});

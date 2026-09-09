import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import vm from 'node:vm';
import test from 'node:test';
import ts from 'typescript';
const source = readFileSync(new URL('../src/utils/skyFootprint.ts', import.meta.url), 'utf8');
const compiled = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ES2022 },
}).outputText;
const { initializeSkyFootprint, projectSkyFootprint } = await import(
  `data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`
);
const ctx = {};
vm.runInNewContext(readFileSync(new URL('../public/lib/d3.min.js', import.meta.url), 'utf8'), ctx);
function group() {
  const node = { children: [] };
  return {
    node: () => node,
    append() {
      const child = { attrs: {}, styles: {} };
      node.children.push(child);
      return {
        attr(k, v) {
          child.attrs[k] = v;
          return this;
        },
        style(k, v) {
          child.styles[k] = v;
          return this;
        },
      };
    },
  };
}
test('initially clipped footprint retains paths and recovers after panning', () => {
  const corners = [
      [119, -1],
      [121, -1],
      [121, 1],
      [119, 1],
    ],
    g = group();
  initializeSkyFootprint(g, corners);
  let center = [0, 0];
  const project = ctx.d3.geo.stereographic().scale(600).translate([600, 400]);
  const clip = p => ctx.d3.geo.distance(center, p) < Math.PI / 2;
  assert.equal(projectSkyFootprint(g.node().__fovCorners, project, clip, 1, 1), null);
  assert.deepEqual(
    g.node().children.map(c => c.attrs.class),
    ['fov-hit', 'fov-rect'],
  );
  center = [120, 0];
  project.rotate([-120, 0]);
  assert.match(projectSkyFootprint(g.node().__fovCorners, project, clip, 1, 1), /^M.* Z$/);
  center = [0, 0];
  assert.equal(projectSkyFootprint(corners, project, clip, 1, 1), null);
  center = [120, 0];
  assert.ok(projectSkyFootprint(corners, project, clip, 1, 1));
});
test('valid wide or rotated footprints survive zoom beyond half a viewport', () => {
  const corners = [
    [-5, -1],
    [5, -1],
    [5, 1],
    [-5, 1],
  ];
  for (const scale of [600, 20000])
    for (const roll of [0, 90]) {
      const project = ctx.d3.geo
        .stereographic()
        .scale(scale)
        .translate([600, 400])
        .rotate([0, 0, roll]);
      assert.ok(projectSkyFootprint(corners, project, () => true, 1, 1));
    }
});
test('invalid projections and scaling hide without destroying retained geometry', () => {
  const corners = [
    [0, 0],
    [1, 0],
    [1, 1],
    [0, 1],
  ];
  const g = group();
  initializeSkyFootprint(g, corners);
  assert.equal(
    projectSkyFootprint(
      corners,
      () => [NaN, 0],
      () => true,
      1,
      1,
    ),
    null,
  );
  assert.equal(
    projectSkyFootprint(
      corners,
      p => p,
      () => true,
      0,
      1,
    ),
    null,
  );
  assert.equal(g.node().__fovCorners, corners);
  assert.equal(g.node().children.length, 2);
});

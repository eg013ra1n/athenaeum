import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';
const require = createRequire(import.meta.url);
const source = readFileSync(
  new URL('../src/components/ExposureVersionReview.tsx', import.meta.url),
  'utf8',
);
function text(node) {
  if (node == null) return '';
  if (typeof node !== 'object') return String(node);
  if (Array.isArray(node)) return node.map(text).join('');
  return text(node.props?.children);
}
function find(node, label) {
  if (!node || typeof node !== 'object') return null;
  if (node.props?.onClick && text(node) === label) return node;
  for (const child of [node.props?.children].flat(Infinity)) {
    const match = find(child, label);
    if (match) return match;
  }
  return null;
}
function harness(missing = false) {
  const versions = Array.from({ length: 505 }, (_, id) => ({
    frameId: id + 1,
    filename: `frame-${id + 1}.fits`,
    path: '/fixture',
    classification: { stage: id % 2 ? 'calibrated' : 'unknown', evidence: [] },
  }));
  const states = [
      { versions, exposureCount: 505, exposureSeconds: 0, suggestions: [], suggestionCount: 0 },
    ],
    calls = [];
  let index = 0;
  const exports = {};
  const hooks = {
    useState: initial => {
      const id = index++;
      if (!(id in states)) states[id] = initial;
      return [
        states[id],
        value => (states[id] = typeof value === 'function' ? value(states[id]) : value),
      ];
    },
    useRef: () => ({ current: null }),
    useEffect() {},
    useCallback: fn => fn,
    lazy: () => () => null,
    Suspense: () => null,
  };
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
      console: { error() {} },
      require: name => {
        if (name === 'react') return hooks;
        if (name === '../api')
          return {
            api: {
              invoke: async (command, args) => {
                calls.push(args.frameIds);
                return args.frameIds
                  .filter(id => !missing || id !== 1)
                  .toReversed()
                  .map(id => ({ frame: { id }, file: { id } }));
              },
            },
          };
        if (name === '../contexts/NotificationContext')
          return { useNotifications: () => ({ notify() {} }) };
        if (name === './Toolbar') return { ToolbarButton: 'button' };
        if (name === '../utils/dateFormatting') return { formatTimestamp: s => s };
        return require(name);
      },
    },
  );
  return {
    states,
    calls,
    render: () => {
      index = 0;
      return exports.ExposureVersionReview({ objectIds: [1], onClose() {}, onChanged() {} });
    },
  };
}
test('Blink spans all pages, preserves order and starts on clicked file', async () => {
  const h = harness();
  await find(h.render(), 'Blink all (505)').props.onClick();
  assert.deepEqual(
    h.calls.map(ids => ids.length),
    [400, 105],
  );
  assert.equal(h.states[4].frames.length, 505);
  assert.equal(h.states[4].frames[0].frame.id, 1);
  assert.equal(h.states[4].frames[504].frame.id, 505);
  h.states[4] = null;
  h.states[3] = 1;
  await find(h.render(), 'frame-51.fits').props.onClick();
  assert.equal(h.states[4].index, 50);
});
test('Unknown Blink contains only Unknown and missing metadata fails visibly', async () => {
  const h = harness();
  await find(h.render(), 'Blink Unknown (253)').props.onClick();
  assert.equal(h.states[4].frames.length, 253);
  assert.ok(h.states[4].frames.every(f => f.frame.id % 2 === 1));
  const absent = harness(true);
  await find(absent.render(), 'Blink all (505)').props.onClick();
  assert.equal(absent.states[4], null);
  assert.match(absent.states[2], /no longer in the catalog/);
});

test('likely raw assessment is visible but never automatically assigned', () => {
  const h = harness();
  h.states[0].versions[0].assessment = {
    label: 'Likely uncalibrated',
    candidateStage: 'raw',
    confidence: 'heuristic, not confirmed',
    evidence: ['Filename timestamp agrees'],
  };
  const tree = h.render();
  assert.match(text(tree), /Likely uncalibrated/);
  assert.ok(find(tree, 'Confirm Raw'));
  assert.equal(h.calls.length, 0);
  assert.equal(h.states[0].versions[0].classification.stage, 'unknown');
});

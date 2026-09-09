import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { createRequire } from 'node:module';
import vm from 'node:vm';
import ts from 'typescript';
const require = createRequire(import.meta.url);
function find(node, text) {
  if (!node || typeof node !== 'object') return null;
  if (node.type === 'button' && node.props.children === text) return node;
  for (const child of [node.props?.children].flat(Infinity)) {
    const result = find(child, text);
    if (result) return result;
  }
  return null;
}
for (const kind of ['observing'])
  test(`${kind}: stale failure is logged without updating an obsolete view`, async () => {
    const component = kind === 'equipment' ? 'EquipmentProfilesPanel' : 'ObservingGoalsPanel';
    const source = readFileSync(
      new URL(`../src/components/${kind}/${component}.tsx`, import.meta.url),
      'utf8',
    );
    const exports = {},
      effects = [],
      writes = [],
      logs = [];
    let index = 0,
      reject;
    const pending = new Promise((_, r) => {
      reject = r;
    });
    const hooks = {
      useState: initial => {
        const id = index++;
        return [
          kind === 'equipment' && id === 2 ? 'Camera' : initial,
          value => writes.push({ id, value }),
        ];
      },
      useRef: value => ({ current: value }),
      useEffect: fn => effects.push(fn),
    };
    const api = {
      invoke: command => (command === 'get_equipment_profiles' ? Promise.resolve([]) : pending),
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
        console: { error: (...args) => logs.push(args) },
        require: name => {
          if (name === 'react') return hooks;
          if (name === '../../api') return { api };
          if (name === '../../contexts/NotificationContext')
            return { useNotifications: () => ({ notify() {} }) };
          if (name.startsWith('./')) return {};
          return require(name);
        },
      },
    );
    const tree = exports[component](
      kind === 'equipment' ? { cameras: ['Camera'] } : { frameSetId: 1 },
    );
    const cleanups = effects.map(effect => effect());
    if (kind === 'equipment') find(tree, 'Review saved solves').props.onClick();
    await Promise.resolve();
    cleanups.forEach(cleanup => cleanup?.());
    writes.length = 0;
    const error = Error('late rejected request');
    reject(error);
    await new Promise(resolve => setImmediate(resolve));
    assert.equal(logs.length, 1);
    assert.equal(logs[0][1], error);
    assert.deepEqual(writes, []);
  });

import { vi } from 'vitest';
import { api } from '../api';

/**
 * Answer `api.invoke` by command name. A value is resolved as is; a function gets the args.
 *
 * - The answer is always a promise, like the real `invoke`: a value resolves, a returned
 *   promise (pending, rejected) is passed through, a throwing function rejects.
 * - A fixed value is read when `routeInvoke` runs. An answer that depends on a variable the
 *   test reassigns later, or on the call itself, is a function (`() => current`).
 * - A rejection is a function too (`() => Promise.reject(…)`): a rejected promise held in the
 *   table would be created, and flagged unhandled, before any command asks for it.
 * - A command with no route goes to `fallback`; without one it resolves `null`, the default
 *   most hand-written routers in the tests already had.
 *
 * Call it where the test file mocks `src/api` (`vi.mock('<path>/api')`); it replaces any
 * earlier implementation of `api.invoke` and leaves the recorded calls alone.
 */
export function routeInvoke(
  routes: Record<string, unknown | ((args: any) => unknown)>,
  fallback?: (cmd: string, args: unknown) => unknown,
): void {
  vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: unknown) => {
    try {
      if (Object.prototype.hasOwnProperty.call(routes, cmd)) {
        const route = routes[cmd];
        return Promise.resolve(typeof route === 'function' ? route(args) : route);
      }
      return Promise.resolve(fallback ? fallback(cmd, args) : null);
    } catch (e) {
      return Promise.reject(e);
    }
  }) as never);
}

/** A `routeInvoke` fallback for a test that wants a command it did not list to fail loudly. */
export const unexpectedCommand = (cmd: string): Promise<never> =>
  Promise.reject(new Error(`unexpected ${cmd}`));

// The one definition of the default `api` mock. A test file opts in with
// `vi.mock('<relative path to src/api>')` — no factory — and vitest picks this
// file up in place of `src/api/index.ts`. Mock state is per test file.
//
// `invoke` and `listen` start as bare `vi.fn()`s (they return `undefined`); a
// test that needs an answer sets it itself, or through `routeInvoke` in
// `src/test/apiMock.ts`. A test whose default must differ keeps its own factory.
import { vi } from 'vitest';

export const api = { invoke: vi.fn(), listen: vi.fn() };

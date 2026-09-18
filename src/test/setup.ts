import '@testing-library/jest-dom/vitest';
import { afterEach } from 'vitest';
import { cleanup } from '@testing-library/react';

// `vitest.config.ts` doesn't set `test.globals`, so `@testing-library/react`'s
// own auto-cleanup (which only registers itself when it finds a global
// `afterEach`) never fires — every `render()` from a component test file
// (Task B1 is the first: `Checkbox.test.tsx`, `SettingNumber.test.tsx`) would
// otherwise leave its DOM mounted across `it` blocks in the same file, so a
// later `screen.getByRole(...)` that matched cleanly on its own throws
// "found multiple elements". Explicit, so it never depends on that detection.
afterEach(() => {
  cleanup();
});

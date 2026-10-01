import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    environment: 'jsdom',
    setupFiles: ['src/test/setup.ts'],
    include: ['src/**/*.test.{ts,tsx}'],
    // `index.css?raw` reaches `indexCss.test.ts` as text; every other CSS
    // import stays stubbed as before.
    css: { include: [/\/src\/index\.css/] },
  },
});

// The tests run in Node, without the void plugin vite.config.ts builds the
// Worker with: what they test needs nothing from the Workers runtime but
// the one function test-setup.ts adds.
import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    include: ['**/*.test.ts'],
    exclude: ['node_modules/**', '.void/**', 'dist/**'],
    setupFiles: ['./test-setup.ts'],
  },
});

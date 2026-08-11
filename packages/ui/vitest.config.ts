import { defineConfig } from 'vitest/config';

export default defineConfig({
  test: {
    // The components are rendered and driven the way a user drives them, which
    // needs a DOM. jsdom is enough: nothing here measures layout.
    environment: 'jsdom',
    globals: true,
    include: ['src/**/*.test.tsx'],
    restoreMocks: true,
  },
});

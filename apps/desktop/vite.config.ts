/// <reference types="vitest/config" />
import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

/**
 * Vite builds the interface; Tauri wraps the result in a window.
 *
 * The Tauri command-line tool starts this dev server for `npm run dev` and runs
 * `vite build` for `npm run build:app`, so the two paths cannot drift: there is
 * one build here, and the desktop application is the same bundle a browser
 * would load from `npm run dev:web`.
 */
export default defineConfig({
  plugins: [react()],

  // Tauri prints the compiler's progress in the same terminal. Clearing the
  // screen on every reload would take a Rust error away mid-read.
  clearScreen: false,

  server: {
    // Tauri's `devUrl` names this port, so a port already in use has to be an
    // error rather than a silent move to 5174 that Tauri then cannot find.
    port: 5173,
    strictPort: true,
  },

  build: {
    // A crash report from an installed application is the only diagnostic
    // available for the interface, for the same reason `[profile.release]`
    // keeps debug symbols in the recorder.
    sourcemap: true,
  },

  test: {
    environment: 'jsdom',
    globals: false,
    setupFiles: ['./src/test/setup.ts'],
    // `packages/*` are consumed as source rather than built, so their tests run
    // here too. One `npm test` for the whole npm workspace is worth more than a
    // second Vitest configuration somebody has to remember to run, and the
    // shell's own tests already reach across the boundary: what they exercise is
    // the application and its component library together.
    include: ['src/**/*.test.{ts,tsx}', '../../packages/*/src/**/*.test.{ts,tsx}'],
  },
});

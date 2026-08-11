import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

/**
 * The port `tauri dev` points the window at. Fixed rather than chosen at
 * random because tauri.conf.json's `devUrl` has to name the same one, and
 * `strictPort` makes a clash fail loudly instead of silently serving on a port
 * the window will never open.
 */
const DEV_PORT = 1420;

export default defineConfig({
  plugins: [react()],

  // Vite's dependency pre-bundler treats anything under node_modules as a
  // published package. `@clipped/ui` and `@clipped/shared` are symlinked
  // workspace source, so pre-bundling them would freeze a copy and stop
  // edits to a component showing up until the cache was cleared.
  optimizeDeps: {
    exclude: ['@clipped/shared', '@clipped/ui'],
  },

  server: {
    port: DEV_PORT,
    strictPort: true,
    // Tauri shows the compiler's error, not Vite's overlay, when the Rust
    // side fails; keeping the overlay on is what surfaces the frontend's.
    watch: {
      // Cargo's output is large and changes constantly during
      // `tauri dev`. Watching it wakes the dev server for nothing.
      ignored: ['**/src-tauri/**'],
    },
  },

  build: {
    // WebView2 on Windows 10/11 is evergreen Chromium, so there is no
    // reason to down-level past what it supports. `chrome110` is the
    // floor `color-mix()` in the design tokens needs anyway.
    target: 'chrome110',
    // Debug symbols for the production bundle are what make a stack trace
    // from a user's machine readable (AGENTS.md section 15).
    sourcemap: true,
  },
});

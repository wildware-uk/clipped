import js from '@eslint/js';
import jsxA11y from 'eslint-plugin-jsx-a11y';
import react from 'eslint-plugin-react';
import reactHooks from 'eslint-plugin-react-hooks';
import reactRefresh from 'eslint-plugin-react-refresh';
import globals from 'globals';
import tseslint from 'typescript-eslint';

/**
 * One ESLint configuration for every TypeScript package in this repository.
 *
 * It is at the root, rather than one per package, for the same reason
 * `[workspace.lints]` is in the root `Cargo.toml`: a rule that is on in one
 * package and off in the next is a rule nobody can rely on.
 *
 * `jsx-a11y` is not decoration. The accessibility baseline in AGENTS.md section
 * 46 - keyboard operation, labels, focus - is mostly a set of rules a linter can
 * check, and a rule that fails the build is worth more than a paragraph asking
 * people to remember.
 */
export default tseslint.config(
  {
    // Build output and dependencies. `apps/desktop/src-tauri` is Rust and is
    // linted by clippy, not by this.
    ignores: ['**/dist/', '**/node_modules/', 'apps/desktop/src-tauri/'],
  },

  js.configs.recommended,
  tseslint.configs.recommended,

  {
    files: ['**/*.{ts,tsx}'],
    languageOptions: {
      globals: globals.browser,
    },
    rules: {
      // The interface talks to the recorder over IPC, where `any` is exactly
      // the thing that turns a protocol mismatch into a runtime surprise.
      '@typescript-eslint/no-explicit-any': 'error',
      '@typescript-eslint/consistent-type-imports': [
        'error',
        { prefer: 'type-imports', fixStyle: 'separate-type-imports' },
      ],
    },
  },

  {
    files: ['**/*.tsx'],
    ...react.configs.flat.recommended,
    ...react.configs.flat['jsx-runtime'],
    settings: { react: { version: 'detect' } },
  },

  {
    files: ['**/*.{ts,tsx}'],
    plugins: { 'react-hooks': reactHooks },
    rules: reactHooks.configs.recommended.rules,
  },

  {
    files: ['**/*.tsx'],
    ...jsxA11y.flatConfigs.strict,
  },

  {
    files: ['apps/desktop/src/**/*.tsx', 'packages/ui/src/**/*.tsx'],
    plugins: { 'react-refresh': reactRefresh },
    rules: {
      // Hot reloading only survives a module that exports components alone.
      'react-refresh/only-export-components': ['warn', { allowConstantExport: true }],
    },
  },

  {
    files: ['**/*.config.{js,ts}', 'eslint.config.js'],
    languageOptions: { globals: globals.node },
  },
);

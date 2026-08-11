// ESLint configuration for every TypeScript package in the npm workspace.
//
// One flat config at the repository root rather than one per package: the three
// packages are parts of a single application, and a rule that holds in
// `packages/ui` but not in `apps/desktop` would be a rule nobody can rely on.
//
// The accessibility plugin is not decoration. AGENTS.md section 46 requires
// keyboard navigation, visible focus, screen reader labels and non-colour-only
// state, and `jsx-a11y` catches the mechanical half of that - a click handler on
// a `div`, an icon-only control with no accessible name - at the point the code
// is written rather than in a manual audit nobody schedules.

import js from '@eslint/js';
import jsxA11y from 'eslint-plugin-jsx-a11y';
import reactHooks from 'eslint-plugin-react-hooks';
import globals from 'globals';
import tseslint from 'typescript-eslint';

export default tseslint.config(
    {
        // Build output and dependencies. `src-tauri/target` is Cargo's, and
        // `dist` is Vite's; neither is source anybody wrote.
        ignores: [
            '**/dist/**',
            '**/node_modules/**',
            'apps/desktop/src-tauri/target/**',
            'target/**',
        ],
    },
    js.configs.recommended,
    tseslint.configs.recommendedTypeChecked,
    {
        languageOptions: {
            parserOptions: {
                projectService: true,
                tsconfigRootDir: import.meta.dirname,
            },
            globals: globals.browser,
        },
    },
    jsxA11y.flatConfigs.recommended,
    reactHooks.configs['recommended-latest'],
    {
        // The Vite and Vitest configuration files run in Node, not the browser,
        // and are typed by `tsconfig.node.json` rather than the app's tsconfig.
        files: ['**/*.config.{ts,js}'],
        languageOptions: { globals: globals.node },
    },
    {
        // Config files and test setup are plain modules; the type-aware rules
        // that matter for application code produce only noise here.
        files: ['eslint.config.js'],
        extends: [tseslint.configs.disableTypeChecked],
    },
);

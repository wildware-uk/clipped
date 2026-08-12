// @vitest-environment node
//
// Holds the Settings screen to the Rust that owns what it talks about.
//
// The screen is an account of settings this window cannot read: what each one
// is, how it is set today, and what has to land before this window can hold it
// (`settings.ts`). Every one of those is a claim about code in another process,
// and a screen of claims nobody re-checks is a screen that goes quietly wrong —
// a renamed settings key, a subcommand that moved, an `apply_settings` that got
// implemented, and the screen still says what it said in August.
//
// TypeScript cannot call any of it, so this file does the next honest thing and
// reads the definitions out of the sources that hold them. Each case names the
// file and the item it reads, and throws when that item is no longer there,
// because a check that has stopped finding its subject must fail rather than
// pass on nothing — the rule `contrast.test.ts` and `stylesheet.test.ts` in
// `packages/ui` learned the hard way.
//
// It runs in the node environment because the subject is those files as text.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import {
  NOTHING_IS_EDITABLE,
  NOTIFICATIONS_FILE,
  SETTINGS_FILE,
  SETTINGS_SECTIONS,
  type SettingsFile,
} from './settings';

/** The repository root, from this file. */
const root = fileURLToPath(new URL('../../../', import.meta.url));

/** A source file as text, with CRLF - a git setting - normalised away. */
function source(path: string): string {
  return readFileSync(`${root}${path}`, 'utf8').replace(/\r\n/g, '\n');
}

const VALUE_RS = source('crates/session/src/config/value.rs');
const DOCUMENT_RS = source('crates/session/src/config/document.rs');
const DIRECTORIES_RS = source('crates/logging/src/directories.rs');
const COMMAND_RS = source('crates/ipc/src/command.rs');
const CLI_RS = source('apps/recorder/src/cli.rs');
const POLICY_RS = source('apps/desktop/src-tauri/src/notification_policy.rs');
const NOTIFICATIONS_RS = source('apps/desktop/src-tauri/src/notifications.rs');
const TAURI_CONF = source('apps/desktop/src-tauri/tauri.conf.json');

/**
 * The braced block that follows `marker`, or a failure naming what has moved.
 *
 * Brace matching rather than a pattern per item: the blocks read below are
 * `match` arms and enumerations, and a regular expression over them would break
 * on a line wrap `cargo fmt` decided differently — which would look like the
 * screen being wrong rather than like this file needing an edit.
 */
function blockAfter(path: string, text: string, marker: string): string {
  const at = text.indexOf(marker);
  expect(at, `${path} no longer contains "${marker}"`).toBeGreaterThanOrEqual(0);

  const open = text.indexOf('{', at);
  expect(open, `${path} has no block after "${marker}"`).toBeGreaterThanOrEqual(0);

  let depth = 0;
  for (let index = open; index < text.length; index += 1) {
    if (text[index] === '{') {
      depth += 1;
    } else if (text[index] === '}') {
      depth -= 1;
      if (depth === 0) {
        return text.slice(open + 1, index);
      }
    }
  }

  throw new Error(`${path}: the block after "${marker}" is not closed`);
}

/** Every double-quoted string in a fragment of Rust. */
function quoted(fragment: string): readonly string[] {
  return [...fragment.matchAll(/"([^"]*)"/g)].map((found) => found[1] ?? '');
}

/** The value of a `const NAME: &str = "…";` in a Rust source. */
function constant(path: string, text: string, name: string): string {
  const found = new RegExp(`const ${name}: &str = "([^"]*)";`).exec(text);
  expect(found, `${path} no longer declares ${name}`).not.toBeNull();
  return found?.[1] ?? '';
}

/** Every setting the screen names a key for, in one of the two files. */
function keysNamedOnTheScreen(file: SettingsFile): readonly string[] {
  return SETTINGS_SECTIONS.flatMap((section) =>
    section.rows.flatMap((row) => (row.key?.file === file ? [row.key.name] : [])),
  );
}

/** Everything the screen says, as one body of text to read commands out of. */
const EVERYTHING_THE_SCREEN_SAYS = [
  ...Object.values(NOTHING_IS_EDITABLE),
  ...SETTINGS_SECTIONS.flatMap((section) => [
    section.lead,
    ...section.rows.flatMap((row) => [row.label, row.today, row.run ?? '', row.needs]),
  ]),
].join('\n');

describe('the settings the screen names', () => {
  /*
   * `SettingKey` exists partly for this screen — its own documentation says so:
   * "the settings screen can list what there is to render without this module
   * having to publish a second list that goes stale". This is that list, and
   * this is what stops it going stale: both directions, so that a setting the
   * configuration API gains is a failure here rather than a row nobody added,
   * and a setting the screen invented is a failure here rather than a row a
   * reader believes.
   */
  it('are exactly the ones the configuration API models', () => {
    const modelled = quoted(
      blockAfter(
        'crates/session/src/config/value.rs',
        VALUE_RS,
        "pub const fn name(self) -> &'static str",
      ),
    );

    expect(modelled.length, 'SettingKey::name no longer returns any key').toBeGreaterThan(0);
    expect([...keysNamedOnTheScreen('settings.json')].sort()).toEqual([...modelled].sort());
  });

  /*
   * The three notification categories are the desktop host's own, in a second
   * settings file until issue #252 folds them into the configuration API. Their
   * keys are documented as stable, because renaming one would silently switch a
   * category back on for somebody who had switched it off — so a rename has to
   * fail something, and the screen is one of the things that would otherwise
   * carry the old spelling.
   */
  it('include every notification category, spelled as its file spells it', () => {
    const categories = quoted(
      blockAfter(
        'apps/desktop/src-tauri/src/notification_policy.rs',
        POLICY_RS,
        "pub(crate) const fn key(self) -> &'static str",
      ),
    );

    expect(categories.length, 'NotificationCategory::key returns no key').toBeGreaterThan(0);
    expect([...keysNamedOnTheScreen('notifications.json')].sort()).toEqual([...categories].sort());
  });
});

describe('the files the screen tells somebody to open', () => {
  /*
   * `%LOCALAPPDATA%\Clipped\settings.json`, built from the two constants that
   * decide it rather than restated. A path is the one thing on this screen
   * anybody can act on directly (AGENTS.md section 28), so a path that has
   * quietly become wrong is worse than no path at all.
   */
  it('name the settings file where the recorder actually keeps it', () => {
    const directory = constant(
      'crates/logging/src/directories.rs',
      DIRECTORIES_RS,
      'APPLICATION_DIRECTORY',
    );
    const file = constant('crates/session/src/config/document.rs', DOCUMENT_RS, 'FILE_NAME');

    // Joined rather than written as one template: `\$` in a template literal is
    // an escape, so `String.raw` with a `\${…}` in it yields the substitution
    // unexpanded and the case compares the screen with a piece of source code.
    expect(SETTINGS_FILE).toBe(['%LOCALAPPDATA%', directory, file].join('\\'));
  });

  /*
   * The notification switches are in the Tauri application configuration
   * directory, which on Windows is `%APPDATA%\<bundle identifier>`. Both halves
   * are read: changing the identifier moves every user's file, and it is the
   * sort of change made for a reason that has nothing to do with this screen.
   */
  it('name the notification file where the window actually reads it', () => {
    const identifier = /"identifier":\s*"([^"]+)"/.exec(TAURI_CONF);
    expect(identifier, 'tauri.conf.json declares no identifier').not.toBeNull();

    const file = constant(
      'apps/desktop/src-tauri/src/notifications.rs',
      NOTIFICATIONS_RS,
      'SETTINGS_FILE',
    );

    expect(NOTIFICATIONS_FILE).toBe(['%APPDATA%', identifier?.[1] ?? '', file].join('\\'));
  });
});

describe('the claim the whole screen rests on', () => {
  /*
   * The screen says, in the one panel a reader is meant to take away, that
   * `apply_settings` is refused as not implemented by every build. On the day
   * that stops being true this screen is misleading in the worst way available
   * to it: telling somebody that what they want is impossible when it has just
   * been built.
   */
  it('is that apply_settings is still an unbuilt command', () => {
    const declaration = COMMAND_RS.slice(COMMAND_RS.indexOf('pub const UNBUILT_COMMANDS'));
    const listed = declaration.slice(0, declaration.indexOf('];'));

    expect(listed, 'UNBUILT_COMMANDS is gone from crates/ipc/src/command.rs').not.toBe('');
    expect(listed).toContain('UnbuiltCommand::ApplySettings');

    // And that it is still called what the panel calls it. `UnbuiltCommand`'s
    // own `name` is the second of the two in that file; `Command`'s takes
    // `&self`, which is what keeps these two markers apart.
    const unbuiltNames = quoted(
      blockAfter('crates/ipc/src/command.rs', COMMAND_RS, 'pub const fn name(self)'),
    );
    expect(unbuiltNames).toContain('apply_settings');
    expect(NOTHING_IS_EDITABLE.why).toContain('apply_settings');
  });

  /*
   * And that nothing in the protocol reads configuration either — which is the
   * half that would let this window at least *show* a setting. Read as the
   * commands the recorder parses, so that adding one called `get_settings`
   * fails here and this screen gets rewritten with it.
   */
  it('is that no command reads the settings back', () => {
    const commands = quoted(
      blockAfter('crates/ipc/src/command.rs', COMMAND_RS, 'pub fn from_request'),
    ).filter((literal) => /^[a-z_]+$/.test(literal));

    expect(commands).toContain('get_status');
    for (const command of commands) {
      expect(command, `${command} looks like a settings command`).not.toMatch(/settings|config/);
    }
  });
});

describe('the commands the screen tells somebody to run', () => {
  /** Every `clipped-recorder <subcommand>` the screen prints. */
  const SUBCOMMANDS = [
    ...new Set(
      [...EVERYTHING_THE_SCREEN_SAYS.matchAll(/clipped-recorder ([a-z][a-z-]*)/g)].map(
        (found) => found[1] ?? '',
      ),
    ),
  ];

  /** Every `--option` the screen prints. */
  const OPTIONS = [
    ...new Set(
      [...EVERYTHING_THE_SCREEN_SAYS.matchAll(/--([a-z][a-z-]*)/g)].map((found) => found[1] ?? ''),
    ),
  ];

  it('are the subcommands the recorder has', () => {
    const declared = [
      ...blockAfter('apps/recorder/src/cli.rs', CLI_RS, 'pub enum Command').matchAll(
        /^ {4}(\w+)\(/gm,
      ),
    ].map((found) => found[1] ?? '');

    expect(declared, 'the recorder declares no subcommands').not.toHaveLength(0);
    expect(SUBCOMMANDS, 'the screen names no command to run').not.toHaveLength(0);

    for (const subcommand of SUBCOMMANDS) {
      // clap spells a variant on the command line in kebab case, so this is the
      // same name read back the other way: `start-at-login` is `StartAtLogin`.
      const variant = subcommand
        .split('-')
        .map((word) => word.slice(0, 1).toUpperCase() + word.slice(1))
        .join('');

      expect(declared, `clipped-recorder ${subcommand}`).toContain(variant);
    }
  });

  it('are the options those subcommands take', () => {
    expect(OPTIONS, 'the screen quotes no options').not.toHaveLength(0);

    for (const option of OPTIONS) {
      // A `long` clap takes from the field name, kebab back to snake.
      const field = option.replaceAll('-', '_');
      expect(CLI_RS, `--${option}`).toMatch(new RegExp(`pub ${field}\\b`));
    }
  });
});

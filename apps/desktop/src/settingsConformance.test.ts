// @vitest-environment node
//
// Holds the Settings screen to the Rust that owns what it talks about.
//
// The screen draws a control per setting the recorder sends, and an account of
// the settings SPEC.md asks for that have nowhere to be saved (`settings.ts`).
// Both halves are claims about code in another process, and a screen of claims
// nobody re-checks is a screen that goes quietly wrong — a renamed settings key
// the window then never draws, a subcommand that moved, a `#252` that landed
// and a screen still saying what it said in August.
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
const RECORDER_SETTINGS_RS = source('apps/recorder/src/settings.rs');
const MESSAGE_RS = source('crates/ipc/src/message.rs');
const SERVE_RS = source('apps/recorder/src/serve.rs');
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

/** Every setting the screen draws a control for, in section order. */
const KEYS_DRAWN_AS_CONTROLS = SETTINGS_SECTIONS.flatMap((section) => section.keys);

/** Everything the screen says, as one body of text to read commands out of. */
const EVERYTHING_THE_SCREEN_SAYS = SETTINGS_SECTIONS.flatMap((section) => [
  section.lead,
  ...section.rows.flatMap((row) => [row.label, row.today, row.run ?? '', row.needs]),
]).join('\n');

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
  it('are exactly the ones the recorder will send, and every one is drawn', () => {
    const modelled = quoted(
      blockAfter(
        'crates/session/src/config/value.rs',
        VALUE_RS,
        "pub const fn name(self) -> &'static str",
      ),
    );
    expect(modelled.length, 'SettingKey::name no longer returns any key').toBeGreaterThan(0);

    // The recording directory is not a `SettingKey` — it is global rather than
    // per game — and the recorder puts it in the same list, so the screen has
    // to draw it from the same list (`apps/recorder/src/settings.rs`).
    const directory = constant(
      'apps/recorder/src/settings.rs',
      RECORDER_SETTINGS_RS,
      'RECORDING_DIRECTORY',
    );

    // Both directions. A setting the configuration API gains and this screen
    // does not list is one nobody can change from the window, which is the
    // failure this whole issue was about; one the screen lists and the recorder
    // never sends is a control that would never be drawn.
    expect([...KEYS_DRAWN_AS_CONTROLS].sort()).toEqual([...modelled, directory].sort());
  });

  /*
   * And the keys the account rows quote are real keys too. These are the rows
   * that say "set this by hand in the file" — a spelling that has drifted sends
   * somebody to edit a key nothing reads.
   */
  it('include only keys the settings file really has, in the rows that name one', () => {
    const named = keysNamedOnTheScreen('settings.json');
    expect(named.length, 'no row names a settings.json key any more').toBeGreaterThan(0);

    for (const key of named) {
      expect(
        [VALUE_RS, RECORDER_SETTINGS_RS, source('crates/session/src/config/storage.rs')].some(
          (text) => text.includes(`"${key}"`),
        ),
        `${key} is not a key any Rust source declares`,
      ).toBe(true);
    }
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
   * That the recorder has the three commands this screen is built out of. Until
   * issue #51 it had none of them: `apply_settings` was parsed only so that it
   * could be refused with `not_implemented`, and there was no command that read
   * a setting at all. A screen of live controls against a build that refuses
   * them is worse than the account it replaced — every control would fail at
   * the moment somebody used it.
   *
   * Read as the commands `Command::from_request` parses, so that a command
   * quietly renamed on the Rust side fails here rather than in a window nobody
   * has opened yet.
   */
  it('is that the recorder reads and changes settings, and lists this machine’s devices', () => {
    const commands = quoted(
      blockAfter('crates/ipc/src/command.rs', COMMAND_RS, 'pub fn from_request'),
    ).filter((literal) => /^[a-z_]+$/.test(literal));

    expect(commands).toContain('get_status');
    for (const command of ['get_settings', 'apply_settings', 'get_audio_devices']) {
      expect(commands, `${command} is not a command this recorder parses`).toContain(command);
    }
  });

  /*
   * And that none of the three is refused as unbuilt. `apply_settings` was the
   * last command in that list, and a command left in it after its subsystem
   * landed refuses every request with a plausible sentence about a milestone
   * that nobody questions — which is exactly what the previous version of this
   * screen was written around.
   */
  it('is that none of them is a command the recorder refuses as not built', () => {
    expect(
      COMMAND_RS,
      'UNBUILT_COMMANDS is back in crates/ipc/src/command.rs, and the screen has not been told',
    ).not.toContain('UNBUILT_COMMANDS');
  });

  /*
   * And that a window can tell before it draws anything. A recorder older than
   * this window still answers the handshake, and the feature is what stops the
   * screen offering controls it will refuse (AGENTS.md section 27).
   */
  it('is that the recorder says so in its welcome', () => {
    expect(MESSAGE_RS, 'crates/ipc no longer declares a `settings` feature').toMatch(
      /SETTINGS = "settings";/,
    );
  });
});

describe('the claim the start-at-login switch rests on', () => {
  /*
   * That the recorder answers the two commands the switch is built out of
   * (issue #308). Until it did, the Startup section printed
   * `clipped-recorder start-at-login enable` and asked somebody to go and run
   * it — because nothing in the protocol read or wrote that value, and a switch
   * drawn over nothing is the control that silently does nothing of AGENTS.md
   * section 27.
   *
   * Read out of `Command::from_request` for the reason the settings commands
   * are: a command quietly renamed on the Rust side has to fail here rather
   * than in a window nobody has opened.
   */
  it('is that the recorder reads and writes whether it starts at sign-in', () => {
    const commands = quoted(
      blockAfter('crates/ipc/src/command.rs', COMMAND_RS, 'pub fn from_request'),
    ).filter((literal) => /^[a-z_]+$/.test(literal));

    for (const command of ['get_start_at_login', 'set_start_at_login']) {
      expect(commands, `${command} is not a command this recorder parses`).toContain(command);
    }
  });

  /*
   * And that a window can tell before it draws the switch. A recorder older
   * than this window has the settings commands and neither of these, so the
   * capability is what separates "Clipped does not start at sign-in" from
   * "this recorder cannot be asked" — opposite answers, and the switch would
   * be drawn identically for both.
   */
  it('is that the recorder says so in its welcome', () => {
    expect(MESSAGE_RS, 'crates/ipc no longer declares a `startup` feature').toMatch(
      /STARTUP = "startup";/,
    );
  });

  /*
   * And that the recorder writes it through the subcommand's own code rather
   * than a second implementation of the same registry value (AGENTS.md section
   * 55). Two implementations of "is Clipped set to start at login" is two
   * answers to one question, and the settings screen would show whichever one
   * happened to be wired up.
   */
  it('is that one module writes that registry value, and serve calls it', () => {
    expect(
      SERVE_RS,
      'serve no longer answers get_start_at_login through crate::start_at_login',
    ).toMatch(/Command::GetStartAtLogin[\s\S]*?start_at_login::current/);
    expect(SERVE_RS, 'serve no longer answers set_start_at_login through it either').toMatch(
      /Command::SetStartAtLogin[\s\S]*?start_at_login::set/,
    );
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

  it('are the subcommands the recorder has, with the options those take', () => {
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

    // The options, in the same case rather than one of their own: the screen
    // quotes fewer of them now that most settings are controls, and a case that
    // asserted "every option is real" over an empty list would pass on nothing.
    for (const option of OPTIONS) {
      // A `long` clap takes from the field name, kebab back to snake.
      const field = option.replaceAll('-', '_');
      expect(CLI_RS, `--${option}`).toMatch(new RegExp(`pub ${field}\\b`));
    }
  });
});

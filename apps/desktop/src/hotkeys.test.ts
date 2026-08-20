import type { HotkeyBinding } from '@clipped/shared';
import { describe, expect, it } from 'vitest';

import { actionHolding, chordOf, hotkeySettingKey } from './hotkeys';

/** A row, with only the fields these cases read. */
function binding(action: string, hotkey?: string): HotkeyBinding {
  return {
    action,
    label: action,
    hotkey,
    state: hotkey === undefined ? { state: 'unbound' } : { state: 'registered' },
    handled: true,
  } as HotkeyBinding;
}

describe('the combination a press names', () => {
  it('spells a chord the way the recorder does', () => {
    expect(chordOf({ key: 'F9', ctrlKey: true, shiftKey: false, altKey: false })).toBe('Ctrl+F9');
    expect(chordOf({ key: 'F9', ctrlKey: true, shiftKey: true, altKey: false })).toBe(
      'Ctrl+Shift+F9',
    );
    expect(chordOf({ key: 'F9', ctrlKey: true, shiftKey: true, altKey: true })).toBe(
      'Ctrl+Alt+Shift+F9',
    );
  });

  /*
   * The order is fixed rather than the order they were held down in. The
   * recorder parses this string and answers with its own `Display`, so a window
   * that sent `Shift+Ctrl+F9` would save a setting and then read back something
   * that did not match what it saved.
   */
  it('puts the modifiers in one order however they were held', () => {
    const held = chordOf({ key: 'F9', ctrlKey: true, shiftKey: true, altKey: true });
    expect(held).toBe('Ctrl+Alt+Shift+F9');
  });

  it('uppercases a letter, so one key is one chord', () => {
    expect(chordOf({ key: 'k', ctrlKey: true, shiftKey: false, altKey: false })).toBe('Ctrl+K');
    expect(chordOf({ key: 'K', ctrlKey: true, shiftKey: false, altKey: false })).toBe('Ctrl+K');
  });

  /*
   * A modifier alone is somebody part way through a chord. Answering `null` is
   * what lets the capture control keep waiting rather than save `Shift`.
   */
  it('names nothing while only modifiers are down', () => {
    for (const key of ['Control', 'Shift', 'Alt', 'Meta']) {
      expect(chordOf({ key, ctrlKey: true, shiftKey: true, altKey: false })).toBeNull();
    }
  });

  /*
   * `RegisterHotKey` would accept a bare `F9`, and then every press of F9 in
   * every application on the machine would belong to Clipped. Nobody asked for
   * that by pressing one key in a settings screen.
   */
  it('refuses a key with no modifier', () => {
    expect(chordOf({ key: 'F9', ctrlKey: false, shiftKey: false, altKey: false })).toBeNull();
    expect(chordOf({ key: 'K', ctrlKey: false, shiftKey: false, altKey: false })).toBeNull();
  });

  it('refuses a key it cannot spell for the recorder', () => {
    expect(chordOf({ key: 'Dead', ctrlKey: true, shiftKey: false, altKey: false })).toBeNull();
    expect(chordOf({ key: 'F25', ctrlKey: true, shiftKey: false, altKey: false })).toBeNull();
  });
});

describe('the conflict this window can see', () => {
  const rows = [binding('save_replay', 'Ctrl+F10'), binding('add_bookmark', 'Ctrl+F9')];

  it('names the action already holding a combination', () => {
    expect(actionHolding(rows, 'Ctrl+F9', 'save_replay')?.action).toBe('add_bookmark');
  });

  it('is not a conflict with the row being rebound', () => {
    expect(actionHolding(rows, 'Ctrl+F10', 'save_replay')).toBeNull();
  });

  it('finds nothing for a combination no action holds', () => {
    expect(actionHolding(rows, 'Ctrl+F8', 'save_replay')).toBeNull();
  });

  /*
   * An unbound row holds no combination. Without this, `undefined === undefined`
   * would make every unbound action a conflict with every other one.
   */
  it('does not treat two unbound actions as holding the same combination', () => {
    const unbound = [binding('save_replay'), binding('add_bookmark')];
    expect(actionHolding(unbound, 'Ctrl+F8', 'save_replay')).toBeNull();
  });
});

describe('the name a chord is saved under', () => {
  /*
   * The prefix `apps/recorder/src/settings.rs` reads. Asserted rather than
   * assumed: a window that sent `save_replay` would be told the key is not a
   * setting, and a window that sent `hotkey-save_replay` would be told the same,
   * both after the user had typed a combination.
   */
  it('is the action under the prefix the recorder reads', () => {
    expect(hotkeySettingKey('save_replay')).toBe('hotkey_save_replay');
  });
});

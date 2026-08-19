import type { AudioDevices, SettingEntry } from '@clipped/shared';
import type { ReactNode } from 'react';

import type { LibraryRead } from './library';
import { glossOf, MAXIMUM_USAGE, MINIMUM_FREE_SPACE } from './storage';
import type { Inheritance } from './settings';
import {
  chooseRecordingDirectory,
  describeInheritance,
  describeSettingsProblem,
  isSwitch,
  MICROPHONE,
  microphoneOptions,
  RECORDING_DIRECTORY,
} from './settings';

/**
 * One setting, drawn as the control the recorder says it is.
 *
 * Here rather than inside the Settings screen because two pages draw the same
 * settings: the global page, and one game's. A second implementation of "is
 * this a switch, a list or a field" would be a second answer to what the
 * recorder already said, differing in whichever of them somebody forgot to
 * change (AGENTS.md section 55).
 *
 * What differs between the two pages is one thing, and it is
 * {@link SettingScope}: which layer counts as "set here". The recorder answers
 * that per setting — `SettingEntry.overridden` is `Resolved::is_overridden`
 * asked against the scope it resolved for — so this draws the answer and never
 * works one out (`crates/session/src/config/value.rs`).
 */

/**
 * Which page a control is on.
 *
 * The global settings, or one game's. A game's page marks every control with
 * where its value came from, because that is the question that page exists to
 * answer; the global page has no layer above it to have inherited from, so it
 * says only whether the value was configured.
 */
export type SettingScope = { readonly kind: 'global' } | { readonly kind: 'game' };

/** The element id a setting's control has, so its label can name it. */
function fieldId(key: string): string {
  return `setting-${key}`;
}

/** The element id a setting's hint has, so its control can be described by it. */
function hintId(key: string): string {
  return `setting-${key}-hint`;
}

/**
 * Where a value came from, beside the control it applies to.
 *
 * Drawn only on a game's page, because only there is there a layer below to
 * have inherited from. Two signals, neither of them colour: the **word** —
 * "Inherited" or "Set for this game" — and, separately, whether Reset is on the
 * control at all, because Reset exists exactly when a value is this game's
 * (issue #63's first acceptance criterion, AGENTS.md section 46).
 *
 * The word is inside the accessible name of the control's own group rather than
 * floating beside it: `aria-label` names what it is about, so a screen reader
 * reaching it hears "Frame rate: Inherited" rather than a bare "Inherited"
 * whose subject is two elements away.
 */
function Inherited({
  inheritance,
  label,
}: {
  readonly inheritance: Inheritance | undefined;
  readonly label: string;
}): ReactNode {
  if (inheritance === undefined) {
    return null;
  }
  return (
    <span className="clipped-tag clipped-tag--outline" aria-label={`${label}: ${inheritance.tag}`}>
      {inheritance.tag}
    </span>
  );
}

/** One setting the recorder says is in force: a control, and what it accepts. */
export function Field({
  entry,
  devices,
  scope,
  edited,
  onEdit,
  onReset,
}: {
  readonly entry: SettingEntry;
  readonly devices: LibraryRead<AudioDevices>;
  /** Which page this is: the global settings, or one game's. */
  readonly scope: SettingScope;
  readonly edited: string | undefined;
  readonly onEdit: (value: string) => void;
  readonly onReset: () => void;
}): ReactNode {
  const value = edited ?? entry.value;
  const inheritance = scope.kind === 'game' ? describeInheritance(entry) : undefined;
  const options = entry.key === MICROPHONE ? microphoneOptions(entry, devices) : undefined;
  const choices = entry.choices ?? [];
  // Only the two settings that are a number of bytes. A maximum age is in days
  // and "90 days is 90 bytes" would be nonsense, so the gloss is asked for by
  // key rather than of every field that happens to hold a number.
  const gloss =
    entry.key === MAXIMUM_USAGE || entry.key === MINIMUM_FREE_SPACE ? glossOf(value) : undefined;

  /*
   * A setting whose only two values are `true` and `false` is a switch, and it
   * is drawn as one. A list of those two words is what the recorder sends —
   * every value on that protocol is the text the settings file spells it in
   * (`crates/ipc/src/settings.rs`) — and drawing it as a two-option dropdown
   * reading "true" and "false" would be the settings file leaking through a
   * control (AGENTS.md section 29).
   */
  if (isSwitch(entry)) {
    return (
      <div className="clipped-field">
        <label className="clipped-field__label" htmlFor={fieldId(entry.key)}>
          <input
            id={fieldId(entry.key)}
            type="checkbox"
            aria-describedby={hintId(entry.key)}
            checked={value !== 'false'}
            onChange={(event) => {
              onEdit(event.target.checked ? 'true' : 'false');
            }}
          />{' '}
          {entry.label}
        </label>

        <Inherited inheritance={inheritance} label={entry.label} />

        {/*
         * Reset, on a switch, is "stop saying anything about this" rather than
         * "turn it on" — the two differ the day the shipped default changes.
         */}
        {entry.overridden ? (
          <button
            type="button"
            className="clipped-btn clipped-btn--secondary"
            onClick={onReset}
            aria-label={`Reset ${entry.label}`}
          >
            Reset
          </button>
        ) : null}

        <p className="clipped-muted" id={hintId(entry.key)}>
          {inheritance?.hint ??
            (entry.overridden ? 'You changed this.' : 'Clipped ships with this on.')}
        </p>

        <NotYetInForce entry={entry} />
      </div>
    );
  }

  return (
    <div className="clipped-field">
      <label className="clipped-field__label" htmlFor={fieldId(entry.key)}>
        {entry.label}
      </label>

      <Inherited inheritance={inheritance} label={entry.label} />

      {options !== undefined || choices.length > 0 ? (
        <select
          className="clipped-input"
          id={fieldId(entry.key)}
          aria-describedby={hintId(entry.key)}
          value={value}
          onChange={(event) => {
            onEdit(event.target.value);
          }}
        >
          {(options ?? choices.map((choice) => ({ value: choice, label: choice }))).map(
            (option) => (
              <option key={option.value} value={option.value}>
                {option.label}
              </option>
            ),
          )}
        </select>
      ) : (
        <input
          className="clipped-input"
          id={fieldId(entry.key)}
          aria-describedby={hintId(entry.key)}
          type="text"
          value={value}
          onChange={(event) => {
            onEdit(event.target.value);
          }}
        />
      )}

      {entry.key === RECORDING_DIRECTORY ? (
        <button
          type="button"
          className="clipped-btn clipped-btn--secondary"
          onClick={() => {
            void chooseRecordingDirectory(value).then((chosen) => {
              // Dismissed is not a choice, and must not clear what is there.
              if (chosen !== null) {
                onEdit(chosen);
              }
            });
          }}
        >
          Browse…
        </button>
      ) : null}

      {/*
       * Enabled only for a setting this scope actually set. Reset on a value
       * nobody configured would be a control that does nothing, and the
       * recorder is what knows which is which (`Resolved::is_overridden`).
       */}
      {entry.overridden ? (
        <button
          type="button"
          className="clipped-btn clipped-btn--secondary"
          onClick={onReset}
          aria-label={`Reset ${entry.label}`}
        >
          Reset
        </button>
      ) : null}

      <p className="clipped-muted" id={hintId(entry.key)}>
        {entry.accepted}{' '}
        {inheritance?.hint ??
          (entry.overridden ? '' : 'Nothing has changed this, so it is what Clipped ships with.')}
        {/*
         * The same number, read back in the unit a person reads. The field
         * carries what the settings file carries, because that is what the
         * recorder accepts and what its refusal names - a window with a second
         * vocabulary for a setting is one that can disagree with the file
         * (`crates/ipc/src/settings.rs`). This never travels: it is a gloss on
         * what was typed, not a value.
         */}
        {gloss === undefined ? '' : ` That is ${gloss}.`}
      </p>

      {/*
       * A list that could not be asked for is said, not drawn as a machine with
       * no microphone: the two are opposite answers, and only one of them means
       * "plug something in" (AGENTS.md sections 27 and 45).
       */}
      {entry.key === MICROPHONE && devices.state === 'unread' ? (
        <p className="clipped-muted" role="status">
          This machine’s microphones could not be listed, so only the two choices above are offered.{' '}
          {describeSettingsProblem(devices.problem)}
        </p>
      ) : null}

      <NotYetInForce entry={entry} />
    </div>
  );
}

/**
 * What is still in force, for a value that is saved and not yet the one being
 * used.
 *
 * The recording directory is the only setting that can be in this state, and it
 * is for the length of one sitting: where automatic recordings are written moves
 * between sittings and never during one, so that a sitting’s session record is
 * never separated from the files it names (AGENTS.md section 56, issue #609).
 * Without this the control looks as though it did nothing — the folder on screen
 * is the one that was saved, and the footage is going somewhere else (AGENTS.md
 * section 27).
 *
 * The recorder’s own sentence, because only the recorder knows what is in force.
 * `role="status"` so that a screen reader is told when it appears, which is
 * immediately after the save it explains.
 */
export function NotYetInForce({ entry }: { readonly entry: SettingEntry }): ReactNode {
  if (entry.not_yet_in_force === undefined) {
    return null;
  }
  return (
    <p className="clipped-muted" role="status">
      {entry.not_yet_in_force}
    </p>
  );
}

/** One setting the file can carry and nothing reads: its value, and why. */
export function NotInForce({ entry }: { readonly entry: SettingEntry }): ReactNode {
  return (
    <div className="clipped-field">
      <p className="clipped-field__label">{entry.label}</p>
      <p>
        <code className="clipped-code">{entry.value}</code>
      </p>
      {/* The recorder's own sentence: only it knows what would have to land. */}
      <p className="clipped-muted">{entry.unavailable}</p>
    </div>
  );
}

import type { AudioDevices, SettingEntry, SettingsView } from '@clipped/shared';
import { railPanelId, railTabId, SectionRail, type RailSection } from '@clipped/ui';
import { useState, type ReactNode } from 'react';

import { HotkeyList } from './HotkeyList';
import type { LibraryRead } from './library';
import { StartAtLoginSwitch } from './StartAtLoginSwitch';
import {
  chooseRecordingDirectory,
  DEFAULT_DEVICE,
  describeSettingsProblem,
  DEVICE_PREFIX,
  deviceNamed,
  HOTKEYS_SECTION,
  MICROPHONE,
  NO_DEVICE,
  RECORDING_DIRECTORY,
  SETTINGS_SECTIONS,
  STARTUP_SECTION,
  useAudioDevices,
  useSettings,
  type SettingRow,
  type SettingsSection,
} from './settings';

/**
 * The Settings screen (issue #51).
 *
 * The deck draws a rail of sections and a pane of controls, and the controls
 * are here because the settings are now reachable: the recorder owns
 * `settings.json` and answers `get_settings`, `apply_settings` and
 * `get_audio_devices`, so this window shows what is in force and changes it
 * without ever reading or validating that file itself (`settings.ts`).
 *
 * Two rules decide what is drawn, and both come from the recorder rather than
 * from anything decided here:
 *
 * - a setting is a **control** only when the recorder says something reads it
 *   when a recording starts. One that nothing reads is drawn as its value and
 *   the recorder's sentence saying what it is waiting for, because a control
 *   that silently changed nothing is the defect AGENTS.md section 27 is about;
 *   - a setting's **options** are the ones it said it accepts. The window keeps
 *   no list of codecs, and the microphones are this machine's, asked for at the
 *   moment the screen is opened.
 *
 * What is left in each section's table is the settings SPEC.md asks for that
 * have nowhere to be saved, or nothing behind them, each naming the issue that
 * would build it.
 */

/** The name the rail's element ids are built from. */
const RAIL = 'settings';

/** The rail's entries, which are the sections themselves. */
const RAIL_SECTIONS: readonly RailSection[] = SETTINGS_SECTIONS.map((section) => ({
  id: section.id,
  label: section.label,
}));

/** The element id a setting's control has, so its label can name it. */
function fieldId(key: string): string {
  return `setting-${key}`;
}

/** The element id a setting's hint has, so its control can be described by it. */
function hintId(key: string): string {
  return `setting-${key}-hint`;
}

/** What a microphone setting offers: the two words, then this machine's devices. */
function microphoneOptions(
  entry: SettingEntry,
  devices: LibraryRead<AudioDevices>,
): readonly { readonly value: string; readonly label: string }[] {
  const listed = devices.state === 'read' ? devices.value.microphones : [];
  const options = [
    { value: DEFAULT_DEVICE, label: 'Whichever Windows is using' },
    { value: NO_DEVICE, label: 'Do not record a microphone' },
    ...listed.map((device) => ({
      value: `${DEVICE_PREFIX}${device.name}`,
      label: device.is_default ? `${device.name} (default)` : device.name,
    })),
  ];

  // A configured device that is not in the list is one that is unplugged, and
  // it stays on offer: dropping it would silently change what is recorded to
  // whatever the list happened to start with (AGENTS.md section 27).
  const named = deviceNamed(entry.value);
  if (named !== undefined && !options.some((option) => option.value === entry.value)) {
    options.push({ value: entry.value, label: `${named} (not connected)` });
  }
  return options;
}

/** One setting the recorder says is in force: a control, and what it accepts. */
function Field({
  entry,
  devices,
  edited,
  onEdit,
  onReset,
}: {
  readonly entry: SettingEntry;
  readonly devices: LibraryRead<AudioDevices>;
  readonly edited: string | undefined;
  readonly onEdit: (value: string) => void;
  readonly onReset: () => void;
}): ReactNode {
  const value = edited ?? entry.value;
  const options = entry.key === MICROPHONE ? microphoneOptions(entry, devices) : undefined;
  const choices = entry.choices ?? [];

  return (
    <div className="clipped-field">
      <label className="clipped-field__label" htmlFor={fieldId(entry.key)}>
        {entry.label}
      </label>

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
        {entry.accepted}
        {entry.overridden ? '' : ' Nothing has changed this, so it is what Clipped ships with.'}
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
    </div>
  );
}

/** One setting the file can carry and nothing reads: its value, and why. */
function NotInForce({ entry }: { readonly entry: SettingEntry }): ReactNode {
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

/** One setting this window cannot offer at all, as a row of the account. */
function Row({ row }: { readonly row: SettingRow }): ReactNode {
  return (
    <tr>
      <th scope="row">
        {row.label}
        {row.key ? (
          <>
            {' '}
            <code className="clipped-code">{row.key.name}</code>
          </>
        ) : null}
      </th>
      <td>
        {row.today}
        {row.run ? (
          <>
            {' '}
            <code className="clipped-code">{row.run}</code>
          </>
        ) : null}
      </td>
      <td className="clipped-muted">{row.needs}</td>
    </tr>
  );
}

/** The pane one section of the rail opens. */
function Pane({
  section,
  settings,
  devices,
  edits,
  onEdit,
  onSave,
  onDiscard,
  onReset,
  saving,
  refusal,
}: {
  readonly section: SettingsSection;
  readonly settings: LibraryRead<SettingsView>;
  readonly devices: LibraryRead<AudioDevices>;
  readonly edits: Readonly<Record<string, string>>;
  readonly onEdit: (key: string, value: string) => void;
  readonly onSave: (keys: readonly string[]) => void;
  readonly onDiscard: () => void;
  readonly onReset: (key: string) => void;
  readonly saving: boolean;
  readonly refusal: string | undefined;
}): ReactNode {
  const entries =
    settings.state === 'read'
      ? section.keys.flatMap((key) => {
          const entry = settings.value.settings.find((candidate) => candidate.key === key);
          return entry === undefined ? [] : [entry];
        })
      : [];
  const changed = entries.some((entry) => edits[entry.key] !== undefined);

  return (
    <div
      className="clipped-screen__pane"
      id={railPanelId(RAIL, section.id)}
      role="tabpanel"
      aria-labelledby={railTabId(RAIL, section.id)}
      /*
       * A tab stop of its own, which is what WAI-ARIA asks of a tab panel that
       * may hold no focusable element: without it, tabbing off the rail into a
       * section with nothing but an account in it leaves the window entirely.
       *
       * Suppressed rather than dropped because the rule itself agrees — its own
       * default options are `roles: ['tabpanel']`, and `jsx-a11y`'s strict
       * preset restates the rule with no options at all, which is what takes
       * that allowance away. AGENTS.md section 42: a local suppression, with
       * the reason.
       */
      // eslint-disable-next-line jsx-a11y/no-noninteractive-tabindex
      tabIndex={0}
    >
      <h2 className="clipped-screen__heading">{section.label}</h2>
      <p className="clipped-screen__lead clipped-muted">{section.lead}</p>

      {section.keys.length > 0 && settings.state === 'reading' ? (
        <p className="clipped-panel__body">Asking the recorder…</p>
      ) : null}

      {section.keys.length > 0 && settings.state === 'unread' ? (
        <p className="clipped-panel__body" role="status">
          {describeSettingsProblem(settings.problem)}
        </p>
      ) : null}

      {entries.length > 0 ? (
        <form
          aria-label={`${section.label} settings`}
          onSubmit={(event) => {
            event.preventDefault();
            onSave(entries.map((entry) => entry.key));
          }}
        >
          {entries.map((entry) =>
            entry.applies ? (
              <Field
                key={entry.key}
                entry={entry}
                devices={devices}
                edited={edits[entry.key]}
                onEdit={(value) => {
                  onEdit(entry.key, value);
                }}
                onReset={() => {
                  onReset(entry.key);
                }}
              />
            ) : (
              <NotInForce key={entry.key} entry={entry} />
            ),
          )}

          {/*
           * One refusal, where the changes are, and the recorder's own words:
           * it names the setting, the value and what would have been accepted
           * (AGENTS.md section 45). What was typed stays, so it can be fixed.
           */}
          {refusal === undefined ? null : (
            <p className="clipped-panel__body" role="alert">
              {refusal}
            </p>
          )}

          <div className="clipped-field">
            <button
              type="submit"
              className="clipped-btn clipped-btn--primary"
              disabled={!changed || saving}
            >
              {saving ? 'Saving…' : 'Save changes'}
            </button>
            {changed ? (
              <button
                type="button"
                className="clipped-btn clipped-btn--secondary"
                onClick={onDiscard}
              >
                Discard changes
              </button>
            ) : null}
          </div>
        </form>
      ) : null}

      {/*
       * The one section with live state of its own. A hotkey the user cannot
       * have is a key that does nothing and says nothing, and the recorder is
       * the only process that knows (issue #232).
       */}
      {section.id === HOTKEYS_SECTION ? <HotkeyList /> : null}

      {/*
       * The other one, and not a setting either: a Run value Windows reads at
       * sign-in rather than a key in the settings file, written by the recorder
       * because the recorder is the executable it names (issue #308).
       */}
      {section.id === STARTUP_SECTION ? <StartAtLoginSwitch /> : null}

      {section.rows.length > 0 ? (
        <table className="clipped-table">
          <thead>
            <tr>
              <th scope="col">Setting</th>
              <th scope="col">How it is set today</th>
              <th scope="col">What this window needs first</th>
            </tr>
          </thead>
          <tbody>
            {section.rows.map((row) => (
              <Row key={row.label} row={row} />
            ))}
          </tbody>
        </table>
      ) : null}
    </div>
  );
}

/** The Settings screen. */
export function SettingsScreen(): ReactNode {
  const settings = useSettings();
  const devices = useAudioDevices();
  const [openId, setOpenId] = useState(SETTINGS_SECTIONS[0]?.id ?? '');
  const [edits, setEdits] = useState<Record<string, string>>({});
  const open = SETTINGS_SECTIONS.find((section) => section.id === openId) ?? SETTINGS_SECTIONS[0];

  /**
   * Saves what was edited in one section.
   *
   * Only that section's settings, because that is what the button in front of
   * somebody says it does: an edit left behind in another pane is theirs to
   * come back to, not something a Save over here should send on their behalf.
   *
   * Nothing is translated on the way out. A control's value is already the
   * words the settings file spells that setting in — a microphone is
   * `name:Shure MV7` because that is the option's value — so a translation here
   * would be a second vocabulary to keep in step (`crates/ipc/src/settings.rs`).
   */
  const save = (keys: readonly string[]): void => {
    const values: Record<string, string | null> = {};
    for (const key of keys) {
      const edited = edits[key];
      if (edited !== undefined) {
        values[key] = edited;
      }
    }

    void settings.apply(values).then((saved) => {
      // Cleared only for what the recorder took, and only what was sent: on a
      // refusal the edits stay so that a bad value can be corrected rather than
      // retyped.
      if (saved) {
        setEdits((current) =>
          Object.fromEntries(Object.entries(current).filter(([key]) => !keys.includes(key))),
        );
      }
    });
  };

  return (
    <>
      <h1 className="clipped-screen__title">Settings</h1>

      <p className="clipped-screen__lead">
        Clipped’s settings are global, with per-game overrides on top of them. The recorder holds
        them, and applies them to a recording when a game launches.
      </p>

      {settings.read.state === 'read' ? (
        <p className="clipped-muted">
          Kept in <code className="clipped-code">{settings.read.value.file}</code>, which the
          recorder owns.
        </p>
      ) : null}

      <div className="clipped-screen__split">
        <SectionRail
          label="Settings sections"
          name={RAIL}
          sections={RAIL_SECTIONS}
          currentId={open?.id ?? ''}
          onSelect={(section) => {
            setOpenId(section.id);
          }}
        />
        {open ? (
          <Pane
            section={open}
            settings={settings.read}
            devices={devices}
            edits={edits}
            onEdit={(key, value) => {
              setEdits((current) => ({ ...current, [key]: value }));
            }}
            onSave={save}
            onDiscard={() => {
              setEdits({});
            }}
            onReset={(key) => {
              // An edit to the setting being reset is dropped: Reset is what
              // the user asked for, and leaving a half-typed value behind it
              // would send that value with the next Save.
              setEdits((current) =>
                Object.fromEntries(Object.entries(current).filter(([edited]) => edited !== key)),
              );
              void settings.apply({ [key]: null });
            }}
            saving={settings.save.state === 'saving'}
            refusal={
              settings.save.state === 'refused'
                ? describeSettingsProblem(settings.save.problem)
                : undefined
            }
          />
        ) : null}
      </div>
    </>
  );
}

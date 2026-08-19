import type { AudioDevices, SettingsView, StorageReport } from '@clipped/shared';
import { railPanelId, railTabId, SectionRail, type RailSection } from '@clipped/ui';
import { useState, type ReactNode } from 'react';
import { Link } from 'react-router';

import { HotkeyList } from './HotkeyList';
import { PerGameSettings } from './PerGameSettings';
import { Field, NotInForce } from './SettingField';
import { StorageAccount } from './StorageAccount';
import { asProblem, describeProblem, type LibraryProblem, type LibraryRead } from './library';
import { StartAtLoginSwitch } from './StartAtLoginSwitch';
import { limitsFrom, previewLimits, size, LIMIT_KEYS } from './storage';
import type { SettingScope } from './SettingField';
import {
  describeSettingsProblem,
  HOTKEYS_SECTION,
  PER_GAME_SECTION,
  SETTINGS_SECTIONS,
  STARTUP_SECTION,
  STORAGE_SECTION,
  useAudioDevices,
  useSettings,
  type SettingRow,
  type SettingsSection,
} from './settings';
import { SETUP_PATH } from './setup';

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

/**
 * The scope every control on this screen is drawn in.
 *
 * The global settings, always: the per-game page is `PerGameSettings`, and it
 * holds a read and a scope of its own. Named here so that a control on this
 * screen cannot silently be drawn as though it were on a game's page.
 */
const GLOBAL: SettingScope = { kind: 'global' };

/** The rail's entries, which are the sections themselves. */
const RAIL_SECTIONS: readonly RailSection[] = SETTINGS_SECTIONS.map((section) => ({
  id: section.id,
  label: section.label,
}));

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

/**
 * What a Save is waiting on, when it is waiting on something.
 *
 * Only the storage limits ever put a Save here. `measuring` is the recorder
 * being asked what the limit would take; `confirm` is that answer, and it is not
 * a warning but a list of somebody's recordings; `unmeasured` is the question
 * having failed, which is a different thing from the answer being "nothing" and
 * must not be drawn as one (AGENTS.md section 27).
 */
type Asking =
  | { readonly state: 'measuring'; readonly keys: readonly string[] }
  | {
      readonly state: 'confirm';
      readonly keys: readonly string[];
      readonly report: StorageReport;
    }
  | {
      readonly state: 'unmeasured';
      readonly keys: readonly string[];
      readonly problem: LibraryProblem;
    };

/**
 * What saving a storage limit would delete, and the two ways out of it.
 *
 * Two presses rather than one, and the second says what it is about to do -
 * the same shape the Library screen empties the trash with, because it is the
 * same kind of decision. What is different is where the sentence comes from:
 * the count and the size are the recorder's dry run rather than this window's
 * arithmetic, so they are what a sweep would actually take (issue #529).
 *
 * The recordings go to the trash rather than away, and the message says so and
 * says where to get them back. That is not a softening: it is the difference
 * between this and something irreversible, and somebody deciding needs it
 * (SPEC.md section 28, AGENTS.md section 45).
 */
function LimitConfirmation({
  asking,
  onConfirm,
  onCancel,
}: {
  readonly asking: Asking | undefined;
  readonly onConfirm: (keys: readonly string[]) => void;
  readonly onCancel: () => void;
}): ReactNode {
  if (asking === undefined) {
    return null;
  }

  if (asking.state === 'measuring') {
    return (
      <p className="clipped-panel__body" aria-busy="true">
        Working out what this would delete…
      </p>
    );
  }

  if (asking.state === 'unmeasured') {
    return (
      <p className="clipped-panel__body" role="alert">
        Clipped could not work out what this limit would delete. {describeProblem(asking.problem)}{' '}
        Saving it will let automatic cleanup act on it without anybody having seen what it would
        take.{' '}
        <button
          type="button"
          className="clipped-btn clipped-btn--secondary"
          onClick={() => {
            onConfirm(asking.keys);
          }}
        >
          Save it anyway
        </button>{' '}
        <button type="button" className="clipped-btn clipped-btn--secondary" onClick={onCancel}>
          Leave it as it is
        </button>
      </p>
    );
  }

  return (
    <p className="clipped-panel__body" role="alert">
      Saving this would move {String(asking.report.would_delete.total)} recording(s),{' '}
      {size(asking.report.would_delete.total_bytes)}, to the trash — the oldest first, and nothing
      that is protected. You can restore them from the Library screen until the trash is emptied.
      {asking.report.still_over_limit > 0
        ? ` It would still be ${size(asking.report.still_over_limit)} over afterwards, because everything else is protected.`
        : ''}{' '}
      <button
        type="button"
        className="clipped-btn clipped-btn--secondary"
        onClick={() => {
          onConfirm(asking.keys);
        }}
      >
        Save the limit
      </button>{' '}
      <button type="button" className="clipped-btn clipped-btn--secondary" onClick={onCancel}>
        Keep the recordings
      </button>
    </p>
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
  confirmation,
  saved,
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
  /**
   * What has to be agreed to before Save sends anything, where anything does.
   *
   * Built by the screen rather than here, because it is about the save rather
   * than about the section: only one Save in this window can delete somebody's
   * recordings, and what it would delete is a question only the recorder can
   * answer (`storage.ts`, issue #95).
   */
  readonly confirmation: ReactNode;
  /** Bumped after each save, so the measured panel follows what was applied. */
  readonly saved: number;
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
                scope={GLOBAL}
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

          {confirmation}

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

      {/*
       * And the third: the measurement the limits above act on. Not a setting
       * either - it is what the recorder found on the disk, and it is what makes
       * the three fields above something somebody can set having looked rather
       * than guessed (SPEC.md section 27, issue #95).
       */}
      {section.id === STORAGE_SECTION ? <StorageAccount refreshOn={saved} /> : null}

      {/*
       * And the fourth: one game's settings, read and saved against that game's
       * own layer rather than the global one. It holds its own read because it
       * is a different question — `get_settings` for a game — and folding two
       * pages together in this window would destroy the one fact it draws
       * (issue #63).
       */}
      {section.id === PER_GAME_SECTION ? <PerGameSettings devices={devices} /> : null}

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
  const [asking, setAsking] = useState<Asking | undefined>(undefined);
  const [saved, setSaved] = useState(0);
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

    void settings.apply(values).then((applied) => {
      // Cleared only for what the recorder took, and only what was sent: on a
      // refusal the edits stay so that a bad value can be corrected rather than
      // retyped.
      if (applied) {
        setEdits((current) =>
          Object.fromEntries(Object.entries(current).filter(([key]) => !keys.includes(key))),
        );
        // The measured panel is now describing the limits from before this save,
        // so it is asked again. Only on success: a refused change did not happen
        // and nothing has moved.
        setSaved((count) => count + 1);
      }
    });
  };

  /**
   * Saves, first asking what a storage limit would delete.
   *
   * This is the one Save in the window that can destroy somebody's recordings.
   * A maximum usage is enforced by a sweep that runs after every reconciliation,
   * so saving one is not a preference taking effect later - it is footage on its
   * way to the trash, and somebody has to be told which before it goes
   * (SPEC.md section 27, AGENTS.md section 56).
   *
   * The question is the recorder's own dry run, which is the measurement the
   * sweep itself takes: what the screen shows before the limit is saved cannot
   * disagree with what happens after it is (issue #529, AGENTS.md section 55).
   *
   * A limit that would take nothing is saved without a confirmation. A dialog
   * that appears whatever the answer teaches people to dismiss it, and then it
   * is not a confirmation of anything (AGENTS.md section 27).
   *
   * A measurement that fails does **not** save quietly. It cannot be told from
   * one that would delete nothing, and the two are opposite; so it says what
   * went wrong and offers the save as something to choose (AGENTS.md section 45).
   */
  const saveWithCare = (keys: readonly string[]): void => {
    const editedLimits = keys.filter(
      (key) => LIMIT_KEYS.includes(key as (typeof LIMIT_KEYS)[number]) && edits[key] !== undefined,
    );
    if (editedLimits.length === 0) {
      save(keys);
      return;
    }

    // What the three limits would be after this save: the fields as they now
    // stand, edited or not, because a sweep is judged against all three at once
    // and previewing only what changed would ask a different question.
    const proposed: Record<string, string> = {};
    if (settings.read.state === 'read') {
      for (const entry of settings.read.value.settings) {
        if (LIMIT_KEYS.includes(entry.key as (typeof LIMIT_KEYS)[number])) {
          proposed[entry.key] = edits[entry.key] ?? entry.value;
        }
      }
    }

    setAsking({ state: 'measuring', keys });
    previewLimits(limitsFrom(proposed))
      .then((report) => {
        if (report.would_delete.total === 0) {
          setAsking(undefined);
          save(keys);
          return;
        }
        setAsking({ state: 'confirm', keys, report });
      })
      .catch((thrown: unknown) => {
        setAsking({ state: 'unmeasured', keys, problem: asProblem(thrown) });
      });
  };

  const confirmation = (
    <LimitConfirmation
      asking={asking}
      onConfirm={(keys) => {
        setAsking(undefined);
        save(keys);
      }}
      onCancel={() => {
        setAsking(undefined);
      }}
    />
  );

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
            onSave={saveWithCare}
            confirmation={confirmation}
            saved={saved}
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

      {/*
       * The way back to the first run (issue #109). A destination rather than a
       * control that changes something: it clears nothing and resets nothing,
       * it walks the two questions again with what is saved already filled in,
       * and Finish saves them the same way this screen would.
       *
       * After the rail rather than before it, because the rail is this screen's
       * first tab stop and a link ahead of it would put a stop between the
       * sidebar and the sections every time somebody tabbed in.
       */}
      <p>
        <Link to={SETUP_PATH}>Run setup again</Link>
      </p>
    </>
  );
}

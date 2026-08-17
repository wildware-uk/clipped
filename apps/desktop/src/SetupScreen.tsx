import type { HotkeyBinding, SettingEntry } from '@clipped/shared';
import { useState, type ReactNode } from 'react';
import { useNavigate } from 'react-router';

import { describeCondition, useHotkeys } from './hotkeys';
import type { LibraryRead } from './library';
import {
  chooseRecordingDirectory,
  describeSettingsProblem,
  MICROPHONE,
  microphoneOptions,
  NO_DEVICE,
  RECORDING_DIRECTORY,
  useAudioDevices,
  useSettings,
  type MicrophoneOption,
} from './settings';
import {
  describeLevel,
  meterFraction,
  setupEntries,
  useMicrophoneLevel,
  type LevelReading,
} from './setup';

/**
 * The first run (issue #109).
 *
 * SPEC.md section 45 is the walkthrough this exists for, and its first four
 * steps are the whole scope: install, launch once, **select microphone and
 * recording directory**, close the window. Everything else Clipped can be
 * configured with has a shipped default and a control on the Settings screen,
 * and putting it here would make the first thing somebody sees longer without
 * making the walkthrough more likely to work.
 *
 * Nothing on this screen is new machinery. The settings come from
 * `get_settings`, the device list from `get_audio_devices`, the folder picker
 * from the same dialog the Settings screen opens, and Finish is one
 * `apply_settings` — the same three commands the Settings screen uses, through
 * the same hooks (`settings.ts`, AGENTS.md section 55). The one thing that is
 * new is the meter, because a list of device names cannot answer the question
 * somebody choosing a microphone is actually asking.
 *
 * # Why every field starts filled in
 *
 * Because the issue asks that the flow can be completed by accepting what is
 * detected, and because the recorder already resolves both to something usable:
 * a folder under the user's Videos directory and whichever microphone Windows
 * considers default. Finish then writes both **explicitly**, including when they
 * were not changed. That is deliberate rather than lazy: an explicit value is
 * what {@link setupIsNeeded} reads to know this profile has been through setup,
 * and it is the truth — somebody looked at these two answers and accepted them.
 */

/** One step of the flow. */
interface Step {
  /** Stable identifier, used in the heading's element id. */
  readonly id: string;
  /** What the step is called. */
  readonly label: string;
}

/** One step per question, then the one that is not a question. */
const RECORDINGS_STEP: Step = { id: 'recordings', label: 'Where recordings go' };
const MICROPHONE_STEP: Step = { id: 'microphone', label: 'Your microphone' };
const READY_STEP: Step = { id: 'ready', label: 'Ready' };

/**
 * The steps this recorder's settings make, in order.
 *
 * Built from what came back rather than fixed, because a recorder older than
 * this window may not send one of the two — and a step with no field in it is a
 * step somebody would press Continue on wondering what they had missed. The
 * settings screen leaves out a key the recorder did not send for the same
 * reason (`settings.ts`).
 */
function stepsFor(keys: readonly string[]): readonly Step[] {
  return [
    ...(keys.includes(RECORDING_DIRECTORY) ? [RECORDINGS_STEP] : []),
    ...(keys.includes(MICROPHONE) ? [MICROPHONE_STEP] : []),
    READY_STEP,
  ];
}

/** The action the replay hotkey performs, as the recorder names it. */
const SAVE_REPLAY = 'save_replay';

/**
 * The element id a step's heading has, so its section can be named by it.
 *
 * Prefixed apart from {@link fieldId} deliberately: a step is called
 * `microphone` and so is the setting it asks about, and two elements sharing an
 * id sends a field's `<label for>` to whichever came first — which was the
 * heading, leaving the control with no accessible name at all.
 */
function stepId(id: string): string {
  return `setup-step-${id}`;
}

/** The element id a control has, so its label can name it. */
function fieldId(key: string): string {
  return `setup-${key}`;
}

/** The element id a hint has, so its control can be described by it. */
function hintId(key: string): string {
  return `setup-${key}-hint`;
}

/** Where recordings are written. */
function RecordingsStep({
  entry,
  value,
  onChange,
}: {
  readonly entry: SettingEntry;
  readonly value: string;
  readonly onChange: (value: string) => void;
}): ReactNode {
  return (
    <>
      <p className="clipped-screen__lead clipped-muted">
        Clipped writes every recording here, and organises them by game. Pick a drive with room on
        it — an hour of gameplay is several gigabytes.
      </p>

      <div className="clipped-field">
        <label className="clipped-field__label" htmlFor={fieldId(RECORDING_DIRECTORY)}>
          {entry.label}
        </label>
        <input
          className="clipped-input"
          id={fieldId(RECORDING_DIRECTORY)}
          aria-describedby={hintId(RECORDING_DIRECTORY)}
          type="text"
          value={value}
          onChange={(event) => {
            onChange(event.target.value);
          }}
        />
        <button
          type="button"
          className="clipped-btn clipped-btn--secondary"
          onClick={() => {
            void chooseRecordingDirectory(value).then((chosen) => {
              // Dismissed is not a choice, and must not clear what is there.
              if (chosen !== null) {
                onChange(chosen);
              }
            });
          }}
        >
          Browse…
        </button>
        <p className="clipped-muted" id={hintId(RECORDING_DIRECTORY)}>
          {entry.accepted}
          {entry.overridden ? '' : ' This is where Clipped would put them if you changed nothing.'}
        </p>
      </div>
    </>
  );
}

/** The meter, and the sentence that says what it means. */
function Level({ reading }: { readonly reading: LevelReading }): ReactNode {
  const peak = reading.state === 'heard' ? reading.level.peak : 0;

  return (
    <div className="clipped-field">
      <p className="clipped-field__label" id="setup-level-label">
        Input level
      </p>
      {/*
       * A native meter: it is the element this is, it is announced as one, and
       * it needs no stylesheet of its own. The value is the peak on a decibel
       * scale rather than the raw amplitude, because a linear bar barely moves
       * for speech (`meterFraction`).
       */}
      <meter
        aria-labelledby="setup-level-label"
        aria-describedby="setup-level-detail"
        min={0}
        max={1}
        value={meterFraction(peak)}
      />
      {/*
       * The sentence, and not only the bar. A meter alone cannot say that the
       * device is muted or unplugged, which are the two reasons it would sit
       * still while somebody talks at it, and it says nothing at all to a
       * screen reader as it moves (AGENTS.md sections 27 and 28).
       */}
      <p className="clipped-muted" id="setup-level-detail" role="status">
        {describeLevel(reading)}
      </p>
    </div>
  );
}

/** Which microphone to record, and proof that it works. */
function MicrophoneStep({
  entry,
  value,
  options,
  onChange,
}: {
  readonly entry: SettingEntry;
  readonly value: string;
  readonly options: readonly MicrophoneOption[];
  readonly onChange: (value: string) => void;
}): ReactNode {
  // Asked for the value on screen rather than the one saved, so that switching
  // device moves the meter to the device somebody is considering.
  const reading = useMicrophoneLevel(value);
  const recorded = value !== NO_DEVICE;

  return (
    <>
      <p className="clipped-screen__lead clipped-muted">
        Your microphone is recorded onto a track of its own, so it can be edited apart from the game
        and from everything else the machine played (SPEC.md section 12).
      </p>

      <div className="clipped-field">
        <label className="clipped-field__label" htmlFor={fieldId(MICROPHONE)}>
          {entry.label}
        </label>
        <select
          className="clipped-input"
          id={fieldId(MICROPHONE)}
          aria-describedby={hintId(MICROPHONE)}
          value={value}
          onChange={(event) => {
            onChange(event.target.value);
          }}
        >
          {options.map((option) => (
            <option key={option.value} value={option.value}>
              {option.label}
            </option>
          ))}
        </select>
        <p className="clipped-muted" id={hintId(MICROPHONE)}>
          {entry.accepted}
        </p>
      </div>

      {/*
       * No meter for "record no microphone": there is nothing to listen to, and
       * a bar sitting at zero would look like a device that had failed rather
       * than a choice somebody made (AGENTS.md section 27).
       */}
      {recorded ? (
        <Level reading={reading} />
      ) : (
        <p className="clipped-panel__body">
          No microphone will be recorded. The game and the rest of the machine still get tracks of
          their own.
        </p>
      )}
    </>
  );
}

/** What the replay hotkey is, and whether pressing it would do anything. */
function ReplayHotkey({
  read,
}: {
  readonly read: LibraryRead<readonly HotkeyBinding[]>;
}): ReactNode {
  if (read.state === 'reading') {
    return <p className="clipped-panel__body">Asking the recorder…</p>;
  }
  if (read.state === 'unread') {
    // Not a failure of setup: the hotkey is registered by the recorder whether
    // or not this window could ask about it, and saying nothing would be worse
    // than saying that it could not be checked.
    return (
      <p className="clipped-panel__body" role="status">
        Clipped could not check the replay hotkey. It is in Settings once this window can reach the
        recorder.
      </p>
    );
  }

  const replay = read.value.find((row) => row.action === SAVE_REPLAY);
  if (replay === undefined) {
    return (
      <p className="clipped-panel__body">
        This recorder does not report a replay hotkey. Settings lists whatever it does register.
      </p>
    );
  }

  return (
    <p className="clipped-panel__body">
      {replay.label}:{' '}
      {replay.hotkey === undefined ? (
        'not bound to anything'
      ) : (
        <code className="clipped-code">{replay.hotkey}</code>
      )}
      {'. '}
      {/*
       * The recorder's own sentence. A combination Discord already owns is a
       * key that does nothing, and this window is not the process that knows —
       * the recorder is, at the moment it registered (`hotkeys.ts`).
       */}
      {describeCondition(replay)}
    </p>
  );
}

/** What was chosen, what happens next, and the key to press. */
function ReadyStep({
  directory,
  microphone,
  options,
}: {
  readonly directory: string;
  readonly microphone: string;
  readonly options: readonly MicrophoneOption[];
}): ReactNode {
  const hotkeys = useHotkeys();
  const chosen = options.find((option) => option.value === microphone);

  return (
    <>
      <p className="clipped-screen__lead clipped-muted">
        Close this window when you are done. Clipped keeps running, notices the game you launch, and
        records it — there is nothing to start.
      </p>

      <dl className="clipped-panel__body">
        <dt className="clipped-field__label">Recordings</dt>
        <dd>
          <code className="clipped-code">{directory}</code>
        </dd>
        <dt className="clipped-field__label">Microphone</dt>
        <dd>{chosen?.label ?? microphone}</dd>
      </dl>

      {/*
       * Said rather than offered. The recorder reports `capture_target` as a
       * setting nothing reads — every recording captures the game's own window
       * until issue #61 — so a control for it here would be the first thing a
       * user ever saw and would do nothing (AGENTS.md section 27).
       */}
      <p className="clipped-panel__body">
        Clipped records the game’s own window, with the game’s audio, the rest of the machine’s
        audio and your microphone each on a track of its own.
      </p>

      <ReplayHotkey read={hotkeys} />
    </>
  );
}

/**
 * The first run.
 *
 * Nothing is written until Finish, and Finish writes both answers together: the
 * recorder applies a whole request or none of it, so a folder it refuses cannot
 * leave a microphone saved beside it (`crates/ipc/src/settings.rs`).
 */
export function SetupScreen(): ReactNode {
  const settings = useSettings();
  const devices = useAudioDevices();
  const navigate = useNavigate();
  const [stepIndex, setStepIndex] = useState(0);
  const [edits, setEdits] = useState<Record<string, string>>({});

  const entries = settings.read.state === 'read' ? setupEntries(settings.read.value) : [];
  const directoryEntry = entries.find((entry) => entry.key === RECORDING_DIRECTORY);
  const microphoneEntry = entries.find((entry) => entry.key === MICROPHONE);

  // The recorder's resolved value is the starting point for every field, which
  // is what makes "accept what was detected" a thing somebody can do.
  const valueOf = (entry: SettingEntry | undefined): string =>
    entry === undefined ? '' : (edits[entry.key] ?? entry.value);
  const directory = valueOf(directoryEntry);
  const microphone = valueOf(microphoneEntry);

  const options = microphoneEntry === undefined ? [] : microphoneOptions(microphoneEntry, devices);

  /*
   * The one answer the recorder cannot resolve for us: a blank directory means
   * the machine described no Videos directory at all, and Save would be
   * refused. Only a question that was actually asked can block Finish — a
   * recorder that never sent the setting has not left it blank.
   */
  const missingDirectory = directoryEntry !== undefined && directory === '';

  const steps = stepsFor(entries.map((row) => row.key));
  const step = steps[Math.min(stepIndex, steps.length - 1)];
  const last = stepIndex >= steps.length - 1;

  const edit = (key: string, value: string): void => {
    setEdits((current) => ({ ...current, [key]: value }));
  };

  /**
   * Saves both answers, and leaves.
   *
   * Only on success. A refused value has to stay on screen to be corrected, and
   * a flow that navigated away regardless would leave somebody on the Home
   * screen believing they had finished (AGENTS.md section 54).
   */
  const finish = (): void => {
    const values: Record<string, string | null> = {};
    if (directoryEntry !== undefined) {
      values[directoryEntry.key] = directory;
    }
    if (microphoneEntry !== undefined) {
      values[microphoneEntry.key] = microphone;
    }

    void settings.apply(values).then((saved) => {
      if (saved) {
        void navigate('/', { replace: true });
      }
    });
  };

  return (
    <>
      <h1 className="clipped-screen__title">Set up Clipped</h1>

      <p className="clipped-screen__lead">
        Two answers, and Clipped records every game you play without being asked again. Both are in
        Settings afterwards.
      </p>

      {settings.read.state === 'reading' ? (
        <p className="clipped-panel__body">Asking the recorder…</p>
      ) : null}

      {settings.read.state === 'unread' ? (
        <p className="clipped-panel__body" role="alert">
          {describeSettingsProblem(settings.read.problem)}
        </p>
      ) : null}

      {/*
       * Drawn only when the recorder sent the settings the steps are about. A
       * flow over fields that could not be read would be a flow whose Finish
       * was always going to fail.
       */}
      {entries.length > 0 && step !== undefined ? (
        <section aria-labelledby={stepId(step.id)}>
          <p className="clipped-kicker">
            Step {stepIndex + 1} of {steps.length}
          </p>
          <h2 className="clipped-screen__heading" id={stepId(step.id)}>
            {step.label}
          </h2>

          {step.id === 'recordings' && directoryEntry !== undefined ? (
            <RecordingsStep
              entry={directoryEntry}
              value={directory}
              onChange={(value) => {
                edit(directoryEntry.key, value);
              }}
            />
          ) : null}

          {step.id === 'microphone' && microphoneEntry !== undefined ? (
            <MicrophoneStep
              entry={microphoneEntry}
              value={microphone}
              options={options}
              onChange={(value) => {
                edit(microphoneEntry.key, value);
              }}
            />
          ) : null}

          {step.id === 'ready' ? (
            <ReadyStep directory={directory} microphone={microphone} options={options} />
          ) : null}

          {/*
           * The recorder's own refusal, where the button that caused it is: it
           * names the setting, the value and what would have been accepted.
           */}
          {settings.save.state === 'refused' ? (
            <p className="clipped-panel__body" role="alert">
              {describeSettingsProblem(settings.save.problem)}
            </p>
          ) : null}

          <div className="clipped-field">
            {stepIndex > 0 ? (
              <button
                type="button"
                className="clipped-btn clipped-btn--secondary"
                onClick={() => {
                  setStepIndex(stepIndex - 1);
                }}
              >
                Back
              </button>
            ) : null}

            {last ? (
              <button
                type="button"
                className="clipped-btn clipped-btn--primary"
                onClick={finish}
                // Saying so before the press is better than a refusal after
                // it.
                disabled={settings.save.state === 'saving' || missingDirectory}
              >
                {settings.save.state === 'saving' ? 'Saving…' : 'Finish setup'}
              </button>
            ) : (
              <button
                type="button"
                className="clipped-btn clipped-btn--primary"
                onClick={() => {
                  setStepIndex(stepIndex + 1);
                }}
              >
                Continue
              </button>
            )}
          </div>

          {last && missingDirectory ? (
            <p className="clipped-panel__body" role="alert">
              Clipped could not work out where to put recordings on this machine. Go back and choose
              a folder.
            </p>
          ) : null}
        </section>
      ) : null}
    </>
  );
}

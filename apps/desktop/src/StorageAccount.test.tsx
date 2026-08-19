import type { SettingEntry, SettingsView, StorageReport } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { SettingsScreen } from './SettingsScreen';
import { MAXIMUM_AGE_DAYS, MAXIMUM_USAGE, MINIMUM_FREE_SPACE } from './storage';
import { A_COUNT, textRuns } from './test/counts';
import { stubRecorderLinkRuntime, type StubbedRuntime } from './test/recorderLinkRuntime';
import schema from '../../../packages/shared/src/ipc/protocol-schema.json';

/**
 * The Storage section's contract, as tests (SPEC.md section 27, issue #95).
 *
 * Four properties, and each is one that would rot in silence.
 *
 * The first is that **every figure comes from the recorder**. A screen that drew
 * "0 bytes" or "nothing would be deleted" over a measurement nobody took would
 * be indistinguishable from one over a measurement that came back that way — and
 * the two send somebody in opposite directions, one to set a limit and one to go
 * looking for a missing drive (AGENTS.md section 27).
 *
 * The second is that **changing the recording folder says what happens to what
 * is already recorded**. Recordings are ordinary files and the library indexes
 * where each one is, so a folder change moves nothing; a screen that left that
 * to be guessed at would be a screen somebody read as "and my old recordings go
 * where?" (issue #95's second criterion).
 *
 * The third is that **a limit that would delete is confirmed first**, against
 * the recorder's own dry run rather than this window's arithmetic, and that
 * nothing is sent until it is agreed to (AGENTS.md section 56, issue #529).
 *
 * The fourth is that a **dry run that failed is not read as "nothing would be
 * deleted"**. They are opposite answers and only one of them is safe to save on.
 *
 * # Where the fixtures come from
 *
 * `protocol-schema.json`, which `cargo run -p clipped-ipc --bin protocol-schema`
 * writes from the Rust types themselves. So the report these tests drive the
 * screen with is the recorder's own exemplar, field for field — a shape typed
 * out here by hand would go on passing after the protocol moved underneath it,
 * which is the failure the whole conformance apparatus exists to prevent.
 */

/** A sample frame the Rust build produced, by the name it recorded it under. */
function sample(name: string): Record<string, unknown> {
  const samples = schema.samples as { name: string; frame: Record<string, unknown> }[];
  const found = samples.find((candidate) => candidate.name === name);
  if (found === undefined) {
    throw new Error(
      `protocol-schema.json has no sample called "${name}"; regenerate it, or the name has moved`,
    );
  }
  return found.frame;
}

/** The storage report out of one of those samples. */
function reportIn(name: string): StorageReport {
  const frame = sample(name) as { outcome: { ok: { storage: StorageReport } } };
  return frame.outcome.ok.storage;
}

/** What a recorder answers about a library it has measured. */
const MEASURED = reportIn('what the library occupies, against the limits that are configured');

/** What it answers about limits somebody is about to save and has not. */
const DRY_RUN = reportIn('a dry run of limits a window is about to save, which it cannot meet');

/** The same report with fields answered differently. */
function reportWith(changes: Partial<StorageReport>): StorageReport {
  return { ...MEASURED, ...changes };
}

/** One setting as the recorder sends it. */
function entry(overrides: Partial<SettingEntry> & Pick<SettingEntry, 'key'>): SettingEntry {
  return {
    label: overrides.key,
    value: '',
    overridden: false,
    accepted: 'something',
    applies: true,
    ...overrides,
  };
}

/**
 * The settings a recorder sends for this section.
 *
 * The four keys are imported rather than spelled, and
 * `settingsConformance.test.ts` is what holds those constants to the Rust that
 * declares them — in both directions, so a key the recorder gains and one this
 * screen invented both fail there rather than here.
 */
const SETTINGS: SettingsView = {
  file: String.raw`C:\Users\alex\AppData\Local\Clipped\settings.json`,
  settings: [
    entry({
      key: 'recording_directory',
      label: 'Recording directory',
      value: String.raw`D:\Clips`,
      accepted: 'a folder on this machine, such as D:\\Clips',
    }),
    entry({
      key: MAXIMUM_USAGE,
      label: 'Maximum usage',
      value: 'none',
      accepted: 'a number of bytes, at least 1000000000 (1 GB), or `none` to keep everything',
    }),
    entry({
      key: MINIMUM_FREE_SPACE,
      label: 'Minimum free space',
      value: 'none',
      accepted: 'a number of bytes to leave free on the drive, `0` to fill it, or `none`',
    }),
    entry({
      key: MAXIMUM_AGE_DAYS,
      label: 'Maximum recording age',
      value: 'none',
      accepted: 'a whole number of days, at least 1, or `none`',
    }),
  ],
};

/** The same, with one setting answered differently. */
function settingsWith(key: string, changes: Partial<SettingEntry>): SettingsView {
  return {
    ...SETTINGS,
    settings: SETTINGS.settings.map((candidate) =>
      candidate.key === key ? { ...candidate, ...changes } : candidate,
    ),
  };
}

/** What the window asked `recorder_storage`, in order. */
function measurements(runtime: StubbedRuntime): readonly Record<string, unknown>[] {
  return runtime.invocations
    .filter((invocation) => invocation.command === 'recorder_storage')
    .map((invocation) => invocation.args);
}

/** What the window sent to `apply_recorder_settings`, in order. */
function saved(runtime: StubbedRuntime): readonly Record<string, unknown>[] {
  return runtime.invocations
    .filter((invocation) => invocation.command === 'apply_recorder_settings')
    .map((invocation) => invocation.args['values'] as Record<string, unknown>);
}

function renderScreen(
  overrides: Parameters<typeof stubRecorderLinkRuntime>[2] = {},
): StubbedRuntime {
  const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
    recorderSettings: () => SETTINGS,
    recorderStorage: () => MEASURED,
    audioDevices: () => ({ microphones: [] }),
    recorderHotkeys: () => [],
    ...overrides,
  });
  render(
    <MemoryRouter>
      <SettingsScreen />
    </MemoryRouter>,
  );
  return runtime;
}

async function openStorage(user: UserEvent): Promise<void> {
  await user.click(screen.getByRole('tab', { name: 'Storage' }));
}

/**
 * The paragraph a named button sits in.
 *
 * Queried through the button rather than by `role="alert"`, because a refusal
 * from the recorder is an alert too — and a test that found "an alert" would
 * pass on the wrong one, which is how a broken guard looks like an ambiguous
 * query rather than like a limit that saved itself.
 */
function around(button: HTMLElement): HTMLElement {
  const paragraph = button.closest('p');
  if (paragraph === null) {
    throw new Error(`the "${button.textContent ?? ''}" button is not in a paragraph`);
  }
  return paragraph;
}

/**
 * Waits until the save has either asked something or gone through.
 *
 * Both are outcomes of pressing Save, and which one happened is the thing under
 * test — so a test cannot wait on the one it expects without passing for the
 * wrong reason when the other happens. Waiting on *either* leaves the assertion
 * about which to the assertion, where its failure names what went wrong instead
 * of reporting a button that is not on screen.
 */
async function settled(runtime: StubbedRuntime, asks: string): Promise<void> {
  await waitFor(() => {
    expect(
      saved(runtime).length + screen.queryAllByRole('button', { name: asks }).length,
      'the save neither asked anything nor sent anything',
    ).toBeGreaterThan(0);
  });
}

/**
 * Fails if anything in the panel reads as a measurement.
 *
 * `A_COUNT` alone is not enough here and finding that out was the point of
 * breaking it: it matches "12 recordings" and "4 GB" and does **not** match
 * "0 bytes", which is exactly the figure a panel drawn over a failed read would
 * invent. So this sweeps for any digit at all, and the two sentences this panel
 * shows when it has measured nothing are written without one for that reason.
 *
 * The meter and the tables are checked as well, because a bar sitting at nought
 * is a claim about a disk made with no numeral in sight.
 */
function drawsNoFigure(measured: HTMLElement): void {
  for (const run of textRuns(measured)) {
    expect(run, 'nothing measured, so nothing that reads as a count').not.toMatch(A_COUNT);
    expect(run, 'nothing measured, so no figure of any kind').not.toMatch(/\d/);
  }
  expect(
    within(measured).queryByRole('meter'),
    'a bar at nought is a claim about a disk nobody read',
  ).not.toBeInTheDocument();
  expect(within(measured).queryAllByRole('table')).toHaveLength(0);
}

/** The measured panel, once it is there. */
async function panel(): Promise<HTMLElement> {
  return waitFor(() => screen.getByRole('region', { name: 'What Clipped is using' }));
}

describe('the Storage section', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it('draws no figure at all while the library has not been measured', async () => {
    const user = userEvent.setup();
    // A measurement that never answers: the promise this returns is never
    // settled, so the panel stays in its reading state for the whole test.
    renderScreen({ recorderStorage: () => new Promise<never>(() => undefined) });
    await openStorage(user);

    await waitFor(() => {
      expect(screen.getByText('Measuring your library…')).toBeInTheDocument();
    });

    // A panel of zeroes over an unanswered question is the failure this whole
    // section is shaped to avoid.
    drawsNoFigure(await panel());
  });

  it('draws the recorder’s own figures once it has measured', async () => {
    const user = userEvent.setup();
    renderScreen();
    await openStorage(user);

    const measured = await panel();

    // 411204889112 bytes, in the unit the panel picks. Asserted through the
    // fixture rather than as a literal, so a sample the Rust build changes
    // changes this expectation with it.
    await waitFor(() => {
      expect(measured).toHaveTextContent(/411 GB/);
    });
    expect(measured).toHaveTextContent(/162 GB/);
    expect(measured).toHaveTextContent(/1000 GB|1.0 TB/);
    expect(measured).toHaveTextContent(MEASURED.recordings_directory);
    expect(measured).toHaveTextContent(MEASURED.trash_directory);
  });

  it('says what is never deleted with a figure against it, from the recorder’s own rules', async () => {
    const user = userEvent.setup();
    renderScreen();
    await openStorage(user);

    const kept = await waitFor(() =>
      screen.getByRole('table', { name: 'Never deleted automatically' }),
    );
    const group = MEASURED.protected[0];
    expect(group, 'the sample should carry a protection rule to draw').toBeDefined();
    expect(kept).toHaveTextContent(group?.label ?? '');
    expect(kept).toHaveTextContent(String(group?.recordings ?? 0));
  });

  it('lists the largest recordings, saying which of them a sweep may not take', async () => {
    const user = userEvent.setup();
    renderScreen();
    await openStorage(user);

    const largest = await waitFor(() => screen.getByRole('table', { name: 'Largest recordings' }));
    const biggest = MEASURED.largest.recordings[0];
    expect(biggest, 'the sample should carry a recording to draw').toBeDefined();
    expect(largest).toHaveTextContent('clipped-cs2-20260811-201400-1.mkv');
    expect(largest).toHaveTextContent(biggest?.protected_because ?? '');

    // And it says it is not the whole list. The sample counts 118 and names
    // one, and a table that showed one row without saying so would read as a
    // library with one recording in it.
    expect(await panel()).toHaveTextContent(/largest of 118 recordings/);
  });

  it('says a measurement that failed could not be taken, rather than drawing nothing', async () => {
    const user = userEvent.setup();
    renderScreen({
      recorderStorage: () => {
        throw {
          code: 'library_unavailable',
          message: 'the drive could not be measured: D:\\ is not there',
        };
      },
    });
    await openStorage(user);

    const measured = await panel();
    await waitFor(() => {
      expect(measured).toHaveTextContent(/could not be measured/);
    });
    drawsNoFigure(measured);
  });

  it('says what happens to footage that is already recorded when the folder changes', async () => {
    const user = userEvent.setup();
    renderScreen();
    await openStorage(user);

    const measured = await panel();
    await waitFor(() => {
      expect(measured).toHaveTextContent(/Changing the recording folder moves nothing/);
    });
    expect(measured).toHaveTextContent(/stay where they are, still play, and stay in your library/);
  });

  it('shows the recorder’s own sentence about where automatic recordings still go', async () => {
    const still = String.raw`Automatic recordings still go to D:\Old. They go here from the next session.`;
    const user = userEvent.setup();
    renderScreen({
      recorderSettings: () =>
        settingsWith('recording_directory', {
          value: String.raw`E:\Clips`,
          overridden: true,
          not_yet_in_force: still,
        }),
    });
    await openStorage(user);

    await waitFor(() => {
      expect(screen.getByText(still)).toBeInTheDocument();
    });
  });

  it('asks what a limit would delete, and sends nothing until it is agreed to', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      recorderStorage: (args) => (args['limits'] === undefined ? MEASURED : DRY_RUN),
    });
    await openStorage(user);
    await panel();

    await user.clear(screen.getByLabelText('Maximum usage'));
    await user.type(screen.getByLabelText('Maximum usage'), '250000000000');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await settled(runtime, 'Save the limit');
    // The load-bearing assertion, first: a screen that asked and saved anyway
    // would pass every assertion below it.
    expect(saved(runtime), 'nothing may be saved before the question is answered').toEqual([]);

    // The recorder's own dry run, named in the words somebody has to agree to.
    const asked = around(screen.getByRole('button', { name: 'Save the limit' }));
    expect(asked).toHaveTextContent(/Saving this would move 118 recording\(s\)/);
    expect(asked).toHaveTextContent(/411 GB/);
    // And what it could not do even so, which is what tells somebody the limit
    // will not be met however long they wait.
    expect(asked).toHaveTextContent(/still be 12 GB over/);

    await user.click(screen.getByRole('button', { name: 'Save the limit' }));
    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ [MAXIMUM_USAGE]: '250000000000' }]);
    });
  });

  it('saves nothing when the deletion is declined', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      recorderStorage: (args) => (args['limits'] === undefined ? MEASURED : DRY_RUN),
    });
    await openStorage(user);
    await panel();

    await user.clear(screen.getByLabelText('Maximum usage'));
    await user.type(screen.getByLabelText('Maximum usage'), '250000000000');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    await screen.findByRole('button', { name: 'Keep the recordings' });
    await user.click(screen.getByRole('button', { name: 'Keep the recordings' }));

    expect(saved(runtime)).toEqual([]);
    // And what was typed is still there to be corrected or reconsidered.
    expect(screen.getByLabelText('Maximum usage')).toHaveValue('250000000000');
  });

  it('asks about all three limits together, not only the one that was edited', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      recorderSettings: () => settingsWith(MAXIMUM_AGE_DAYS, { value: '90', overridden: true }),
      recorderStorage: (args) => (args['limits'] === undefined ? MEASURED : DRY_RUN),
    });
    await openStorage(user);
    await panel();

    await user.clear(screen.getByLabelText('Maximum usage'));
    await user.type(screen.getByLabelText('Maximum usage'), '250000000000');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    await screen.findByRole('button', { name: 'Save the limit' });

    // A sweep is judged against all three at once, so a dry run that asked
    // about one of them would be answering a different question from the one
    // the save is about.
    const asked = measurements(runtime).filter((args) => args['limits'] !== undefined);
    expect(asked).toEqual([
      { limits: { maximum_usage_bytes: 250000000000, maximum_age_days: 90 } },
    ]);
  });

  it('saves a limit that would delete nothing without asking anybody to agree to it', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      recorderStorage: (args) =>
        args['limits'] === undefined
          ? MEASURED
          : reportWith({
              proposed: true,
              would_delete: { total: 0, total_bytes: 0, recordings: [] },
            }),
    });
    await openStorage(user);
    await panel();

    await user.clear(screen.getByLabelText('Maximum usage'));
    await user.type(screen.getByLabelText('Maximum usage'), '900000000000');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ [MAXIMUM_USAGE]: '900000000000' }]);
    });
    // A confirmation that appeared whatever the answer would be one people
    // learn to dismiss, and then it confirms nothing.
    expect(screen.queryByRole('button', { name: 'Save the limit' })).not.toBeInTheDocument();
  });

  it('does not read a dry run that failed as a limit that would delete nothing', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      recorderStorage: (args) => {
        if (args['limits'] === undefined) {
          return MEASURED;
        }
        throw { code: 'library_unavailable', message: 'the index could not be read' };
      },
    });
    await openStorage(user);
    await panel();

    await user.clear(screen.getByLabelText('Maximum usage'));
    await user.type(screen.getByLabelText('Maximum usage'), '250000000000');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await settled(runtime, 'Save it anyway');
    // The load-bearing assertion, and it is asserted before anything about what
    // is on screen: a measurement that failed and a limit that would take
    // nothing are opposite answers, and only one of them is safe to save on.
    expect(saved(runtime), 'a failed measurement must not save quietly').toEqual([]);
    expect(around(screen.getByRole('button', { name: 'Save it anyway' }))).toHaveTextContent(
      /could not work out what this limit would delete/,
    );

    // And it is a choice rather than a dead end (AGENTS.md section 45).
    await user.click(screen.getByRole('button', { name: 'Save it anyway' }));
    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ [MAXIMUM_USAGE]: '250000000000' }]);
    });
  });

  it('measures again after a limit is saved, so the figures are of the limit in force', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      recorderStorage: (args) =>
        args['limits'] === undefined
          ? MEASURED
          : reportWith({
              proposed: true,
              would_delete: { total: 0, total_bytes: 0, recordings: [] },
            }),
      applySettings: () => settingsWith(MAXIMUM_USAGE, { value: '900000000000', overridden: true }),
    });
    await openStorage(user);
    await panel();

    const before = measurements(runtime).filter((args) => args['limits'] === undefined).length;

    await user.clear(screen.getByLabelText('Maximum usage'));
    await user.type(screen.getByLabelText('Maximum usage'), '900000000000');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(
        measurements(runtime).filter((args) => args['limits'] === undefined).length,
      ).toBeGreaterThan(before);
    });
  });

  it('reads a limit back in the unit a person reads, without changing what would be sent', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      recorderStorage: (args) =>
        args['limits'] === undefined
          ? MEASURED
          : reportWith({
              proposed: true,
              would_delete: { total: 0, total_bytes: 0, recordings: [] },
            }),
    });
    await openStorage(user);
    await panel();

    await user.clear(screen.getByLabelText('Maximum usage'));
    await user.type(screen.getByLabelText('Maximum usage'), '250000000000');

    const field = screen.getByLabelText('Maximum usage');
    const hint = document.getElementById(field.getAttribute('aria-describedby') ?? '');
    expect(hint).toHaveTextContent('That is 250 GB.');

    // The gloss is a reading, not a value: what travels is still the bytes the
    // settings file spells the setting in.
    await user.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ [MAXIMUM_USAGE]: '250000000000' }]);
    });
  });

  it('says nothing is deleted automatically when no limit is set', async () => {
    const user = userEvent.setup();
    renderScreen({ recorderStorage: () => reportWith({ limits: {} }) });
    await openStorage(user);

    const measured = await panel();
    await waitFor(() => {
      expect(measured).toHaveTextContent(/No limit is set, so Clipped deletes nothing on its own/);
    });
  });

  it('says the protection rules hold nothing rather than drawing an empty table', async () => {
    const user = userEvent.setup();
    renderScreen({ recorderStorage: () => reportWith({ protected: [] }) });
    await openStorage(user);

    const measured = await panel();
    await waitFor(() => {
      expect(measured).toHaveTextContent(/Nothing is protected from automatic cleanup yet/);
    });
    expect(
      within(measured).queryByRole('table', { name: 'Never deleted automatically' }),
    ).not.toBeInTheDocument();
  });
});

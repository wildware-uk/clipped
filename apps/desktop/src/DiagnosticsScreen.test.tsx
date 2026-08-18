import type { Diagnostics, EncoderAccount } from '@clipped/shared';
import { PROTOCOL_VERSION } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import {
  buildDiagnosticsReport,
  describeCaptureHealth,
  describeConcerns,
  diagnostics,
  type DiagnosticsReportInput,
} from './diagnostics';
import type { LibraryRead } from './library';
import { DiagnosticsScreen } from './DiagnosticsScreen';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';
import type { RecorderLinkView } from './useRecorderLink';

/**
 * The Diagnostics screen's contract, as tests (issue #101).
 *
 * Four properties, and they are the four that would rot in silence.
 *
 * **The health summary is the recorder's state, not a sentence somebody typed.**
 * A screen whose wording is a constant looks identical to one that is following
 * the link, and stays identical after the link has been disconnected from it,
 * which is why the case below drives the whole application and moves the link
 * underneath it.
 *
 * **No rendering claims more than the link can establish.** A dropped connection
 * means this window lost sight of a recorder; it does not mean the recorder
 * stopped, because the recorder is a separate process that goes on recording with
 * no window at all (ADR 0002). Only the recorder's own `idle` may say nothing is
 * being recorded.
 *
 * **The report leaks nothing.** This is the property with the highest cost of
 * failure: a report is pasted into a public bug tracker, which is further than a
 * log file on somebody's disk ever travels. The case below builds the worst state
 * the window can be in — five separate places a Windows path arrives — and
 * asserts that no account name, drive or folder survives.
 *
 * **What is copied is what was shown.** `docs/privacy.md` asks that nothing about
 * what leaves the machine is hidden, and a preview that is not the payload is
 * exactly a hidden difference.
 */

/** Mounts the application the way `main.tsx` does, StrictMode and all. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** Opens the Diagnostics screen from the sidebar. */
async function openDiagnostics(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(screen.getByRole('link', { name: 'Diagnostics' }));
}

/** The live region the health state is drawn in. */
function healthPanel(): HTMLElement {
  return screen.getByRole('region', { name: 'Capture health' });
}

/**
 * What a recorder that has been asked and answered says, for a case to vary.
 *
 * A machine with a working NVENC and an AMD encoder it cannot use, which is the
 * ordinary gaming laptop and the arrangement with the most to get wrong.
 */
const ENCODERS: EncoderAccount = {
  probed: false,
  detected_at: '2026-08-11T20:14:00+01:00',
  elapsed_ms: 3,
  adapters: [
    {
      description: 'NVIDIA GeForce RTX 4090',
      vendor: 'nvidia',
      kind: 'own_video_memory',
      video_memory_bytes: 25_769_803_776,
      driver_version: '32.0.15.6094',
      captures: true,
    },
  ],
  encoders: [
    {
      encoder: 'nvenc',
      label: 'NVIDIA NVENC',
      available: true,
      implemented: true,
      adapter: 'NVIDIA GeForce RTX 4090',
      asked: false,
      codecs: [
        {
          codec: 'h264',
          supported: true,
          max_width: 4096,
          max_height: 4096,
          max_framerate_1080p: 522,
          inferred: true,
        },
      ],
    },
    {
      encoder: 'amf',
      label: 'AMD AMF',
      available: false,
      unavailable: 'no adapter from this vendor is present',
      implemented: true,
      asked: false,
      codecs: [],
    },
  ],
};

function answered(overrides: Partial<Diagnostics> = {}): LibraryRead<Diagnostics> {
  return { state: 'read', value: { encoders: ENCODERS, ...overrides } };
}

/** A recorder that could not be asked at all. */
const UNREACHABLE: LibraryRead<Diagnostics> = {
  state: 'unread',
  problem: { code: 'recorder_unreachable', message: 'the recorder is not running.' },
};

/** A view with nothing wrong and nothing known, for a case to vary one field. */
function view(overrides: Partial<RecorderLinkView> = {}): RecorderLinkView {
  return {
    link: null,
    observedAt: null,
    interrupted: null,
    failed: null,
    ended: null,
    ...overrides,
  };
}

/** A recorder attached and recording the file named. */
function recording(output: string): RecorderLinkView {
  return view({
    link: {
      link: 'attached',
      recorder_process_id: 4242,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-7',
        output,
        target: 'process cs2.exe',
        elapsed_ms: 42_000,
      },
    },
    observedAt: new Date('2026-08-12T09:13:58.000Z'),
  });
}

/*
 * Every state the link has, and what each means for capture health. Listed rather
 * than generated: the failure this guards against is two states collapsing into
 * one wording, and a table built from the states themselves could not see that.
 */
const STATES: readonly (readonly [string, RecorderLinkView, string, RegExp])[] = [
  ['outside the Clipped window', view(), 'Not known', /not the Clipped window.*no recorder to ask/],
  [
    'while the link is being made',
    view({ link: { link: 'connecting' } }),
    'Not known yet',
    /Looking for/,
  ],
  [
    'while the link is being remade',
    view({
      link: {
        link: 'reconnecting',
        attempt: 2,
        attempts_allowed: 4,
        delay_ms: 500,
        reason: 'The pipe closed.',
      },
    }),
    'Not known',
    /The pipe closed\. Attempt 2 of 4\./,
  ],
  [
    'when the link has given up',
    view({
      link: { link: 'unavailable', reason: 'clipped-recorder.exe is not beside this application.' },
    }),
    'No recorder',
    /not beside this application/,
  ],
  [
    'when a recorder is attached and idle',
    view({
      link: { link: 'attached', recorder_process_id: 7, features: [], status: { state: 'idle' } },
    }),
    'Ready',
    /nothing is being recorded/i,
  ],
  [
    'when a recorder is recording',
    recording('D:\\clips\\match.mkv'),
    'Recording',
    /process cs2\.exe/,
  ],
];

describe('what the Diagnostics screen says about capture health', () => {
  it.each(STATES)('is described %s', (_case, given, state, detail) => {
    const described = describeCaptureHealth(given);

    expect(described.state).toBe(state);
    expect(described.detail).toMatch(detail);
  });

  /*
   * The claim this screen must never make. A link that dropped tells you the
   * window lost sight of a recorder; the recorder is a separate process
   * (ADR 0002) and a recorder that is still up goes on recording either way.
   * Only `attached` and `idle` has the recorder itself as its source, so only it
   * may say so — and it does, which is what the second assertion pins.
   */
  it('never says nothing is being recorded unless the recorder said so', () => {
    for (const [when, given] of STATES) {
      const described = describeCaptureHealth(given);
      const said = `${described.state} ${described.detail} ${described.action ?? ''}`;
      const fromTheRecorder = given.link?.link === 'attached' && given.link.status.state === 'idle';

      if (!fromTheRecorder) {
        expect(said, when).not.toMatch(/nothing is being recorded/i);
      }
    }

    expect(
      describeCaptureHealth(
        view({
          link: {
            link: 'attached',
            recorder_process_id: 7,
            features: [],
            status: { state: 'idle' },
          },
        }),
      ).detail,
    ).toMatch(/nothing is being recorded/i);
  });

  /*
   * A failure with nothing to do about it is the message AGENTS.md section 45
   * exists to prevent, and "no recorder" is the state somebody most needs one in.
   */
  it('offers something to do when the link has given up, and nothing when it has not', () => {
    const given = describeCaptureHealth(
      view({ link: { link: 'unavailable', reason: 'the recorder went' } }),
    );

    expect(given.action).toMatch(/Restarting Clipped/);
    expect(given.action).toMatch(/logs/);

    expect(
      describeCaptureHealth(recording('D:\\clips\\match.mkv')).action,
      'a working recorder needs no instructions',
    ).toBeUndefined();
  });
});

describe('what the Diagnostics screen says has gone wrong', () => {
  /*
   * The healthy case, which acceptance criterion 3 names explicitly. A screen
   * that says nothing where a failure would go is indistinguishable from one that
   * is not watching, so the absence is stated rather than left blank.
   */
  it('says so when nothing has failed', () => {
    expect(describeConcerns(recording('D:\\clips\\match.mkv'))).toEqual([]);
  });

  it('names the file a failed recording wrote, which is the only actionable part', () => {
    const [said] = describeConcerns(
      view({
        failed: {
          recording_id: 'r-7',
          error: { code: 'recording_failed', message: 'the encoder stopped accepting frames' },
          output: 'D:\\clips\\match.mkv',
          seenAt: new Date('2026-08-12T09:13:12.000Z'),
        },
      }),
    );

    expect(said).toContain('the encoder stopped accepting frames');
    expect(said).toContain('recording_failed');
    expect(said).toContain('D:\\clips\\match.mkv');
  });

  it('says it cannot name the file rather than trailing off', () => {
    const [said] = describeConcerns(
      view({
        failed: {
          recording_id: 'r-9',
          error: { code: 'internal', message: 'the muxer could not write a frame' },
          output: null,
          seenAt: new Date('2026-08-12T09:13:12.000Z'),
        },
      }),
    );

    expect(said).toMatch(/did not see which file/);
  });

  it('reports an interrupted recording as interrupted and not resumed', () => {
    const [said] = describeConcerns(
      view({
        interrupted: {
          recording_id: 'r-6',
          output: 'D:\\clips\\earlier.mkv',
          target: 'process cs2.exe',
          elapsed_ms: 90_000,
        },
      }),
    );

    expect(said).toMatch(/not resumed/);
    expect(said).toContain('D:\\clips\\earlier.mkv');
  });
});

/**
 * The worst state this window can be in, for the report's privacy case.
 *
 * Five separate places a Windows path arrives, because a check that covered only
 * the recording's own `output` would pass while the sentence beside it named the
 * account: the recording being written, the file a failure left, the message of
 * that failure, the file an earlier interruption left, and the startup notice.
 */
const LEAKY: DiagnosticsReportInput = {
  /*
   * A sixth. The recorder's refusals carry its own sentences, and those name
   * files: `get_diagnostics` refused because the recorder went missing reads
   * "the recorder was not found at C:\Users\alice\...". A report that scrubbed
   * the five above and passed this one through would leak the same account name
   * by a newer route (issue #302).
   */
  recorder: {
    state: 'unread',
    problem: {
      code: 'recorder_unreachable',
      message: String.raw`the recorder was not found at C:\Users\alice\AppData\Local\Clipped\clipped-recorder.exe`,
    },
  },
  view: {
    link: {
      link: 'attached',
      recorder_process_id: 4242,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-7',
        output: String.raw`C:\Users\alice\Videos\Clipped\match.mkv`,
        target: 'process cs2.exe',
        elapsed_ms: 42_000,
      },
    },
    observedAt: new Date('2026-08-12T09:13:58.000Z'),
    interrupted: {
      recording_id: 'r-6',
      output: String.raw`C:\Users\alice\Videos\Clipped\earlier.mkv`,
      target: 'process cs2.exe',
      elapsed_ms: 90_000,
    },
    failed: {
      recording_id: 'r-7',
      error: {
        code: 'recording_failed',
        message: String.raw`the muxer could not write to C:\Users\alice\Videos\Clipped\match.mkv`,
      },
      output: String.raw`C:\Users\alice\Videos\Clipped\match.mkv`,
      seenAt: new Date('2026-08-12T09:13:12.000Z'),
    },
    // No sitting has ended in this fixture. The support report says nothing
    // about one, so there is nothing here for the redaction below to catch.
    ended: null,
  },
  notice: String.raw`the recorder was not found at C:\Users\alice\AppData\Local\Clipped\clipped-recorder.exe`,
  userAgent: 'Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 Edg/141.0.0.0',
  takenAt: new Date('2026-08-12T09:14:02.311Z'),
};

describe('the support report', () => {
  /*
   * The property with the highest cost of failure. `docs/logging.md` keeps
   * directory components out of the logs because a Windows user path starts with
   * the account name; a report goes further than a log ever does, so it has to
   * keep the same rule or undo it.
   *
   * Each leaked string is asserted separately so a failure names which one got
   * through.
   */
  it.each([
    ['the account name', 'alice'],
    ['the user directory', 'Users'],
    ['the drive', 'C:\\'],
    ['a folder Clipped chose', 'Videos'],
    ['a folder Windows chose', 'AppData'],
  ])('does not carry %s out of this machine', (_what, leaked) => {
    expect(buildDiagnosticsReport(LEAKY)).not.toContain(leaked);
  });

  /*
   * Redacting is not the same as dropping. A report that lost the recording's
   * identity would be private and useless; the digest is what lets a reader line
   * a report up with the log lines about the same file, and the two recordings
   * here have different names so both must survive.
   */
  it('keeps the file names and the digests that identify them', () => {
    const report = buildDiagnosticsReport(LEAKY);

    expect(report).toMatch(/match\.mkv#[0-9a-f]{16}/);
    expect(report).toMatch(/earlier\.mkv#[0-9a-f]{16}/);
    expect(report).toMatch(/clipped-recorder\.exe#[0-9a-f]{16}/);
  });

  /*
   * The whole reason a failed recording's report is worth having. Dropping any of
   * these leaves a reader with "a recording failed" and nothing to work from
   * (acceptance criterion 2).
   */
  it.each([
    ['the recorder it was talking to', /Recorder process\s+4242/],
    ['what the recorder was doing', /Recorder state\s+recording/],
    ['what was being recorded', /Recording target\s+process cs2\.exe/],
    ['how long it had run, and when that was true', /Elapsed when observed\s+42 s/],
    ['when the window last heard from the recorder', /Status observed\s+2026-08-12T09:13:58/],
    ['the failure code, which is stable across versions', /code\s+recording_failed/],
    ['the failure message, which is not', /message\s+the muxer could not write to/],
    [
      'the protocol version the two ends agreed on',
      new RegExp(String.raw`Protocol version\s+` + String(PROTOCOL_VERSION)),
    ],
    ['which build of the interface wrote it', /Interface\s+@clipped\/desktop \d/],
    ['the webview and the Windows build under it', /Webview\s+Mozilla\/5\.0 \(Windows NT/],
    ['the notice nothing else remembers', /Notice\s+the recorder was not found/],
  ])('carries %s', (_what, expected) => {
    expect(buildDiagnosticsReport(LEAKY)).toMatch(expected);
  });

  /*
   * A reader has to be able to tell "dropped no frames" from "counted no frames"
   * (AGENTS.md section 27). Listed here independently of `diagnostics()` rather
   * than mapped from it: a case that walked the function's own output would go on
   * passing after the list had been emptied to one entry.
   */
  const NOT_REPORTED: readonly string[] = [
    'Game detection',
    'Capture backend',
    'Resolution changes',
    'Encoder',
    'Dropped frames',
    'Encoder latency',
    'Audio drift',
    'Audio devices',
    'Muxer status',
    'Disk latency',
    'Plugin events',
    'Log files',
  ];

  it.each(NOT_REPORTED)('says that %s is not reported by this build', (subject) => {
    const line = buildDiagnosticsReport(LEAKY)
      .split('\n')
      .find((candidate) => candidate.startsWith('Not reported by this build:'));

    expect(line).toContain(subject);
  });

  it('names where the log files are, since they are not in it', () => {
    const report = buildDiagnosticsReport(LEAKY);

    expect(report).toContain(String.raw`%LOCALAPPDATA%\Clipped\logs`);
    expect(report).toMatch(/Log files are not in this report/);
  });

  /*
   * The structural half of "contains nothing it should not". Every line of the
   * report is either prose written here or one of these labels, so a field added
   * later — a window title, a device name, a path somebody forgot to redact — has
   * to be added to this list before the suite goes green. A check that only
   * looked for known leaks could not see a new kind of one.
   */
  it('carries these fields and no others', () => {
    const labels = buildDiagnosticsReport(LEAKY)
      .split('\n')
      .map((line) => line.trim())
      .filter((line) => /^\S.*\s{2,}\S/.test(line))
      .map((line) => line.replace(/\s{2,}.*$/, ''));

    expect(labels).toEqual([
      'Taken',
      'Interface',
      'Protocol version',
      'Webview',
      'Status observed',
      'Recorder link',
      'Recorder process',
      'Recorder state',
      'Recording id',
      'Recording target',
      'Recording file',
      'Elapsed when observed',
      'Capture health',
      'Capture backend',
      'Encoders',
      'Recording failed',
      'seen',
      'code',
      'message',
      'file',
      'Recording interrupted',
      'target',
      'file',
      'elapsed',
      'Notice',
    ]);
  });

  /*
   * The healthy case again, in the report rather than on the panel: a reader has
   * to be able to tell a run where nothing failed from a window that was not
   * watching for failures.
   */
  it('states that nothing failed rather than leaving the fields out', () => {
    const report = buildDiagnosticsReport({
      view: recording('D:\\clips\\match.mkv'),
      notice: undefined,
      userAgent: 'jsdom',
      takenAt: new Date('2026-08-12T09:14:02.311Z'),
      recorder: answered(),
    });

    expect(report).toMatch(/Recording failed\s+none since this window opened/);
    expect(report).toMatch(/Recording interrupted\s+none since this window opened/);
    expect(report).toMatch(/Notice\s+none/);
  });
});

describe('the Diagnostics screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    Reflect.deleteProperty(navigator, 'clipboard');
    window.location.hash = '';
  });

  it('follows the recorder link rather than showing a sentence that was typed once', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();
    await openDiagnostics(user);

    // Anchored, because "Not known" is a substring of "Not known yet" and an
    // unanchored match would let the screen sit on one wording for both.
    await waitFor(() => {
      expect(within(healthPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Not known yet$/,
      );
    });

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-7',
        output: 'D:\\clips\\match.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 42_000,
      },
    });

    await waitFor(() => {
      expect(within(healthPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Recording$/,
      );
    });

    runtime.emit({ event: 'state', link: 'unavailable', reason: 'the recorder exited.' });

    await waitFor(() => {
      expect(within(healthPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^No recorder$/,
      );
    });
    expect(within(healthPanel()).getByText(/the recorder exited\./)).toBeVisible();
  });

  /*
   * A recording that failed is the reason somebody opens this screen, and the
   * state that follows the failure is "idle" — which says nothing about it. This
   * drives the whole application so that the failure has to survive the hook, the
   * shell and the screen, which is where it would be dropped.
   */
  it('keeps a failed recording on screen after the recorder reports itself idle', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();
    await openDiagnostics(user);

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-7',
        output: 'D:\\clips\\match.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 42_000,
      },
    });
    runtime.emit({
      event: 'recording_failed',
      recording_id: 'r-7',
      error: { code: 'recording_failed', message: 'the encoder stopped accepting frames' },
    });
    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 91,
      features: [],
      status: { state: 'idle' },
    });

    await waitFor(() => {
      expect(within(healthPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(/^Ready$/);
    });
    expect(within(healthPanel()).getByText(/the encoder stopped accepting frames/)).toBeVisible();
    expect(within(healthPanel()).getByText(/D:\\clips\\match\.mkv/)).toBeVisible();
  });

  it('announces a change of state rather than only drawing it', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();
    await openDiagnostics(user);

    expect(healthPanel()).toHaveAttribute('aria-live', 'polite');
  });

  /*
   * Issue #101's third acceptance criterion names the healthy case, and it is
   * the one a screen is most likely to get wrong: a panel that draws nothing
   * where a failure would go looks exactly like a panel that is not watching for
   * failures, and looks that way for ever.
   *
   * The scope sentence is asserted beside it because without it "nothing has
   * failed" reads as a statement about the machine. It is a statement about one
   * recorder, since this window opened, and neither half of that is optional.
   */
  it('says on screen that nothing has failed, and what that statement covers', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-7',
        output: 'D:\\clips\\match.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 42_000,
      },
    });
    renderApp();
    await openDiagnostics(user);

    await waitFor(() => {
      expect(within(healthPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Recording$/,
      );
    });

    expect(
      within(healthPanel()).getByText(/No recording has failed or been interrupted/),
    ).toBeVisible();
    const scope = within(healthPanel()).getByText(/recorder this window is attached to/);
    expect(scope).toHaveTextContent(/since this window opened/);
    expect(scope).toHaveTextContent(/issue #100/);
  });

  /*
   * SPEC.md section 36's own list, written out here rather than mapped from
   * `diagnostics()`. That distinction is the whole test: a case that walked the
   * rendered rows and asserted "two cells, and the second names an issue" is
   * satisfied by a table holding one invented row. The claim being made is that
   * *these* are the twelve the specification asks for, each pinned to the work
   * that supplies it.
   *
   * Four of them carry no issue any more, and that is the change issue #302
   * made: Recording paths always came from the status, Game detection came with
   * protocol 2's sitting, and the Capture backend and the Encoder come from
   * `get_diagnostics`. An empty list here means "this row is supplied", so a row
   * that quietly went back to naming an issue would fail the entry below rather
   * than pass unnoticed.
   */
  const MUST_BE_NAMED: readonly (readonly [string, readonly number[]])[] = [
    ['Game detection', []],
    ['Capture backend', []],
    ['Resolution changes', [98]],
    ['Encoder', []],
    ['Dropped frames', [100]],
    ['Encoder latency', [100]],
    ['Audio drift', [100]],
    ['Audio devices', [100, 180]],
    ['Recording paths', []],
    ['Muxer status', [100]],
    ['Disk latency', [100]],
    ['Plugin events', [69]],
    ['Log files', [303]],
  ];

  it('names every diagnostic the specification asks for, and the issue that lands it', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(
      {
        link: 'attached',
        recorder_process_id: 7,
        features: ['diagnostics'],
        status: { state: 'idle' },
      },
      null,
      { recorderDiagnostics: () => ({ encoders: ENCODERS }) },
    );
    renderApp();
    await openDiagnostics(user);

    await waitFor(() => {
      expect(screen.getAllByRole('table').length).toBeGreaterThan(1);
    });
    const rows = within(screen.getAllByRole('table')[0] as HTMLElement)
      .getAllByRole('row')
      .slice(1);
    expect(rows).toHaveLength(MUST_BE_NAMED.length);

    for (const [subject, issues] of MUST_BE_NAMED) {
      const matching = rows.filter(
        (row) => within(row).getAllByRole('cell')[0]?.textContent === subject,
      );
      expect(matching, `one row for ${subject}`).toHaveLength(1);

      const reported =
        within(matching[0] as HTMLElement).getAllByRole('cell')[1]?.textContent ?? '';
      for (const issue of issues) {
        expect(reported, `${subject} is waiting on #${String(issue)}`).toMatch(
          new RegExp(`#${String(issue)}\\b`),
        );
      }
    }
  });

  /*
   * The one diagnostic on that list this window can report. It must be the live
   * path rather than a promise about one, and it must not be a convincing blank
   * when nothing is being recorded.
   */
  it('reports the recording path it has, and says there is none when there is not', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openDiagnostics(user);

    const pathRow = (): string =>
      within(screen.getByRole('table'))
        .getAllByRole('row')
        .filter((row) => within(row).queryAllByRole('cell')[0]?.textContent === 'Recording paths')
        .map((row) => within(row).queryAllByRole('cell')[1]?.textContent ?? '')[0] ?? '';

    await waitFor(() => {
      expect(pathRow()).toMatch(/Nothing is being recorded/);
    });

    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: {
        state: 'recording',
        recording_id: 'r-7',
        output: 'D:\\clips\\match.mkv',
        target: 'process cs2.exe',
        elapsed_ms: 42_000,
      },
    });

    await waitFor(() => {
      expect(pathRow()).toBe('D:\\clips\\match.mkv');
    });
  });

  /*
   * `docs/privacy.md`: nothing surprising, nothing hidden. A preview that is not
   * the payload is precisely a hidden difference, and it is the sort that is
   * introduced by an innocuous refactor — composing the report twice, or copying
   * a richer version "since it is going to a developer anyway".
   */
  it('copies exactly the report it showed, and says that it did', async () => {
    const user = userEvent.setup();
    const written: string[] = [];
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: (text: string): Promise<void> => {
          written.push(text);
          return Promise.resolve();
        },
      },
    });
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openDiagnostics(user);

    const shown = screen.getByText(/^Clipped diagnostics report/).textContent;
    await user.click(screen.getByRole('button', { name: 'Copy report' }));

    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(/copied to the clipboard/);
    });
    expect(written).toEqual([shown]);
  });

  /*
   * A control that silently does nothing is what AGENTS.md section 27 forbids,
   * and the clipboard is the one thing on this screen that can be absent: it is
   * a web API, so it needs no Tauri permission, and it needs a secure context,
   * which is not something this code can check for the user. Both failures are
   * covered — no clipboard at all, and a clipboard that refuses — because they
   * arrive by different paths and only one of them is a rejected promise.
   *
   * Each case says the same two things: that nothing was copied, and that the
   * report is on screen and selectable, which is the way out (AGENTS.md section
   * 45).
   */
  it('says so when there is no clipboard, rather than appearing to work', async () => {
    const user = userEvent.setup();
    // Explicitly, rather than relying on jsdom having none: `userEvent.setup()`
    // installs a stub of its own, so the environment is not the no-clipboard
    // case and a test that assumed it was would be testing the stub.
    Object.defineProperty(navigator, 'clipboard', { configurable: true, value: undefined });
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openDiagnostics(user);

    await user.click(screen.getByRole('button', { name: 'Copy report' }));

    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(/nothing was copied/);
    });
    expect(screen.getByRole('status')).toHaveTextContent(/selectable/);
  });

  it('says so when the clipboard refuses, and quotes what it said', async () => {
    const user = userEvent.setup();
    Object.defineProperty(navigator, 'clipboard', {
      configurable: true,
      value: {
        writeText: (): Promise<void> =>
          Promise.reject(new Error('NotAllowedError: write permission denied')),
      },
    });
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openDiagnostics(user);

    await user.click(screen.getByRole('button', { name: 'Copy report' }));

    await waitFor(() => {
      expect(screen.getByRole('status')).toHaveTextContent(/write permission denied/);
    });
    expect(screen.getByRole('status')).toHaveTextContent(/selectable/);
  });

  /*
   * There is exactly one control here, and it is the one that does something. An
   * Export Support Bundle button is drawn by the deck and would have nothing
   * behind it: the log files it would have to contain are unreachable from this
   * window (issue #303), and a button that wrote a report with no logs in it
   * would be an export in name only.
   */
  it('offers the one control that works, and no button that would not', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openDiagnostics(user);

    const main = screen.getByRole('main');
    expect(
      within(main)
        .getAllByRole('button')
        .map((button) => button.textContent),
    ).toEqual(['Copy report']);
    expect(within(main).queryAllByRole('link')).toHaveLength(0);
    // And the reason the missing one is missing is on the screen, not implied.
    expect(
      within(main).getByText(/Writing one archive with both in it is issue #303/),
    ).toBeVisible();
  });

  it('is reached with the keyboard, and takes focus when it opens', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    await user.click(screen.getByRole('link', { name: 'Diagnostics' }));

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Diagnostics');
    expect(screen.getByRole('main')).toHaveFocus();

    // And the one control is reachable from there with Tab alone.
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Copy report');
  });

  it('has a heading for each of its three parts', () => {
    render(<DiagnosticsScreen view={view()} notice={undefined} />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Diagnostics');
    expect(
      screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
    ).toEqual(expect.arrayContaining(['What this build reports', 'Support report']));
  });

  /*
   * Issue #302's third acceptance criterion, through the whole application: the
   * capture backend and the encoder arrive from the recorder over
   * `recorder_diagnostics` and are drawn, in place of two rows naming the issue.
   *
   * Driven through `App` and the real Tauri wrapper rather than by handing the
   * component a value, because what is being asserted is that the screen *asks*.
   * A case that passed the diagnostics in as a prop would go on passing after
   * the command had been disconnected from the screen.
   */
  it('shows the capture backend and the encoder the recorder reports', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(
      {
        link: 'attached',
        recorder_process_id: 7,
        features: ['diagnostics'],
        status: {
          state: 'recording',
          recording_id: 'r-1',
          output: 'D:\\clips\\match.mkv',
          target: 'process cs2.exe',
          elapsed_ms: 4_200,
        },
      },
      null,
      {
        recorderDiagnostics: () => ({
          capture: {
            setting: 'Automatic',
            started_with: 'Windows Graphics Capture',
            current: 'Desktop Duplication',
            changes: [
              {
                from: 'Windows Graphics Capture',
                to: 'Desktop Duplication',
                restart: false,
                trigger: 'capture_failed',
                reason: 'the compositor stopped delivering frames',
              },
            ],
          },
          encoders: ENCODERS,
        }),
      },
    );
    renderApp();
    await openDiagnostics(user);

    await waitFor(() => {
      expect(screen.getByText(/Desktop Duplication, chosen automatically/)).toBeVisible();
    });
    // The reason, in the recorder's own words. A row that named the backend and
    // dropped why it changed would be the fact with no explanation attached that
    // `CaptureStatus` exists to prevent.
    // Asserted with `getAllByText` because it is deliberately in two places: the
    // row somebody reads, and the report they paste into a bug tracker.
    expect(screen.getAllByText(/the compositor stopped delivering frames/).length).toBeGreaterThan(
      0,
    );
    expect(screen.getByText(/The recording started with Windows Graphics Capture/)).toBeVisible();
  });

  /*
   * The other half, and the one worth more than the first: a recorder that could
   * not be asked must not be drawn as a machine with nothing to report. "Clipped
   * found no encoder here" and "Clipped never asked" are opposite readings of the
   * same blank row (AGENTS.md section 27).
   */
  it('says the recorder could not be asked, rather than that it has no encoder', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openDiagnostics(user);

    await waitFor(() => {
      expect(screen.getAllByText(/could not ask the recorder/i).length).toBeGreaterThan(0);
    });
    expect(screen.queryByText(/no encoder on this machine/i)).toBeNull();
  });

  /*
   * The screen's table is the one place the twelve are enumerated for a reader,
   * so its headings say what it is: not a list of measurements.
   */
  it('heads its table with what it is, rather than with figures it has not got', () => {
    render(<DiagnosticsScreen view={view()} notice={undefined} />);

    expect(
      within(screen.getByRole('table'))
        .getAllByRole('columnheader')
        .map((header) => header.textContent),
    ).toEqual(['Diagnostic', 'What this build reports']);
    expect(diagnostics(view(), UNREACHABLE).filter((entry) => entry.known)).toHaveLength(0);
  });
});

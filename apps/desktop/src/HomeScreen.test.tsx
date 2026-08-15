import type { RecorderStatus } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { HomeScreen } from './HomeScreen';
import type { RecorderProblem, RecordTarget } from './recording';
import { describeRecordingNow } from './recordingNow';
import { A_COUNT, textRuns } from './test/counts';
import { stubRecorderLinkRuntime, type Invocation } from './test/recorderLinkRuntime';
import type { RecorderLinkState } from './useRecorderLink';
import { STATUS_INTERVAL_MS } from './useRecording';

/**
 * The Home screen's contract, as tests (issues #60, #301 and #389).
 *
 * The properties worth guarding, in the order they would hurt:
 *
 * - **the recording state is asked for, not assumed.** Not from the button
 *   having been pressed, not from a clock this window keeps, and not from the
 *   status that arrived with the link — which is measured when a recording
 *   starts and never again (`apps/recorder/src/serve.rs`). Several cases below
 *   drive the recorder's answer and the link apart on purpose, because a window
 *   that is confidently wrong about somebody's footage is worse than one that
 *   says it does not know (issue #389).
 * - **the command that goes out is the one the button offered**, carrying what
 *   the screen named. The middle hop — the Tauri command itself — is covered in
 *   `src-tauri/src/main.rs`, because `invoke` is stubbed here and nothing on
 *   this side can see what reached the recorder.
 * - **a refusal is reported in the recorder's own words**, rather than as a
 *   silent no-op or a spinner that never resolves (AGENTS.md section 45).
 * - **no figure is invented.** No count, no total, no "0 sessions" over a
 *   library that was never read. A zeroed tile is indistinguishable from a
 *   machine that has recorded nothing (AGENTS.md section 27).
 * - **no wording claims more than the link can establish.** The link sees the
 *   one recorder this window started or attached to; a `clipped-recorder watch`
 *   in a terminal is invisible to it. So "Nothing is being recorded" is a claim
 *   about the machine that nothing here measured, and the heading has to name
 *   what it speaks for.
 */

/** Mounts the application the way `main.tsx` does, StrictMode and all. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** The live region the recording state is drawn in. */
function recordingPanel(): HTMLElement {
  return screen.getByRole('region', { name: 'Recording now' });
}

/** The record button, whatever it currently offers to do. */
function recordButton(): HTMLElement {
  return within(recordingPanel()).getByRole('button');
}

/** The file a recording in these cases writes. */
const OUTPUT = 'D:\\clips\\clipped-cs2-2026-08-11T21-04-19.mkv';

/** What the window would record: the application the user was last in. */
const TARGET: RecordTarget = { process_id: 4_242, process_name: 'cs2.exe' };

/**
 * A recorder attached, whose *link* says it is idle.
 *
 * Used as the link in nearly every case below, including the ones where a
 * recording is running. That is deliberate and is the point: the status inside
 * the link is measured once, when a recording starts, and the screen must not be
 * reading it. Driving the two apart is what makes "the state comes from
 * `get_status`" a thing a test can fail on.
 */
const ATTACHED: RecorderLinkState = {
  link: 'attached',
  recorder_process_id: 7,
  features: [],
  status: { state: 'idle' },
};

/** A recorder attached whose link claims a recording, for the reverse case. */
const LINK_CLAIMS_RECORDING: RecorderLinkState = {
  link: 'attached',
  recorder_process_id: 7,
  features: [],
  status: {
    state: 'recording',
    recording_id: 'rec-1',
    output: OUTPUT,
    target: 'process cs2.exe',
    elapsed_ms: 754_000,
  },
};

/** What `get_status` answers while a recording has been running this long. */
function recordingFor(elapsedMs: number): RecorderStatus {
  return {
    state: 'recording',
    recording_id: 'rec-1',
    output: OUTPUT,
    target: 'process cs2.exe',
    elapsed_ms: elapsedMs,
  };
}

/** What `get_status` answers when nothing is being recorded. */
const IDLE: RecorderStatus = { state: 'idle' };

/** What `stop_recording` answers: a recording the recorder has finished. */
const FINISHED = {
  output: OUTPUT,
  duration_ms: 9_000,
  end_reason: 'stopped',
  frames_encoded: 540,
  frames_skipped_for_rate: 0,
  frames_dropped_writer_behind: 0,
  encoder: 'nvenc',
  codec: 'av1',
  width: 2_560,
  height: 1_392,
};

/** Every `recorder_status` ask a run made, so a case can wait for the next one. */
function statusAsks(invocations: readonly Invocation[]): readonly Invocation[] {
  return invocations.filter((invocation) => invocation.command === 'recorder_status');
}

/*
 * Every combination of link and answer, and what each one means for "what is
 * being recorded now". They are listed rather than generated: the failure this
 * guards against is two of them collapsing into one wording, and a table built
 * from the states themselves could not see that happen.
 */
const STATES: readonly (readonly [
  string,
  RecorderLinkState | null,
  RecorderStatus | null,
  RecorderProblem | null,
  string,
  RegExp,
])[] = [
  [
    'outside the Clipped window',
    null,
    null,
    null,
    'Not known',
    /not the Clipped window.*no recorder to ask/,
  ],
  [
    'while the link is being made',
    { link: 'connecting' },
    null,
    null,
    'Not known yet',
    /Looking for/,
  ],
  [
    'while the link is being remade',
    {
      link: 'reconnecting',
      attempt: 2,
      attempts_allowed: 4,
      delay_ms: 500,
      reason: 'The pipe closed.',
    },
    null,
    null,
    'Not known',
    /The pipe closed\. Attempt 2 of 4\./,
  ],
  [
    'when there is no recorder',
    { link: 'unavailable', reason: 'clipped-recorder.exe is not beside this application.' },
    null,
    null,
    'Not known',
    /not attached to a recorder.*not beside this application/,
  ],
  [
    'when a recorder is attached and has not answered yet',
    ATTACHED,
    null,
    null,
    'Not known yet',
    /Asking the recorder/,
  ],
  [
    'when the recorder was asked and did not answer',
    ATTACHED,
    null,
    { code: 'recorder_unreachable', message: 'the recorder could not be reached: the pipe closed' },
    'Not known',
    /did not get an answer.*the pipe closed/,
  ],
  [
    'when the recorder answers that it is idle',
    ATTACHED,
    IDLE,
    null,
    'This recorder is not recording',
    /running and idle.*clipped-recorder watch.*invisible here/,
  ],
  [
    'when the recorder answers that it is recording',
    ATTACHED,
    recordingFor(754_000),
    null,
    'Recording process cs2.exe',
    /being written now/,
  ],
];

describe('what the Home screen says about the recording now', () => {
  it.each(STATES)('is described %s', (_case, link, status, problem, state, detail) => {
    const described = describeRecordingNow(link, status, problem);

    expect(described.state).toBe(state);
    expect(described.detail).toMatch(detail);
  });

  /*
   * The heading is the part a screen-reader user skips to and the part somebody
   * takes away, so it has to carry its own scope. Three forms are allowed and
   * no fourth: "Not known…", which claims nothing; "This recorder…", which names
   * what the claim is about; and "Recording <target>", which asserts a recording
   * that demonstrably exists and is therefore true whatever else is running.
   *
   * The wording this rules out is "Nothing is being recorded", which reads as a
   * statement about the machine. `clipped-recorder watch` serves no protocol and
   * is invisible to this link, so it could be recording a game while this window
   * said so — and the screen's own next paragraph says as much.
   *
   * Asserted over every rendering rather than over the one that would be wrong,
   * because the defect is a class: it is the same unscoped heading whichever
   * branch it gets written into.
   */
  it.each(STATES)(
    'either names what it speaks for, or claims nothing, %s',
    (_case, link, status, problem) => {
      expect(describeRecordingNow(link, status, problem).state).toMatch(
        /^(Not known|This recorder |Recording )/,
      );
    },
  );

  /*
   * The file and the duration are only ever drawn for a recording the recorder
   * has just said is running. A path left over from a previous recording, or a
   * duration beside a state that is not "recording", would both be this window
   * making something up.
   */
  it('names the file and the duration only while the recorder says one is running', () => {
    const running = describeRecordingNow(ATTACHED, recordingFor(754_000), null);
    expect(running.output).toBe(OUTPUT);
    expect(running.elapsed).toBe('12:34');

    for (const [, link, status, problem] of STATES.filter(
      ([, , candidate]) => candidate?.state !== 'recording',
    )) {
      const described = describeRecordingNow(link, status, problem);
      expect(described.output).toBeUndefined();
      expect(described.elapsed).toBeUndefined();
    }
  });

  /*
   * The one place a duration may come from. `elapsed_ms` is the recorder's own
   * measurement, and this asserts the screen's figure is a rendering of it
   * rather than of anything else — a different number in means a different
   * number out, every time. Truncated rather than rounded: 1,900 ms is one
   * completed second, and a file holding one second of video beside "0:02"
   * would be a duration nobody measured.
   */
  it('renders the duration the recorder measured, whatever it is', () => {
    expect(describeRecordingNow(ATTACHED, recordingFor(0), null).elapsed).toBe('0:00');
    expect(describeRecordingNow(ATTACHED, recordingFor(1_900), null).elapsed).toBe('0:01');
    expect(describeRecordingNow(ATTACHED, recordingFor(5_000), null).elapsed).toBe('0:05');
    expect(describeRecordingNow(ATTACHED, recordingFor(65_000), null).elapsed).toBe('1:05');
    expect(describeRecordingNow(ATTACHED, recordingFor(3_671_000), null).elapsed).toBe('1:01:11');
  });
});

describe('the Home screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  /*
   * The property the whole feature turns on, in the arrangement that catches
   * the tempting shortcut. The **link** says the recorder is idle; the
   * recorder's answer to `get_status` says it is recording. A screen wired to
   * `link.status` — which is right there, already on screen, and free — draws
   * "This recorder is not recording" over a running recording and passes every
   * other case in this file.
   */
  it('draws what the recorder answers, not the status that arrived with the link', async () => {
    stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => recordingFor(30_000),
    });
    renderApp();

    await waitFor(() => {
      expect(within(recordingPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Recording process cs2\.exe$/,
      );
    });
    expect(within(recordingPanel()).getByText(OUTPUT)).toBeVisible();
  });

  /* And the same trap in the other direction, which is the dangerous one: the
   * link still carrying the recording it last saw start, while the recorder has
   * since stopped. A screen reading the link would claim a recording that is not
   * happening. */
  it('does not claim a recording the recorder says has ended', async () => {
    stubRecorderLinkRuntime(LINK_CLAIMS_RECORDING, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => IDLE,
    });
    renderApp();

    await waitFor(() => {
      expect(within(recordingPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^This recorder is not recording$/,
      );
    });
    expect(within(recordingPanel()).queryByText(OUTPUT)).toBeNull();
  });

  /*
   * Issue #389's third acceptance criterion, at the call site.
   *
   * The recorder answers the same `elapsed_ms` every time it is asked, and the
   * case then lets several real seconds pass — waiting on the asks themselves,
   * so it is the window's own polling that measures the delay. A duration
   * counted up locally would have moved; the one the recorder measured has not,
   * so it must not have.
   *
   * Real time rather than fake, deliberately: a fake clock is exactly the thing
   * a local timer would also be driven by, and advancing it proves less. It
   * costs a few seconds once.
   */
  it('shows the duration the recorder measured and never a clock of its own', async () => {
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => recordingFor(5_000),
    });
    renderApp();

    await waitFor(() => {
      expect(within(recordingPanel()).getByText(/^Recording for /)).toHaveTextContent(
        'Recording for 0:05,',
      );
    });

    // Several rounds of asking, which is several real seconds of wall clock.
    await waitFor(() => expect(statusAsks(runtime.invocations).length).toBeGreaterThanOrEqual(4), {
      timeout: STATUS_INTERVAL_MS * 8,
    });

    // The recorder never said anything but five seconds, so five seconds is the
    // only figure that may be on screen.
    expect(within(recordingPanel()).getByText(/^Recording for /)).toHaveTextContent(
      'Recording for 0:05,',
    );
    for (const run of textRuns(recordingPanel())) {
      expect(run).not.toMatch(/\b0:(0[0-46-9]|[1-9]\d)\b/);
    }
  });

  /*
   * The other half of the same property. The recorder's answer jumps by a minute
   * between one ask and the next, and the screen has to follow it — a window
   * running its own clock would show six seconds, not sixty-five.
   */
  it('follows the recorder when its measurement moves', async () => {
    let elapsed = 5_000;
    stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => recordingFor(elapsed),
    });
    renderApp();

    await waitFor(() => {
      expect(within(recordingPanel()).getByText(/^Recording for /)).toHaveTextContent(
        'Recording for 0:05,',
      );
    });

    elapsed = 65_000;

    await waitFor(
      () => {
        expect(within(recordingPanel()).getByText(/^Recording for /)).toHaveTextContent(
          'Recording for 1:05,',
        );
      },
      { timeout: STATUS_INTERVAL_MS * 5 },
    );
  });

  /*
   * The failure issue #389 names in as many words: a window that says
   * "recording" while the recorder has died.
   *
   * The recorder answers a recording, then stops answering at all. Nothing in
   * the window may keep the old answer on screen — it is a claim about a file
   * that may no longer be being written, and the person reading it is relying on
   * it.
   */
  it('stops saying a recording is running when the recorder stops answering', async () => {
    let answering = true;
    stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => {
        if (!answering) {
          throw {
            code: 'recorder_unreachable',
            message: 'the recorder could not be reached: the pipe closed',
          };
        }
        return recordingFor(12_000);
      },
    });
    renderApp();

    await waitFor(() => {
      expect(within(recordingPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Recording process cs2\.exe$/,
      );
    });

    answering = false;

    await waitFor(
      () => {
        expect(within(recordingPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
          /^Not known$/,
        );
      },
      { timeout: STATUS_INTERVAL_MS * 5 },
    );
    expect(within(recordingPanel()).getByText(/the pipe closed/)).toBeVisible();
    expect(within(recordingPanel()).queryByText(OUTPUT)).toBeNull();
    expect(within(recordingPanel()).queryByText(/^Recording for /)).toBeNull();
  });

  /*
   * The optimism this must not have, in the arrangement where it shows.
   *
   * `start_recording` is accepted — the command went out and the recorder took
   * it — and then **the recorder says nothing more**: the second `get_status` is
   * held open, which is what a recorder busy opening an encoder session looks
   * like from here, and is also what a recorder that died in the attempt looks
   * like. In that gap the window has no answer newer than "idle", so anything it
   * says about a recording is something it made up.
   *
   * The status is held rather than answered "idle" for a reason worth keeping: a
   * version of this case that let the poll answer freely passed against an
   * implementation that *did* set its own state on success, because the next
   * answer corrected it a few milliseconds later. The bug was real and the case
   * could not see it. Holding the answer is what makes the window's own
   * invention the only thing on screen.
   */
  it('does not say it is recording because the button was pressed', async () => {
    const user = userEvent.setup();
    let asked = 0;
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => {
        asked += 1;
        // The first ask is answered, so the screen has a state at all. Every one
        // after it is held open and never resolves.
        return asked === 1 ? IDLE : new Promise<RecorderStatus>(() => undefined);
      },
      startRecording: () => ({ recording_id: 'rec-1', output: OUTPUT }),
    });
    renderApp();

    await waitFor(() => {
      expect(recordButton()).toHaveTextContent('Start recording cs2.exe');
    });

    await user.click(recordButton());

    // The command has been sent, answered, and the window has asked again —
    // which is the last thing it does after a start, so every state it was going
    // to set has been set and drawn by now.
    await waitFor(() => {
      expect(statusAsks(runtime.invocations).length).toBeGreaterThanOrEqual(2);
    });
    await waitFor(() => {
      expect(recordButton()).toBeEnabled();
    });

    expect(within(recordingPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
      /^This recorder is not recording$/,
    );
    expect(recordButton()).toHaveTextContent('Start recording cs2.exe');
    expect(within(recordingPanel()).queryByText(/^Recording for /)).toBeNull();
    expect(within(recordingPanel()).queryByText(OUTPUT)).toBeNull();
  });

  /*
   * What the button sends. `invoke` is stubbed, so this can see the command and
   * its arguments leave the window and no further; what happens to them after
   * that — which protocol command is sent, and whether the process identifier
   * survives — is `src-tauri/src/main.rs`'s own tests, because nothing on this
   * side can see it.
   */
  it('records the application the user was last in, and names it on the button', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => IDLE,
      startRecording: () => ({ recording_id: 'rec-1', output: OUTPUT }),
    });
    renderApp();

    await waitFor(() => {
      expect(recordButton()).toHaveTextContent('Start recording cs2.exe');
    });

    await user.click(recordButton());

    await waitFor(() => {
      expect(
        runtime.invocations.filter((invocation) => invocation.command === 'start_recording'),
      ).toEqual([{ command: 'start_recording', args: { processId: 4_242 } }]);
    });
  });

  /*
   * And what stop sends. The identifier is the safety property: a recording that
   * ended by itself between the screen drawing it and the button being pressed
   * must not have its successor stopped instead (`docs/ipc.md`). A stop that
   * dropped it would send "stop whatever is running", and the damage would show
   * up as somebody's *next* recording ending early.
   */
  it('stops the recording it has on screen, by name', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => recordingFor(9_000),
      stopRecording: () => FINISHED,
    });
    renderApp();

    await waitFor(() => {
      expect(recordButton()).toHaveTextContent('Stop recording');
    });

    await user.click(recordButton());

    await waitFor(() => {
      expect(
        runtime.invocations.filter((invocation) => invocation.command === 'stop_recording'),
      ).toEqual([{ command: 'stop_recording', args: { recordingId: 'rec-1' } }]);
    });
  });

  /*
   * Issue #389's fourth acceptance criterion. A recording that cannot start says
   * why, in the words the protocol returned — not a silent no-op, and not a
   * spinner that never resolves (AGENTS.md section 45).
   */
  it('says why a recording could not start, in the recorder’s own words', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => IDLE,
      startRecording: () => {
        throw {
          code: 'target_not_found',
          message: 'no visible window belongs to process 4242; it may have closed',
        };
      },
    });
    renderApp();

    await waitFor(() => {
      expect(recordButton()).toHaveTextContent('Start recording cs2.exe');
    });

    await user.click(recordButton());

    const said = await within(recordingPanel()).findByRole('alert');
    expect(said).toHaveTextContent('no visible window belongs to process 4242; it may have closed');
    // And it is still offering to record: a refusal is not a dead screen.
    expect(recordButton()).toBeEnabled();
  });

  /*
   * A control that cannot work says so where it is, rather than being drawn as
   * though pressing it would do something (AGENTS.md sections 27 and 45). The
   * reason is text beside the button rather than a `title`, because a tooltip is
   * invisible to a keyboard and to a screen reader.
   */
  it('offers no start when there is nothing to record, and says why', async () => {
    stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => null,
      recorderStatus: () => IDLE,
    });
    renderApp();

    await waitFor(() => {
      expect(
        within(recordingPanel()).getByText(/Nothing has been in front of this window/),
      ).toBeVisible();
    });
    expect(recordButton()).toBeDisabled();
  });

  /*
   * The same rule for a window with no recorder at all. The button's own reason
   * is short and does not repeat the link's, which the panel has already given
   * in full immediately above it — the whole reason is on the screen once.
   */
  it('offers no start when there is no recorder, and says why', async () => {
    stubRecorderLinkRuntime(
      { link: 'unavailable', reason: 'clipped-recorder.exe is not beside this application.' },
      null,
      { recordTarget: () => TARGET },
    );
    renderApp();

    await waitFor(() => {
      expect(
        within(recordingPanel()).getByText('There is no recorder to record with.'),
      ).toBeVisible();
    });
    expect(recordButton()).toBeDisabled();
    expect(
      within(recordingPanel()).getByText(/not attached to a recorder.*not beside this application/),
    ).toBeVisible();
  });

  /*
   * The recording that was just stopped leaves the panel with nothing to point
   * at otherwise: the state goes to "not recording" and the path goes with it,
   * which would leave somebody holding a file they had just made and no idea
   * where it is. The path shown is the one the recorder reported finishing.
   */
  it('names the file a stopped recording finished in', async () => {
    const user = userEvent.setup();
    let running = true;
    stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => (running ? recordingFor(9_000) : IDLE),
      stopRecording: () => {
        running = false;
        return FINISHED;
      },
    });
    renderApp();

    await waitFor(() => {
      expect(recordButton()).toHaveTextContent('Stop recording');
    });

    await user.click(recordButton());

    await waitFor(() => {
      expect(within(recordingPanel()).getByText(/Recording finished/)).toBeVisible();
    });
    expect(within(recordingPanel()).getByText(OUTPUT)).toBeVisible();
  });

  /*
   * The duration changes every second, and a screen reader reading a new one
   * aloud every second would drown the announcement the live region exists for
   * and make the screen unusable (AGENTS.md section 46). It stays in the
   * accessibility tree and is read on demand.
   */
  it('does not announce the duration once a second', async () => {
    stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => recordingFor(5_000),
    });
    renderApp();

    await waitFor(() => {
      expect(within(recordingPanel()).getByText(/^Recording for /)).toHaveAttribute(
        'aria-live',
        'off',
      );
    });
  });

  /* The record control has to be reachable and operable from the keyboard,
   * because a core workflow that needs a mouse fails AGENTS.md section 46. */
  it('has a record control that can be reached and pressed from the keyboard', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => IDLE,
      startRecording: () => ({ recording_id: 'rec-1', output: OUTPUT }),
    });
    renderApp();

    await waitFor(() => {
      expect(recordButton()).toBeEnabled();
    });

    recordButton().focus();
    await user.keyboard('{Enter}');

    await waitFor(() => {
      expect(
        runtime.invocations.filter((invocation) => invocation.command === 'start_recording'),
      ).toHaveLength(1);
    });
  });

  it('announces a change of state rather than only drawing it', async () => {
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    await waitFor(() => {
      expect(recordingPanel()).toHaveAttribute('aria-live', 'polite');
    });
  });

  /*
   * The record control is the only control the recording panel has, and the only
   * one on the screen while the library has not been read. Anything else
   * operable would have to do something, and nothing else here can (AGENTS.md
   * section 27).
   */
  it('offers the record control and no other', async () => {
    stubRecorderLinkRuntime(ATTACHED, null, {
      recordTarget: () => TARGET,
      recorderStatus: () => recordingFor(9_000),
    });
    renderApp();

    await waitFor(() => {
      expect(recordButton()).toBeInTheDocument();
    });

    const main = screen.getByRole('main');
    expect(within(main).getAllByRole('button')).toHaveLength(1);
    expect(within(main).queryAllByRole('link')).toHaveLength(0);
    expect(within(main).queryAllByRole('textbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('combobox')).toHaveLength(0);
    expect(within(main).queryAllByRole('checkbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('radio')).toHaveLength(0);
  });

  /*
   * The failure this exists for is the tempting one: four tiles reading "0
   * sessions", "0 clips", "0 favourites", "0 B", which look like a finished
   * screen on an empty library and are indistinguishable from one on a full
   * library the window could not read.
   *
   * It matches a bare figure against the nouns SPEC.md section 17 counts, so a
   * count of anything — zero or otherwise — fails until something has actually
   * been measured. Run over each text node rather than over the screen's
   * `textContent`, for the reason `test/counts.ts` sets out.
   */
  it('draws no count while the library has not been read', async () => {
    // The runtime here answers neither library command, so nothing has been
    // counted. A figure on this screen would then be one nobody measured.
    stubRecorderLinkRuntime(LINK_CLAIMS_RECORDING);
    renderApp();

    await waitFor(() => {
      expect(recordingPanel()).toBeInTheDocument();
    });

    for (const run of textRuns(screen.getByRole('main'))) {
      expect(run).not.toMatch(A_COUNT);
    }
  });

  /*
   * The other half of that property, and the point of issue #301: once the
   * library *has* been read, the figures on the screen are the index's own.
   * Both cases matter — the first would pass on a screen that can never show
   * anything, and the second on one that invents.
   */
  it('lists the most recent sittings once the library has been read', async () => {
    stubRecorderLinkRuntime(LINK_CLAIMS_RECORDING, null, {
      sessions: () =>
        Promise.resolve({
          sessions: [
            {
              session_id: 'cs2-20260811-201400',
              game_name: 'Counter-Strike 2',
              started_at: '2026-08-11T20:14:00+01:00',
              favourite: false,
              recordings: [
                {
                  recording_id: 12,
                  session_index: 1,
                  path: 'D:\\clips\\gone.mkv',
                  started_at: '2026-08-11T20:14:00+01:00',
                  duration_seconds: 6540,
                  size_bytes: 1024,
                  missing_since: '2026-08-12T09:00:00+01:00',
                  favourite: false,
                  tags: [],
                },
              ],
              clips: [],
            },
          ],
        }),
    });
    renderApp();

    const table = await screen.findByRole('table', { name: 'Recent sessions' });
    expect(table).toHaveTextContent('Counter-Strike 2');
    expect(table).toHaveTextContent('1 recording, 1 file missing');
  });

  it('draws the per-game figures the index computes, and what is missing from them', async () => {
    stubRecorderLinkRuntime(LINK_CLAIMS_RECORDING, null, {
      games: () =>
        Promise.resolve([
          {
            game_id: 'cs2',
            name: 'Counter-Strike 2',
            sessions: 214,
            recordings: 265,
            clips: 31,
            favourites: 12,
            bytes: 0,
            missing: 3,
          },
        ]),
    });
    renderApp();

    const table = await screen.findByRole('table', { name: 'Games' });
    const cells = within(within(table).getAllByRole('row')[1] as HTMLElement)
      .getAllByRole('cell')
      .map((cell) => cell.textContent);

    expect(cells[0]).toBe('Counter-Strike 2');
    expect(cells[1]).toBe('214');
    // A missing file contributes nothing to the size and is counted beside it,
    // in words rather than by colour alone (docs/library.md).
    expect(cells[3]).toBe('0 bytes');
    expect(cells[4]).toBe('3 missing');
  });

  it('says a library it could not read could not be read, rather than showing it as empty', async () => {
    stubRecorderLinkRuntime(LINK_CLAIMS_RECORDING, null, {
      sessions: () =>
        Promise.reject({
          code: 'library_unavailable',
          message: 'the recording library could not be opened: the drive is not connected',
        }),
    });
    renderApp();

    expect(await screen.findByText(/the drive is not connected/)).toBeInTheDocument();
    expect(screen.queryByRole('table', { name: 'Recent sessions' })).not.toBeInTheDocument();
  });

  /*
   * What SPEC.md section 17 asks of Home, and the issue that supplies each —
   * written out here rather than mapped from the screen's own array.
   *
   * That distinction is the whole test. A case that walked the rendered rows and
   * asserted "two cells, and the second names some issue" is satisfied by a
   * table holding one invented row. The claim is not "the rows have the right
   * shape", it is "these four are the four this screen owes, and each is pinned
   * to the work that lands it".
   */
  const MUST_BE_NAMED: readonly (readonly [string, RegExp, readonly number[]])[] = [
    ['recently clipped', /recently clipped/i, [74, 91]],
    ['favourites', /^favourites/i, [58]],
  ];

  it('names each list it still owes, and the issue that lands it', async () => {
    // Recent sessions and the per-game figures have left this list, because
    // both are on the screen now (issue #301). Shrinking it without honouring
    // that fails here, and the two cases below are what honour it.
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    // Neither library command is answered here, so the only table on the screen
    // is the one that says what it still owes.
    const waiting = await screen.findByRole('table');
    const rows = within(waiting).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(MUST_BE_NAMED.length);

    for (const [subject, shows, issues] of MUST_BE_NAMED) {
      const matching = rows.filter((row) =>
        shows.test(within(row).getAllByRole('cell')[0]?.textContent ?? ''),
      );
      expect(matching, `one row for ${subject}`).toHaveLength(1);

      const needs = within(matching[0] as HTMLElement).getAllByRole('cell')[1]?.textContent ?? '';
      for (const issue of issues) {
        expect(needs, `${subject} is waiting on #${String(issue)}`).toMatch(
          new RegExp(`#${String(issue)}\\b`),
        );
      }
    }
  });

  /*
   * Home is the screen the application opens on, so navigating *to* it from a
   * cold start is not a navigation at all — and the shell deliberately does not
   * move focus on the first screen. The path worth checking is coming back to
   * it, which is the one that has to move focus so that a screen reader
   * announces the change.
   *
   * The window therefore opens on Library, by putting the route in the fragment
   * before mounting, exactly as reopening the application on the screen it was
   * last on would. Everything after that is the keyboard alone, because
   * "keyboard navigation" is one of issue #60's three acceptance criteria and a
   * screen only reachable by mouse fails it whatever the shell does.
   */
  it('is reached with Tab and Enter, and takes focus when it opens', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    window.location.hash = '#/library';
    renderApp();
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Library');

    // Skip link, Home.
    await user.tab();
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Home');

    await user.keyboard('{Enter}');

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Home');
    expect(screen.getByRole('main')).toHaveFocus();
  });

  /*
   * Rendered on its own rather than through the shell, so that the screen's own
   * heading structure is the subject: a screen whose only heading was its title
   * gives a screen-reader user nothing to navigate between.
   */
  it('has a heading for each of its two parts', () => {
    render(<HomeScreen link={null} />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Home');
    expect(
      screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
    ).toContain('What this screen will show');
  });
});

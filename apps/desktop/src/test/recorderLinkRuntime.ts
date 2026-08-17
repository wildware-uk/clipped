import { vi } from 'vitest';

/**
 * A stubbed Tauri runtime for the `recorder-link` half of the interface.
 *
 * Shared by the hook's own tests and the shell's, because the two ask different
 * questions of the same arrangement — one that the hook keeps what it is told,
 * the other that what it keeps reaches the screen — and a second copy of this
 * would let them drift apart.
 *
 * Nothing here mocks `@tauri-apps/api`. `listen` is a thin wrapper that
 * registers its handler through `__TAURI_INTERNALS__.transformCallback` and
 * then invokes `plugin:event|listen`; standing that object up runs the real
 * wrapper, which is what makes the commands it sends worth asserting on.
 */

/** What the interface asked the Tauri runtime for. */
export interface Invocation {
  /** The command name, as Tauri's permission model names it. */
  readonly command: string;
  /** Its arguments. */
  readonly args: Record<string, unknown>;
}

/**
 * What the commands that go to the recorder answer with (issues #301 and #389).
 *
 * Functions rather than values, because the interesting cases are the ones that
 * depend on the arguments or on when they are asked: a second page has to answer
 * to the cursor the first one ended with, a search has to answer to the query,
 * and a window that follows a recorder has to be given a different status each
 * time it asks or there is no following to observe.
 *
 * The default for every one of them is a **rejection**, not an empty library and
 * not an idle recorder. A stub that answered "nothing recorded" by default would
 * let a screen test pass while the screen drew an empty library over a read that
 * never happened, or drew "not recording" over a recorder nobody asked — which
 * is the confusion both issues are shaped to prevent (AGENTS.md section 27).
 *
 * `recordTarget` is the exception, and it defaults to `null`: a test that has
 * not said what was in front of the window has had nothing in front of it, which
 * is a state the screen already has to draw honestly.
 */
export interface CommandAnswers {
  /** What `library_events` answers, given the request. */
  readonly events?: (args: Record<string, unknown>) => Promise<unknown>;
  /** What `library_sessions` answers, given the request. */
  readonly sessions?: (args: Record<string, unknown>) => Promise<unknown>;
  /** What `library_games` answers. */
  readonly games?: () => Promise<unknown>;
  /**
   * What `library_trash` answers.
   *
   * The default is a rejection, like the other library reads: a stub that
   * quietly answered with an empty trash would let a screen test pass while the
   * screen never asked, and "nothing has been deleted" is a claim.
   */
  readonly trash?: () => Promise<unknown>;
  /** What `restore_from_trash` answers, given what it was asked to restore. */
  readonly restoreFromTrash?: (args: Record<string, unknown>) => Promise<unknown>;
  /** What `empty_trash` answers, given the confirmation it was sent. */
  readonly emptyTrash?: (args: Record<string, unknown>) => Promise<unknown>;
  /**
   * What `set_favourite` answers, given the target and the state asked for.
   *
   * The default is a rejection, like the reads: a stub that quietly answered
   * would let a screen test watch a star fill in while the recorder was never
   * asked, which is the whole thing issue #58 was missing.
   */
  readonly setFavourite?: (args: Record<string, unknown>) => Promise<unknown>;
  /**
   * What `set_lock` answers, given the target and the state asked for.
   *
   * A rejection by default, like the rest: a padlock that filled itself in
   * while the recorder was never asked is exactly what issue #472 is about.
   */
  readonly setLock?: (args: Record<string, unknown>) => Promise<unknown>;
  /** What `record_target` answers: the process the button would record. */
  readonly recordTarget?: () => unknown;
  /**
   * What `recorder_hotkeys` answers: where every global hotkey stands.
   *
   * The default is a rejection, as the recorder commands' are. A stub that
   * quietly answered with an empty list would let a screen test pass while the
   * screen drew "no hotkey has a problem" over a recorder nobody asked, which is
   * the one thing the hotkey list exists to prevent (AGENTS.md section 27).
   */
  readonly recorderHotkeys?: () => unknown;
  /**
   * What `recorder_settings` answers: every setting, as the recorder holds it.
   *
   * The default is a rejection, as every recorder command's is. A stub that
   * quietly answered with an empty list of settings would let a screen test
   * pass while the screen drew a form over a recorder nobody asked (issue #51).
   */
  readonly recorderSettings?: () => unknown;
  /**
   * What `apply_recorder_settings` answers, given the values it was sent.
   *
   * The answer is the settings as they now stand, which is what the screen
   * redraws from — so a stub that returns the previous view is a recorder that
   * refused nothing and changed nothing, and the screen has to show that.
   */
  readonly applySettings?: (args: Record<string, unknown>) => unknown;
  /**
   * What `audio_devices` answers: the microphones this machine has.
   *
   * The default is a rejection, because "this machine has no microphone" and
   * "nobody asked" must not look the same on screen (AGENTS.md section 27).
   */
  readonly audioDevices?: () => unknown;
  /**
   * What `microphone_level` answers, given the microphone it was asked about.
   *
   * The default is a rejection, because a meter reading zero and a recorder
   * that was never asked must not look the same: one means "say something" and
   * the other means the screen is drawing a control over nothing (AGENTS.md
   * section 27, issue #109).
   */
  readonly microphoneLevel?: (args: Record<string, unknown>) => unknown;
  /**
   * Which folder the directory dialog says to record into, given what it was
   * opened with.
   *
   * `null` is the dialog being dismissed, which is a real answer and not a
   * failure.
   */
  readonly openDialog?: (args: Record<string, unknown>) => unknown;
  /**
   * What `get_status` answers, once per ask.
   *
   * Called again for every round of asking, so a case can move the recorder
   * underneath the window — which is the only way to see a window that follows
   * one rather than holding on to the first answer it got. Throwing is how a
   * recorder that cannot be reached is expressed; the thrown value is what
   * `invoke` rejects with, exactly as a `#[tauri::command]` returning `Err`
   * does.
   */
  readonly recorderStatus?: () => unknown;
  /** What `start_recording` answers, given the arguments it was sent. */
  readonly startRecording?: (args: Record<string, unknown>) => unknown;
  /** What `stop_recording` answers, given the arguments it was sent. */
  readonly stopRecording?: (args: Record<string, unknown>) => unknown;
  /** What `export_recording` answers, given the two paths it was sent. */
  readonly exportRecording?: (args: Record<string, unknown>) => unknown;
  /** What `open_recording` answers, given the path it was sent. */
  readonly openRecording?: (args: Record<string, unknown>) => unknown;
  /**
   * What `open_playback` answers, given the recording and the track it was
   * asked for.
   *
   * The default is a rejection, like the other recorder commands: a stub that
   * quietly answered with an address would let a screen test pass while the
   * screen drew a player over a recorder nobody asked (issue #304).
   */
  readonly openPlayback?: (args: Record<string, unknown>) => unknown;
  /** What `reveal_recording` answers, given the path it was sent. */
  readonly revealRecording?: (args: Record<string, unknown>) => unknown;
  /**
   * Where the Save As dialog says the MP4 should go, given what it was opened
   * with.
   *
   * `null` is the dialog being dismissed, which is a real answer and not a
   * failure. The default is a **rejection**, for the reason the recorder
   * commands' default is: a stub that quietly answered with a path would let an
   * export test pass over a dialog the screen never opened.
   */
  readonly saveDialog?: (args: Record<string, unknown>) => unknown;
}

/** What an unstubbed library command rejects with. */
const NO_LIBRARY_STUBBED = {
  code: 'no_recorder_configured',
  message: 'this test stubbed no library',
};

/** What an unstubbed recording command rejects with. */
const NO_RECORDER_STUBBED = {
  code: 'no_recorder_configured',
  message: 'this test did not say what the recorder answers; pass it to stubRecorderLinkRuntime',
};

/**
 * Runs one of the answers above, turning a throw into the rejection `invoke`
 * would produce.
 *
 * Rejected with the thrown value itself rather than an `Error`, because that is
 * what the real runtime does: `invoke` rejects with the serialised `Err` of the
 * command, which for these is a `RecorderProblem` object.
 */
function answered(answer: (() => unknown) | undefined): Promise<unknown> {
  if (answer === undefined) {
    return Promise.reject(NO_RECORDER_STUBBED);
  }
  try {
    return Promise.resolve(answer());
  } catch (refusal) {
    return Promise.reject(refusal);
  }
}

/** A stubbed runtime, and the things a test does with one. */
export interface StubbedRuntime {
  /** Every command the interface sent, in order. */
  readonly invocations: readonly Invocation[];
  /** Delivers one `recorder-link` event to whatever subscribed. */
  readonly emit: (payload: unknown) => void;
  /**
   * Delivers one event of any name, to the handlers that asked for that name.
   *
   * Routed rather than broadcast, because the window now subscribes to three
   * events — `recorder-link`, `tray-navigate` and `tray-notice` — and a stub
   * that handed every payload to every handler would let a test pass while the
   * interface listened for the wrong name.
   */
  readonly emitTo: (event: string, payload: unknown) => void;
}

/**
 * Makes this document look like the Clipped window.
 *
 * `answer` is what `recorder_link_state` resolves with — the snapshot the
 * window gets when it mounts.
 *
 * `startupNotice` is what `startup_notice` resolves with: something that failed
 * before this window existed, which for a real build is a notification-area icon
 * that could not be added. `null` is the ordinary answer and the default, and it
 * is answered *deliberately* rather than falling through to the identifier every
 * event command gets — a stub that handed back a number would let the interface
 * put one on screen as a sentence and the test still pass.
 *
 * A **promise** is accepted there as well, so that a test can hold the answer
 * back and let something else happen first. That is a real ordering — the
 * command is a round trip and the tray can say something while it is in flight —
 * and it is the only way to see which of the two ends up on screen.
 *
 * `commands` is what everything that goes to the recorder answers with. See
 * {@link CommandAnswers} for why each default is what it is.
 */
export function stubRecorderLinkRuntime(
  answer: unknown,
  startupNotice: string | null | Promise<string | null> = null,
  commands: CommandAnswers = {},
): StubbedRuntime {
  const invocations: Invocation[] = [];
  const handlers: ((event: unknown) => void)[] = [];
  /** Which event name each registered handler asked for, by its identifier. */
  const subscribedTo = new Map<number, string>();

  vi.stubGlobal('__TAURI_INTERNALS__', {
    metadata: { currentWindow: { label: 'main' } },
    transformCallback: (callback: (event: unknown) => void): number => {
      handlers.push(callback);
      return handlers.length;
    },
    invoke: (command: string, args: Record<string, unknown>): Promise<unknown> => {
      invocations.push({ command, args });
      if (command === 'recorder_link_state') {
        return Promise.resolve(answer);
      }
      if (command === 'startup_notice') {
        return Promise.resolve(startupNotice);
      }
      if (command === 'library_sessions') {
        return commands.sessions?.(args) ?? Promise.reject(NO_LIBRARY_STUBBED);
      }
      if (command === 'library_games') {
        return commands.games?.() ?? Promise.reject(NO_LIBRARY_STUBBED);
      }
      if (command === 'library_events') {
        return commands.events?.(args) ?? Promise.reject(NO_LIBRARY_STUBBED);
      }
      if (command === 'library_trash') {
        return commands.trash?.() ?? Promise.reject(NO_LIBRARY_STUBBED);
      }
      if (command === 'restore_from_trash') {
        return commands.restoreFromTrash?.(args) ?? Promise.reject(NO_LIBRARY_STUBBED);
      }
      if (command === 'empty_trash') {
        return commands.emptyTrash?.(args) ?? Promise.reject(NO_LIBRARY_STUBBED);
      }
      if (command === 'set_favourite') {
        return commands.setFavourite?.(args) ?? Promise.reject(NO_LIBRARY_STUBBED);
      }
      if (command === 'set_lock') {
        return commands.setLock?.(args) ?? Promise.reject(NO_LIBRARY_STUBBED);
      }
      if (command === 'record_target') {
        return Promise.resolve(commands.recordTarget?.() ?? null);
      }
      if (command === 'recorder_hotkeys') {
        return answered(commands.recorderHotkeys);
      }
      if (command === 'recorder_settings') {
        return answered(commands.recorderSettings);
      }
      if (command === 'apply_recorder_settings') {
        return answered(
          commands.applySettings === undefined ? undefined : () => commands.applySettings?.(args),
        );
      }
      if (command === 'audio_devices') {
        return answered(commands.audioDevices);
      }
      if (command === 'microphone_level') {
        return answered(
          commands.microphoneLevel === undefined
            ? undefined
            : () => commands.microphoneLevel?.(args),
        );
      }
      if (command === 'plugin:dialog|open') {
        return answered(
          commands.openDialog === undefined ? undefined : () => commands.openDialog?.(args),
        );
      }
      if (command === 'recorder_status') {
        return answered(commands.recorderStatus);
      }
      if (command === 'start_recording') {
        return answered(
          commands.startRecording === undefined ? undefined : () => commands.startRecording?.(args),
        );
      }
      if (command === 'stop_recording') {
        return answered(
          commands.stopRecording === undefined ? undefined : () => commands.stopRecording?.(args),
        );
      }
      if (command === 'export_recording') {
        return answered(
          commands.exportRecording === undefined
            ? undefined
            : () => commands.exportRecording?.(args),
        );
      }
      if (command === 'open_recording') {
        return answered(
          commands.openRecording === undefined ? undefined : () => commands.openRecording?.(args),
        );
      }
      if (command === 'open_playback') {
        return answered(
          commands.openPlayback === undefined ? undefined : () => commands.openPlayback?.(args),
        );
      }
      if (command === 'reveal_recording') {
        return answered(
          commands.revealRecording === undefined
            ? undefined
            : () => commands.revealRecording?.(args),
        );
      }
      if (command === 'plugin:dialog|save') {
        return answered(
          commands.saveDialog === undefined ? undefined : () => commands.saveDialog?.(args),
        );
      }
      if (command === 'plugin:event|listen') {
        // The real wrapper registers its callback first and then sends this,
        // so the identifier in `args.handler` is the one `transformCallback`
        // returned. Recording the pairing is what makes `emitTo` a delivery
        // rather than a broadcast.
        subscribedTo.set(Number(args['handler']), String(args['event']));
      }
      // Every event command answers with an identifier, which is all the
      // wrapper does anything with. `plugin:window|set_title` answers with
      // nothing, and ignores this.
      return Promise.resolve(1);
    },
  });
  // `unlisten` goes through this rather than through `__TAURI_INTERNALS__`, and
  // it is called when the window closes.
  vi.stubGlobal('__TAURI_EVENT_PLUGIN_INTERNALS__', {
    unregisterListener: (): void => undefined,
  });

  const emitTo = (event: string, payload: unknown): void => {
    handlers.forEach((handler, index) => {
      if (subscribedTo.get(index + 1) === event) {
        handler({ event, id: index + 1, payload });
      }
    });
  };

  return {
    invocations,
    emit: (payload: unknown): void => emitTo('recorder-link', payload),
    emitTo,
  };
}

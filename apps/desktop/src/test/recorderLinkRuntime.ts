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
 */
export function stubRecorderLinkRuntime(
  answer: unknown,
  startupNotice: string | null | Promise<string | null> = null,
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

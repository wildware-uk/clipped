import type { ReactNode } from 'react';

import { describeSettingsProblem, useStartAtLogin } from './settings';

/**
 * Whether the recorder starts when this user signs in, on the Settings screen
 * (issue #308).
 *
 * The second thing on this screen that is live and is not a setting, and it is
 * here for the same reason the hotkey list is: it is real, it matters, and the
 * only way to reach it was a terminal. `clipped-recorder start-at-login` has
 * written this value since issue #106; nothing in the window could read it, so
 * the screen used to print the command and ask somebody to go and run it.
 *
 * # Why the recorder writes it and this window does not
 *
 * The value is a command line, and the command line names the executable
 * Windows runs at sign-in — which is the **recorder**, a different program in a
 * different directory. A window writing a path it worked out from its own
 * location would leave a startup entry pointing at nothing whenever the two
 * were not where it assumed, and a startup entry that points at nothing fails
 * silently, once, at a sign-in nobody is watching. So the switch asks the
 * recorder, and the recorder writes its own path.
 *
 * # The third state
 *
 * On, off, and **on but pointing at nothing** — a Clipped that was moved or
 * reinstalled. It is not a variant of "off": Windows will still try the entry,
 * and somebody looking at a switch drawn in the off position would turn it on
 * and be told nothing had changed. It is drawn as on, with what is missing
 * named, and the thing to do about it is the switch itself: turning it on again
 * rewrites the entry with this installation's path.
 */
export function StartAtLoginSwitch(): ReactNode {
  const { read, save, set } = useStartAtLogin();

  return (
    <section className="clipped-panel" aria-label="Start at login">
      <h3 className="clipped-panel__heading">Starting at sign-in</h3>

      {read.state === 'reading' ? (
        <p className="clipped-panel__body">Asking the recorder…</p>
      ) : null}

      {/*
       * Said rather than drawn as a switch in the off position. "Clipped does
       * not start at sign-in" and "nobody could find out whether it does" are
       * opposite answers, and only one of them is fixed by pressing a switch
       * (AGENTS.md sections 27 and 45).
       */}
      {read.state === 'unread' ? (
        <p className="clipped-panel__body" role="status">
          Whether Clipped starts when you sign in could not be read, so this cannot be changed here.{' '}
          {describeSettingsProblem(read.problem)} You can still run{' '}
          <code className="clipped-code">clipped-recorder start-at-login status</code> in a
          terminal.
        </p>
      ) : null}

      {read.state === 'read' ? (
        <div className="clipped-panel__body">
          <div className="clipped-field">
            <label className="clipped-field__label" htmlFor="start-at-login">
              <input
                id="start-at-login"
                type="checkbox"
                checked={read.value.enabled}
                disabled={save.state === 'saving'}
                aria-describedby="start-at-login-hint"
                onChange={(event) => {
                  void set(event.target.checked);
                }}
              />{' '}
              Start the recorder when I sign in
            </label>

            <p className="clipped-muted" id="start-at-login-hint">
              The recorder is what records, watches for games and holds the replay buffer, so this
              is what makes Clipped work without opening anything. It writes one value under your
              own account — {read.value.location} — which Windows also lists in Settings &gt; Apps
              &gt; Startup, with a switch there too.
            </p>

            {/*
             * The command, because it is the whole of what will happen at the
             * next sign-in and there is nowhere else to see it short of a
             * registry editor.
             */}
            {read.value.command === undefined ? null : (
              <p className="clipped-muted">
                At sign-in Windows will run{' '}
                <code className="clipped-code">{read.value.command}</code>
              </p>
            )}

            {/*
             * Reported rather than repaired, and repaired only if asked: the
             * recorder does not rewrite somebody's startup entry because a
             * screen was opened.
             */}
            {read.value.missing_executable === undefined ? null : (
              <p role="status">
                That program is no longer at{' '}
                <code className="clipped-code">{read.value.missing_executable}</code>, so nothing
                will start. Clipped was probably moved or reinstalled. Turn this off and on again to
                point it at the copy you are using now.
              </p>
            )}

            {save.state === 'refused' ? (
              <p role="status">
                That could not be changed, so it is still {read.value.enabled ? 'on' : 'off'}.{' '}
                {describeSettingsProblem(save.problem)}
              </p>
            ) : null}
          </div>
        </div>
      ) : null}
    </section>
  );
}

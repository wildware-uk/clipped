import { useCallback, useEffect, useRef, type ReactNode } from 'react';

/** The regions of the application shell. */
export interface AppShellProps {
  /** The navigation lists, in sidebar order. */
  readonly nav: ReactNode;
  /** The recorder status block pinned to the foot of the sidebar. */
  readonly status: ReactNode;
  /**
   * Identifies the screen on show. When it changes, focus moves to the screen.
   * Without that, following a navigation link leaves focus in the sidebar and a
   * screen reader announces nothing: the window never reloaded, so the platform
   * has no reason to think anything happened.
   */
  readonly screenKey: string;
  /** The current screen. */
  readonly children: ReactNode;
}

/**
 * The window's frame: a title strip, a fixed sidebar, and the screen.
 *
 * The layout is a grid rather than a set of nested flex boxes so that the
 * sidebar's width and the header's height are stated once, as tokens, and the
 * screen gets whatever is left over. Only the sidebar's navigation list and the
 * screen itself scroll; the shell does not.
 */
export function AppShell({ nav, status, screenKey, children }: AppShellProps): ReactNode {
  const main = useRef<HTMLElement>(null);
  const firstScreen = useRef(true);

  useEffect(() => {
    // Not on the first screen: focus belongs where the window put it when it
    // opened, and stealing it would skip the sidebar before anybody has
    // navigated anywhere.
    if (firstScreen.current) {
      firstScreen.current = false;
      return;
    }
    main.current?.focus();
  }, [screenKey]);

  /*
   * The conventional skip link is an anchor to `#main`, which cannot be used
   * here: the router owns the fragment, so following such a link would navigate
   * to a screen that does not exist. A button that moves focus does the same
   * job for the same keystroke. `<main>` carries `tabIndex={-1}` so it can
   * receive that focus without joining the tab order.
   */
  const skipToContent = useCallback(() => {
    main.current?.focus();
  }, []);

  return (
    <div className="clipped-shell">
      <button type="button" className="clipped-skip-link" onClick={skipToContent}>
        Skip to content
      </button>

      <header className="clipped-header">
        <div className="clipped-header__mark" aria-hidden="true" />
        <span className="clipped-header__name">CLIPPED</span>
        <span className="clipped-header__tagline">Game Recorder</span>
      </header>

      <div className="clipped-shell__body">
        <div className="clipped-sidebar">
          <div className="clipped-sidebar__scroll">{nav}</div>
          <div className="clipped-sidebar__status">{status}</div>
        </div>

        <main className="clipped-shell__main" ref={main} tabIndex={-1}>
          {children}
        </main>
      </div>
    </div>
  );
}

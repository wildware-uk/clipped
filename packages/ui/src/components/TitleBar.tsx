import type { JSX } from 'react';

/**
 * What the title bar's buttons do.
 *
 * The shell does not call the window manager itself: `@clipped/ui` renders in
 * a test runner as readily as it does in a Tauri window, and it can only do
 * that if nothing in it imports a Tauri API. The application supplies these.
 */
export interface WindowControls {
    onMinimise: () => void;
    onToggleMaximise: () => void;
    onClose: () => void;
    /** Whether the window is maximised, which decides the middle button's label. */
    isMaximised: boolean;
}

export interface TitleBarProps {
    /** The product name, set in the wordmark. */
    name: string;
    /** A short subtitle beside it - what this application is. */
    tagline: string;
    windowControls: WindowControls;
}

/**
 * The application's own title bar.
 *
 * Clipped draws its own rather than using the system one because the design
 * system specifies it: a dark bar carrying the brand mark, with the window
 * controls flush right. The trade is that Windows 11's Snap Layouts flyout -
 * which appears when the pointer rests on a *system* maximise button - is not
 * available. Dragging a window to a screen edge still snaps it, and every
 * control here is a real button, so the keyboard path is unaffected.
 *
 * `data-tauri-drag-region` is what makes the bar draggable and makes a
 * double-click on it maximise the window, the way a system title bar does.
 * It is set on the brand block rather than the whole bar so that the region
 * cannot swallow a click meant for a button.
 */
export function TitleBar({ name, tagline, windowControls }: TitleBarProps): JSX.Element {
    const { onMinimise, onToggleMaximise, onClose, isMaximised } = windowControls;

    return (
        <header className="app-title-bar">
            <div className="app-title-bar-brand" data-tauri-drag-region>
                <div className="app-title-bar-mark" aria-hidden="true" />
                <span className="app-title-bar-name">{name.toUpperCase()}</span>
                <span className="app-title-bar-tagline">{tagline}</span>
            </div>

            <div className="app-window-controls">
                <button
                    type="button"
                    className="app-window-control"
                    onClick={onMinimise}
                    aria-label="Minimise"
                >
                    <MinimiseGlyph />
                </button>
                <button
                    type="button"
                    className="app-window-control"
                    onClick={onToggleMaximise}
                    aria-label={isMaximised ? 'Restore' : 'Maximise'}
                >
                    {isMaximised ? <RestoreGlyph /> : <MaximiseGlyph />}
                </button>
                <button
                    type="button"
                    className="app-window-control app-window-control-close"
                    onClick={onClose}
                    aria-label="Close"
                >
                    <CloseGlyph />
                </button>
            </div>
        </header>
    );
}

/* The three window-control glyphs, drawn rather than imported. The design
   system uses Lucide for iconography, and the desktop UI will too when it has
   icons worth a dependency (issue #79); a line, a square and a cross are not
   that. `currentColor` is what lets the close button turn its glyph over to
   the ground colour on hover without a second rule. */

const GLYPH_SIZE = 10;

function glyphProps(): { width: number; height: number; viewBox: string; 'aria-hidden': true } {
    return {
        width: GLYPH_SIZE,
        height: GLYPH_SIZE,
        viewBox: `0 0 ${GLYPH_SIZE} ${GLYPH_SIZE}`,
        'aria-hidden': true,
    };
}

function MinimiseGlyph(): JSX.Element {
    return (
        <svg {...glyphProps()}>
            <path d="M0 5h10" stroke="currentColor" strokeWidth="1" />
        </svg>
    );
}

function MaximiseGlyph(): JSX.Element {
    return (
        <svg {...glyphProps()}>
            <rect
                x="0.5"
                y="0.5"
                width="9"
                height="9"
                fill="none"
                stroke="currentColor"
                strokeWidth="1"
            />
        </svg>
    );
}

function RestoreGlyph(): JSX.Element {
    return (
        <svg {...glyphProps()}>
            <path
                d="M2.5 2.5h7v7"
                fill="none"
                stroke="currentColor"
                strokeWidth="1"
                strokeLinejoin="miter"
            />
            <rect
                x="0.5"
                y="4.5"
                width="7"
                height="5"
                fill="none"
                stroke="currentColor"
                strokeWidth="1"
            />
        </svg>
    );
}

function CloseGlyph(): JSX.Element {
    return (
        <svg {...glyphProps()}>
            <path d="M0.5 0.5l9 9M9.5 0.5l-9 9" stroke="currentColor" strokeWidth="1" />
        </svg>
    );
}

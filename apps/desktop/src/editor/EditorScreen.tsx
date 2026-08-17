import type { ReactNode } from 'react';

import { ClipEditor } from './ClipEditor';
import { readEditDocument } from './document';
import type { EventMark } from './events';
import { totalOutputNanos } from './timeline';

/**
 * The Editor screen (issue #83).
 *
 * # What an edit is
 *
 * A clip in Clipped is not a copy of a recording with the boring parts cut out.
 * It is a document that says which recordings to play, which parts of them, in
 * which order, how loud each audio track is and what text to draw over the
 * picture — and the recordings themselves are never modified, moved or
 * re-encoded because somebody made a clip (AGENTS.md sections 56 and 57,
 * `docs/editing.md`). Nothing on this screen or below it writes a file at all.
 *
 * # Why it usually has nothing open
 *
 * An edit document is stored as text in the library's database (issue #55), and
 * **this window cannot read a single row of it**: the desktop reaches the
 * recorder over the control protocol, which has no command about a library, and
 * reaches its own Tauri host through two commands, `recorder_link_state` and
 * `startup_notice`. It has no file-system permission either —
 * `src-tauri/capabilities/default.json` grants three `core:` permissions and
 * nothing else — and it may not link `clipped-library` or `clipped-edit`, which
 * `tests/integration/tests/workspace_layering.rs` asserts.
 *
 * So this screen is given the stored text of a clip's edit document and shows
 * it, and nothing supplies one yet. It says that, and names the work that
 * changes it, rather than drawing an empty timeline — an editor with an empty
 * timeline and a dead playhead is indistinguishable from a broken one, which is
 * what AGENTS.md section 27 forbids.
 */

/** One thing the editor will do, and the work that has to land before it can. */
interface Missing {
  /** What the editor will offer. */
  readonly does: string;
  /** What has to exist first, ending in the issue that builds it. */
  readonly needs: string;
}

/**
 * Everything SPEC.md section 19 asks of the editor that is not here, against
 * what each one is waiting for.
 *
 * The four operations in the second row are **built and tested**, in
 * `crates/edit` (issue #84). What is missing is not the arithmetic; it is any
 * way for this window to reach it, which is the same gap the first row is
 * about. A Split button drawn here would be a control with nothing behind it.
 */
const MISSING: readonly Missing[] = [
  {
    does: 'Open a clip, and save the edit back',
    needs:
      'A command that serves a clip’s edit document and takes an edited one back. The protocol reads the library (library_sessions, library_games, library_events) and says nothing about a clip’s document. Issue #306',
  },
  {
    does: 'Trim the start and end, split, and delete a section',
    needs:
      'The operations exist in crates/edit with undo and redo (issue #84). The controls need the same path to the document as opening one. Issues #84 and #306',
  },
  {
    does: 'Track volume, mute and fades; crop, rotate and speed; text overlays; combining recordings',
    needs: 'Issues #85, #86, #87 and #88, each of which owns its own control',
  },
  {
    does: 'Show what a plugin reported during the recording, on the timeline, filtered by kind',
    needs:
      'The lane, the filter and the arithmetic are built (issue #71), and the recorder serves the marks already placed in the file: library_events, which readEvents calls (issue #329). What is missing is a clip to draw them on — nothing opens one in this window yet. Issue #306',
  },
  {
    does: 'The picture at the playhead, and a waveform under each audio track',
    needs:
      'A path from the window to the recording itself: a frame to draw, and the peaks crates/waveform computes (issue #66). Issue #306',
  },
  {
    does: 'Export the clip to a file',
    needs:
      'The engine is built (issue #89) and the dialog in front of it says what an export of an open clip would be (issue #90). Starting one needs a command this window does not have. Issue #322',
  },
];

/** What the Editor screen is given. */
export interface EditorScreenProps {
  /**
   * The stored text of the clip's edit document, or `null` when nothing has
   * opened one.
   *
   * The text rather than a parsed document, because that is what crosses the
   * boundary: `docs/editing.md` settles that a document travels as the same
   * JSON `crates/edit` writes rather than being converted into a second
   * representation for the desktop. Nothing supplies one in this build, which
   * is what the screen says when it is `null`.
   */
  readonly clip?: string | null;
  /**
   * The game events of the recordings the clip draws on, each already placed in
   * one of them by `clipped_library::events` (issue #71).
   *
   * `null` — the only value this build can produce — means nobody has asked;
   * an empty list means the recordings had none. The editor says which of the
   * two it got rather than drawing the same empty lane for both.
   */
  readonly events?: readonly EventMark[] | null;
}

/** The Editor screen. */
export function EditorScreen({ clip = null, events = null }: EditorScreenProps): ReactNode {
  return (
    <>
      <h1 className="clipped-screen__title">Editor</h1>

      <p className="clipped-screen__lead">
        An edit is a document: which recordings to play, which parts of them, in which order, how
        loud each track is and what text to draw over the picture. Editing writes that document and
        nothing else — no recording is modified, moved or re-encoded because you made a clip.
      </p>

      {clip === null ? <NothingOpen /> : <Opened clip={clip} events={events} />}
    </>
  );
}

/** The state this build is always in: no clip has been opened. */
function NothingOpen(): ReactNode {
  return (
    <>
      <section className="clipped-panel" aria-label="Open clip">
        <h2 className="clipped-panel__heading">No clip is open</h2>
        <p className="clipped-panel__body">
          Nothing in this window can open one yet. A clip’s edit document is stored in the library’s
          database, and this window has no file-system permission to read it. The control protocol
          can be asked about the library — which sittings exist, what each game holds, the marks on
          a recording’s timeline — and says nothing about a clip’s document.
        </p>
        <p className="clipped-panel__body clipped-muted">
          Issue #306 is the way in: a command that serves a document and takes an edited one back.
        </p>
      </section>

      <h2 className="clipped-screen__heading">What the editor will do</h2>
      <p className="clipped-screen__lead clipped-muted">
        None of it is drawn against an invented clip in the meantime. Each row names the work that
        supplies it.
      </p>

      <table className="clipped-table">
        <thead>
          <tr>
            <th scope="col">What the editor will do</th>
            <th scope="col">What has to exist first</th>
          </tr>
        </thead>
        <tbody>
          {MISSING.map((entry) => (
            <tr key={entry.does}>
              <td>{entry.does}</td>
              <td className="clipped-muted">{entry.needs}</td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}

/**
 * A clip that was handed to the screen: the editor, or why it will not open.
 *
 * Both refusals are the same shape of statement — this is what is wrong, and
 * nothing has been changed — because a document that will not load has to say
 * why rather than leaving a blank screen behind.
 */
function Opened({
  clip,
  events,
}: {
  readonly clip: string;
  readonly events: readonly EventMark[] | null;
}): ReactNode {
  const read = readEditDocument(clip);
  if (!read.ok) {
    return <Refused problem={read.problem} />;
  }

  /*
   * A segment with a zero in its speed or a backwards span has no length, so
   * the timeline has no positions and there is nothing to draw a playhead
   * against. The model refuses to write one; a document that reached here
   * anyway is reported rather than drawn as an empty clip, which is what an
   * *empty* document — a user who deleted everything — legitimately is.
   */
  const durationNanos = totalOutputNanos(read.document);
  if (durationNanos === null) {
    return (
      <Refused problem="One of this clip’s segments has no length, so its timeline cannot be read. Nothing has been changed." />
    );
  }

  return <ClipEditor clip={read.document} durationNanos={durationNanos} events={events} />;
}

/** Why a clip did not open, in the one place that says it. */
function Refused({ problem }: { readonly problem: string }): ReactNode {
  return (
    <section className="clipped-panel" aria-label="Open clip">
      <h2 className="clipped-panel__heading">This clip cannot be opened</h2>
      <p className="clipped-panel__body">{problem}</p>
      <p className="clipped-panel__body clipped-muted">
        The clip is left exactly as it was, and so is every recording it refers to.
      </p>
    </section>
  );
}

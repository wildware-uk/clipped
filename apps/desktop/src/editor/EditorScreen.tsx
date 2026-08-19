import type { ClipDocument } from '@clipped/shared';
import type { ReactNode } from 'react';

import { ClipEditor } from './ClipEditor';
import { readEditDocument } from './document';
import type { EventMark } from '../events';
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
 * # Where the document comes from
 *
 * An edit document is stored as text in the library's database (issue #55), and
 * this window can read no row of it directly: it has no file-system permission
 * — `src-tauri/capabilities/default.json` is the whole of its privilege — and
 * it may not link `clipped-library` or `clipped-edit`, which
 * `tests/integration/tests/workspace_layering.rs` asserts.
 *
 * So it asks. `EditorRoute` fetches the document over the control protocol
 * (`library_clip_document`, issue #306) and hands the text here; this screen is
 * given a document and draws it, which keeps every state it can be in — a
 * document that will not parse, one from a newer build, one whose segments have
 * no length — a test that hands it a string.
 *
 * Opened from the sidebar with no clip named, it says what the editor is and
 * what is still being built, rather than drawing an empty timeline: an editor
 * with an empty timeline and a dead playhead is indistinguishable from a broken
 * one, which is what AGENTS.md section 27 forbids.
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
    does: 'Trim the start and end, split, and delete a section',
    needs:
      'The operations exist in crates/edit with undo and redo (issue #84). Opening a clip and saving one back are built (issue #306); what each control still needs is its own wiring to an operation. Issue #84',
  },
  {
    does: 'Track volume, mute and fades; crop, rotate and speed; text overlays; combining recordings',
    needs: 'Issues #85, #86, #87 and #88, each of which owns its own control',
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
   * What the recorder said about where that text came from, when it came from
   * the recorder at all.
   *
   * Absent when a test or a caller supplied the text directly. Present, it
   * carries the two facts a person editing wants and the document itself cannot
   * say: whether this clip has ever been edited, and whether the text stored
   * for it is in an older format than the one on screen.
   */
  readonly opened?: ClipDocument;
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
export function EditorScreen({ clip = null, events = null, opened }: EditorScreenProps): ReactNode {
  return (
    <>
      <h1 className="clipped-screen__title">Editor</h1>

      <p className="clipped-screen__lead">
        An edit is a document: which recordings to play, which parts of them, in which order, how
        loud each track is and what text to draw over the picture. Editing writes that document and
        nothing else — no recording is modified, moved or re-encoded because you made a clip.
      </p>

      {clip === null ? <NothingOpen /> : <Opened clip={clip} events={events} opened={opened} />}
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
          Choose a clip in the Library to edit it. A clip opens with the edit somebody saved for it,
          or — for a recording nobody has edited — with the whole of it, ready to cut.
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
  opened,
}: {
  readonly clip: string;
  readonly events: readonly EventMark[] | null;
  readonly opened: ClipDocument | undefined;
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

  return (
    <>
      <Provenance opened={opened} />
      <ClipEditor clip={read.document} durationNanos={durationNanos} events={events} />
    </>
  );
}

/**
 * What the recorder said about the document that was opened.
 *
 * Two facts the document itself cannot carry, and both change what somebody
 * should expect when they save:
 *
 * - **Nobody has edited this clip.** What is on screen was built from the
 *   recording rather than read from the library, and nothing is stored for this
 *   clip until a save. Saying so is the difference between "your edit is
 *   already here" and "this is where it starts".
 * - **The stored text is older than this.** Reading converted it in memory and
 *   changed nothing. Saving will store the converted document and keep the
 *   original, which is worth saying before somebody saves rather than after.
 *
 * Nothing is drawn when neither applies, which is the ordinary case.
 */
function Provenance({ opened }: { readonly opened: ClipDocument | undefined }): ReactNode {
  if (opened === undefined) {
    return null;
  }
  if (opened.converted_from !== undefined) {
    return (
      <p className="clipped-screen__lead clipped-muted">
        This clip was saved by an older version of Clipped (format {opened.converted_from}). It has
        been brought up to date to show here; nothing has been changed. Saving stores the newer
        version and keeps the original.
      </p>
    );
  }
  if (opened.synthesised) {
    return (
      <p className="clipped-screen__lead clipped-muted">
        Nobody has edited this clip yet, so it starts as the whole of what was recorded. Nothing is
        stored for it until you save.
      </p>
    );
  }
  return null;
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

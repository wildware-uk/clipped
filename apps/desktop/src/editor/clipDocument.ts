import type { ClipDocument, ClipDocumentSaved } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useEffect, useState } from 'react';

import { asProblem, type LibraryProblem, type LibraryRead } from '../library';

/**
 * Opening a clip in the editor, and saving one back (issue #306).
 *
 * # Why the window asks rather than reads
 *
 * A clip's edit document is text in a column of the library index, and this
 * window can reach neither: it has no file-system permission —
 * `src-tauri/capabilities/default.json` is the whole of its privilege — and it
 * may not link `clipped-library` or `clipped-edit`, which
 * `tests/integration/tests/workspace_layering.rs` asserts against the
 * dependency graph rather than trusting a comment. So both directions are a
 * Tauri command in front of a protocol command the recorder answers, exactly as
 * every other library read is (`../library.ts`, `docs/ipc.md`).
 *
 * # What this deliberately does not do
 *
 * It does not parse a document. {@link ClipDocument.document} is handed to
 * `./document.ts` untouched, because that is the one place in this window that
 * knows what an edit document is; a second reader here would be a second
 * implementation of the model to disagree with (AGENTS.md section 55).
 *
 * It does not convert one either, and cannot need to. The recorder converts an
 * older document before sending it and refuses one it cannot read, so what
 * arrives is always at the version `./document.ts` understands. That is what
 * makes that file's refusal of an older document correct rather than a gap.
 *
 * And nothing here can touch a recording. Saving writes one text column of one
 * row; no path added by this file opens a media file at all (AGENTS.md sections
 * 56 and 57).
 */

/** The three states a clip's document can be in on screen. */
export type ClipDocumentRead = LibraryRead<ClipDocument>;

/**
 * Reads one clip's edit document.
 *
 * Rejects with a {@link LibraryProblem} whose `code` is `edit_unreadable` when
 * the document is there and this build cannot read it — a clip saved by a newer
 * Clipped, above all. That is a different thing from a library that could not be
 * opened, and the editor says so differently.
 */
export async function readClipDocument(clip: string): Promise<ClipDocument> {
  return invoke<ClipDocument>('library_clip_document', { clip });
}

/**
 * Stores an edited document against a clip.
 *
 * The recorder validates before it writes, so a refusal means **nothing was
 * stored** and the clip is exactly as it was. It also keeps the text it
 * replaced when that text was in an older format, and says so in
 * {@link ClipDocumentSaved.superseded}.
 */
export async function saveClipDocument(clip: string, document: string): Promise<ClipDocumentSaved> {
  return invoke<ClipDocumentSaved>('save_clip_document', { clip, document });
}

/** A clip's document, and how to save an edited one back. */
export interface ClipDocumentView {
  /** What the recorder answered, or why it did not. */
  readonly read: ClipDocumentRead;
  /**
   * Stores an edited document, and leaves this view holding what was stored.
   *
   * Resolves with what the save did and rejects with why it did not, so that a
   * caller can say either. The view is only advanced on success: an editor that
   * showed the new document after a refused save would be showing something the
   * library does not hold.
   */
  readonly save: (document: string) => Promise<ClipDocumentSaved>;
}

/**
 * One clip's document, asked for when the clip changes.
 *
 * `clip` of `null` is "no clip has been chosen", which is not a read that
 * failed and not an empty one — it is the state the Editor screen is in when
 * somebody opens it from the sidebar rather than from a clip.
 */
export function useClipDocument(clip: string | null): ClipDocumentView {
  /**
   * The answer, and which clip it is about.
   *
   * The clip is stored *with* the answer rather than the state being reset when
   * `clip` changes, so that "this answer is for the clip being shown" is
   * derived below rather than set from inside the effect. Setting it there
   * would be a cascading render, and `react-hooks/set-state-in-effect` refuses
   * it; `../preview.ts`'s `useWaveform` keeps a stale answer out the same way.
   *
   * What must not happen either way: the previous clip's document staying on
   * screen while the next one is in flight, which is the one wrong thing an
   * editor can do with somebody's work.
   */
  const [answer, setAnswer] = useState<{ clip: string; read: ClipDocumentRead } | null>(null);

  useEffect(() => {
    if (clip === null) {
      return;
    }
    let current = true;
    readClipDocument(clip)
      .then((value) => {
        if (current) {
          setAnswer({ clip, read: { state: 'read', value } });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          setAnswer({ clip, read: { state: 'unread', problem: asProblem(thrown) } });
        }
      });
    return (): void => {
      current = false;
    };
  }, [clip]);

  const save = useCallback(
    async (document: string): Promise<ClipDocumentSaved> => {
      if (clip === null) {
        throw new Error('there is no clip open to save');
      }
      const saved = await saveClipDocument(clip, document);
      // What is on screen is now what is stored, and it is no longer a document
      // this window built: a saved replay stops being `synthesised` the moment
      // its document exists in the library.
      setAnswer({
        clip,
        read: { state: 'read', value: { clip, document, synthesised: false } },
      });
      return saved;
    },
    [clip],
  );

  const read: ClipDocumentRead =
    answer !== null && answer.clip === clip ? answer.read : { state: 'reading' };

  return { read, save };
}

export type { LibraryProblem };

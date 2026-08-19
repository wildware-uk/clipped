import type { ReactNode } from 'react';
import { useSearchParams } from 'react-router';

import { useClipDocument } from './clipDocument';
import { EditorScreen } from './EditorScreen';
import { CLIP_PARAMETER } from './route';

/**
 * The Editor route: a clip is named in the address, and its document is fetched
 * (issue #306).
 *
 * # Why the clip is in the address and not in a store
 *
 * The Editor is a sidebar destination — somebody can arrive at it with no clip
 * chosen — and it is also where a clip is opened *from* somewhere else. A search
 * parameter answers both without a second source of truth: `#/editor` is the
 * empty editor, `#/editor?clip=3` is clip 3, and the back button does what a
 * back button should. It is the same bargain the playback route already makes
 * for a recording (`../clipPlayback.ts`). `./route.ts` is where the address is
 * built, so that this file exports only components.
 *
 * # Why the fetching is here and not in `EditorScreen`
 *
 * `EditorScreen` is given the stored text of a document and draws it. That is
 * worth keeping pure: every one of its states — a document that will not parse,
 * one from a newer build, one whose segments have no length — is a test that
 * hands it a string, with no recorder and no round trip in the way. This is the
 * part that talks to the recorder, and it is the part with three states of its
 * own to draw.
 */

/** The Editor route. */
export function EditorRoute(): ReactNode {
  const [parameters] = useSearchParams();
  const clip = parameters.get(CLIP_PARAMETER);
  return <EditorFor clip={clip} />;
}

/**
 * The editor for one clip, or for none.
 *
 * Exported for its tests, which drive it with a clip rather than with a route.
 */
export function EditorFor({ clip }: { readonly clip: string | null }): ReactNode {
  const { read } = useClipDocument(clip);

  if (clip === null) {
    // No clip chosen. `EditorScreen` says what the editor is and what still has
    // to be built, which is a different thing from a clip that would not open.
    return <EditorScreen />;
  }

  if (read.state === 'reading') {
    return (
      <>
        <h1 className="clipped-screen__title">Editor</h1>
        <section className="clipped-panel" aria-label="Open clip">
          <p className="clipped-panel__body">Opening clip {clip}…</p>
        </section>
      </>
    );
  }

  if (read.state === 'unread') {
    // Why it did not open, in the recorder's own words. A recorder built before
    // this command existed refuses by name (`unknown_command`), and that is
    // worth saying plainly rather than showing as a broken clip: the remedy is
    // a Clipped that is up to date, not a different clip.
    return (
      <>
        <h1 className="clipped-screen__title">Editor</h1>
        <section className="clipped-panel" aria-label="Open clip">
          <h2 className="clipped-panel__heading">This clip cannot be opened</h2>
          <p className="clipped-panel__body">
            {read.problem.code === 'unknown_command'
              ? 'This copy of Clipped’s recorder cannot open clips for editing. Update Clipped.'
              : read.problem.message}
          </p>
          <p className="clipped-panel__body clipped-muted">
            The clip is left exactly as it was, and so is every recording it refers to.
          </p>
        </section>
      </>
    );
  }

  return <EditorScreen clip={read.value.document} opened={read.value} />;
}

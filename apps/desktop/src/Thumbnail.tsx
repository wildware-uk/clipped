import type { Preview } from '@clipped/shared';
import type { ReactNode } from 'react';

import {
  describePreviewProblem,
  headlinePreviewProblem,
  pictureUri,
  useThumbnail,
} from './preview';

/**
 * A recording's thumbnail in a list, and what stands in its place when there is
 * not one (issue #448).
 *
 * # Why there is anything here at all
 *
 * #448's second acceptance criterion: *a recording with no thumbnail yet is
 * distinguishable on screen from one whose thumbnail could not be read.* Two
 * empty squares satisfy neither the criterion nor AGENTS.md section 27 — "not
 * made yet" is the ordinary state of a recording written five seconds ago and
 * resolves itself, and "there will not be one" is a fact about a file that never
 * will. Somebody who cannot tell them apart cannot tell whether waiting is worth
 * anything.
 *
 * So each state carries a **word**, and the word is what tells them apart —
 * never a shade, and never the presence or absence of a tile (AGENTS.md section
 * 46). The tile itself is the same in every case: the dark ground a picture
 * would have sat on, so that a row without one is a row with an empty frame
 * rather than a hole where the layout failed.
 *
 * # Why the stand-in is `role="img"` and not just text
 *
 * Every state occupies the same slot and answers the same question, so every
 * state answers it the same way: one thing in the accessibility tree, with a
 * name that says which of the four it is and, where there is one, why. A screen
 * reader moving down the column hears "Thumbnail of cs2-…mkv", "No thumbnail
 * yet for …", "No thumbnail for …: that file has no video stream" — the same
 * distinction the eye gets from the word in the tile. The name carries the
 * detail because a tile 96 pixels wide cannot.
 */

/** What a thumbnail is drawn for. */
export interface ThumbnailProps {
  /**
   * The recording to draw, or `null` to draw nothing and ask nothing.
   *
   * `null` is what a caller passes when the recorder does not advertise
   * `previews`: an older recorder would refuse once per row, and a column of
   * refusals is worse than a column that was never there (issue #447's lesson,
   * one screen along).
   */
  readonly source: string | null;
  /**
   * What to call the recording, for a screen reader.
   *
   * The same string the row's controls name themselves with, because a table of
   * forty recordings otherwise announces forty identical images.
   */
  readonly of: string;
}

/** One recording's thumbnail, in whichever of the four states it is in. */
export function Thumbnail({ source, of }: ThumbnailProps): ReactNode {
  const view = useThumbnail(source);

  switch (view.state) {
    case 'unasked':
      // Nothing, rather than a tile saying the recorder is too old: that is a
      // fact about the recorder and belongs where the recorder is described,
      // not repeated down every row of the library.
      return null;
    case 'asking':
      // No word, because nothing is known yet — which is itself distinct from
      // the three below, all of which say something.
      return <Absent name={`Reading the thumbnail for ${of}`} busy />;
    case 'refused':
      return (
        <Absent
          name={`${headlinePreviewProblem(view.problem)}: ${of}. ${describePreviewProblem(
            view.problem,
          )}`}
        >
          Not read
        </Absent>
      );
    case 'answered':
      return <Answered preview={view.preview} of={of} />;
  }
}

/** What the recorder said, drawn. */
function Answered({ preview, of }: { readonly preview: Preview; readonly of: string }): ReactNode {
  const picture = preview.picture;

  if (preview.state === 'ready' && picture !== undefined) {
    return (
      <img
        className="clipped-thumb"
        src={pictureUri(picture)}
        alt={
          picture.blank === true
            ? // The generator found every candidate frame to be a flat colour and
              // says so. Drawing it is honest — that is what the recording looks
              // like — but a flat rectangle with no explanation reads as a
              // failure, so the name carries the explanation.
              `Thumbnail of ${of}. Every frame Clipped tried was a flat colour.`
            : `Thumbnail of ${of}`
        }
      />
    );
  }

  if (preview.state === 'pending') {
    return (
      <Absent name={`No thumbnail yet for ${of}. Clipped is making one.`}>Not made yet</Absent>
    );
  }

  /*
   * `unavailable`, and also a `ready` that carried no picture — a recorder
   * contradicting itself, which is drawn as "there is not one" rather than as
   * "not yet". Promising a picture that has already been answered for is the one
   * thing this tile must not do, and the reason field says what it says either
   * way: the recording is gone, holds no video, or could not be opened.
   */
  const why = preview.reason === undefined ? '' : ` ${preview.reason}`;
  return <Absent name={`No thumbnail for ${of}.${why}`}>No picture</Absent>;
}

/**
 * The tile a recording with no picture gets.
 *
 * `role="img"` with a name of its own, so that the four states are four
 * differently-named images rather than three images and a fragment of text —
 * see the module note. Its children are the word on screen and are not
 * announced, which is deliberate: the name already says the whole of it, and a
 * reader hearing "Not made yet" twice learns nothing the second time.
 */
function Absent({
  name,
  busy,
  children,
}: {
  readonly name: string;
  readonly busy?: boolean;
  readonly children?: ReactNode;
}): ReactNode {
  return (
    <span
      className="clipped-thumb clipped-thumb--absent"
      role="img"
      aria-label={name}
      {...(busy === true ? { 'aria-busy': true } : {})}
    >
      {children}
    </span>
  );
}

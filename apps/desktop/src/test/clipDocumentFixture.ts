import type { ClipDocument, ClipDocumentSaved } from '@clipped/shared';

import schema from '../../../../packages/shared/src/ipc/protocol-schema.json';

/**
 * What the recorder answers when a clip is opened, taken from the recorder's
 * own exemplar rather than typed out here.
 *
 * `protocol-schema.json` is written by `cargo run -p clipped-ipc --bin
 * protocol-schema` from the Rust types themselves, and three of the frames in
 * it are the replies this window's editor lives on: a clip whose document the
 * library held, one that had none and was given a starting document, and a save
 * that kept the older text it replaced. Everything below is one of those
 * frames — so `converted_from` becoming required, or `synthesised` being
 * renamed, fails these tests instead of leaving them agreeing with a protocol
 * that has moved.
 *
 * It is the standard PR #647 and PR #652 set, for the reason
 * `eventMarkFixture.ts` gives: a hand-typed literal is a copy of the protocol
 * that nothing keeps honest.
 *
 * # Not the same fixture as `editDocumentFixture.ts`
 *
 * That one is a three-segment clip carrying the figures `crates/edit`'s own
 * timeline tests use, and it exists so the window's arithmetic can be held
 * against the crate's. This one is about the *transport* — the shape of the
 * reply and the text inside it — and the two should not be merged: a fixture
 * that had to serve both would be pinned to the protocol and to the arithmetic
 * at once, and could not change for either.
 */

/** A sample frame the Rust build produced, by the name it recorded it under. */
function sample(name: string): Record<string, unknown> {
  const samples = schema.samples as { name: string; frame: Record<string, unknown> }[];
  const found = samples.find((candidate) => candidate.name === name);
  if (found === undefined) {
    throw new Error(
      `protocol-schema.json has no sample called "${name}"; regenerate it, or the name has moved`,
    );
  }
  return found.frame;
}

function replyPayload<T>(name: string, field: string): T {
  const frame = sample(name) as { outcome: { ok: Record<string, unknown> } };
  return frame.outcome.ok[field] as T;
}

/** The recorder's answer for a clip whose document the library held. */
export const STORED_CLIP: ClipDocument = replyPayload("a clip's edit document", 'clip');

/** Its answer for a clip nobody has ever edited. */
export const SYNTHESISED_CLIP: ClipDocument = replyPayload(
  'the starting document of a clip nobody has edited',
  'clip',
);

/** Its answer to a save that kept the older text it replaced. */
export const SUPERSEDING_SAVE: ClipDocumentSaved = replyPayload(
  'an edited document stored, keeping the older text it replaced',
  'saved',
);

/** The clip those exemplars are about, as the library identifies it. */
export const SAMPLE_CLIP = STORED_CLIP.clip;

/**
 * The exemplar's document text, which is a real `clipped_edit` document.
 *
 * `tests/integration/tests/edit_documents_cross_the_protocol.rs` holds this
 * string against what `EditDocument::write` actually produces, so it is not a
 * plausible-looking literal — it is the writer's output.
 */
export const SAMPLE_DOCUMENT = STORED_CLIP.document;

/** The same answer, for a different clip. */
export function clipDocumentOf(clip: string, of: ClipDocument = STORED_CLIP): ClipDocument {
  return { ...of, clip };
}

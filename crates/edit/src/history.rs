//! Undo and redo, as the editor's document plus what it used to be.
//!
//! # Whole documents, not inverse operations
//!
//! The other way to build undo is to record each operation and work out how to
//! reverse it. It is rejected here for the reason [`crate::segment`] rejects
//! storing a cut as an instruction: an inverse has to reproduce state the
//! operation destroyed — the overlay a deletion dropped, the fade a trim
//! shortened — so every operation added later brings a second implementation of
//! itself that has to stay in step, and the failure when it does not is a user
//! pressing Ctrl+Z and getting *nearly* their clip back.
//!
//! A document is a title, a handful of segments and a few lines of text. Keeping
//! the whole of it makes "undo and redo restore exact prior state" ([issue
//! #84](https://github.com/wildware-uk/clipped/issues/84)) true by construction
//! rather than by care, and it costs a clone of something measured in hundreds
//! of bytes.
//!
//! # Depth
//!
//! [`MAX_UNDO_STEPS`] deep, oldest dropped first. A bound rather than a growing
//! list because this lives in the desktop process for as long as the editor is
//! open, and an unbounded history is a slow leak that only shows up for the
//! user who edits all afternoon. Redo is bounded by the same thing: nothing can
//! be redone that was not undone first.
//!
//! # Threading
//!
//! Plain data, owned by whoever holds it — the editor's own thread. Nothing
//! here is shared or interior-mutable, and an export takes a clone of the
//! [`document`](EditHistory::document) rather than a reference into a history
//! somebody is still editing.

use crate::document::EditDocument;
use crate::error::OperationRefused;
use crate::operations::EditOperation;

/// How many edits can be undone.
///
/// Fifty is the depth of a session's worth of trimming, not of an afternoon's:
/// far past what anybody undoes in practice, and small enough that the memory
/// is a rounding error beside the video the editor is showing.
pub const MAX_UNDO_STEPS: usize = 50;

/// An edit document and the states it can be put back into.
///
/// This is what the editor ([issue
/// #83](https://github.com/wildware-uk/clipped/issues/83)) holds while a clip is
/// open. Operations go through [`apply`](Self::apply) rather than through
/// [`EditDocument::apply`] directly, because that is what records the step.
#[derive(Debug, Clone, PartialEq)]
pub struct EditHistory {
    /// The document as it is now.
    document: EditDocument,
    /// What it was before each applied operation, oldest first.
    past: Vec<EditDocument>,
    /// What it was before each undo, oldest first.
    future: Vec<EditDocument>,
}

impl EditHistory {
    /// A history holding `document`, with nothing to undo yet.
    #[must_use]
    pub fn new(document: EditDocument) -> Self {
        Self {
            document,
            past: Vec::new(),
            future: Vec::new(),
        }
    }

    /// The document as it is now.
    #[must_use]
    pub fn document(&self) -> &EditDocument {
        &self.document
    }

    /// The document as it is now, giving up the history that produced it.
    #[must_use]
    pub fn into_document(self) -> EditDocument {
        self.document
    }

    /// Whether there is a state to go back to.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        !self.past.is_empty()
    }

    /// Whether there is an undone state to go forward to.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        !self.future.is_empty()
    }

    /// Applies `operation`, recording a step that [`undo`](Self::undo) reverses.
    ///
    /// Returns whether the document changed. `false` means the operation had
    /// nothing to do — splitting where a boundary already is — and nothing is
    /// recorded: an undo step that restores an identical document is a Ctrl+Z
    /// that appears to do nothing, which is worse than the button that appeared
    /// to do nothing in the first place. A caller that has to tell the user
    /// something has this to tell them with.
    ///
    /// A new operation clears the redo stack, as every editor's does: the
    /// states that were ahead were reached from a document that no longer
    /// exists.
    ///
    /// # Errors
    ///
    /// [`OperationRefused`]. The history is untouched, including the redo
    /// stack — a refused operation is not an edit.
    pub fn apply(&mut self, operation: EditOperation) -> Result<bool, OperationRefused> {
        let edited = self.document.apply(operation)?;
        if edited == self.document {
            return Ok(false);
        }

        if self.past.len() >= MAX_UNDO_STEPS {
            self.past.remove(0);
        }
        self.past
            .push(core::mem::replace(&mut self.document, edited));
        self.future.clear();
        Ok(true)
    }

    /// Goes back one step, or reports that there is nowhere to go.
    pub fn undo(&mut self) -> bool {
        let Some(previous) = self.past.pop() else {
            return false;
        };
        self.future
            .push(core::mem::replace(&mut self.document, previous));
        true
    }

    /// Goes forward one step, undoing an undo.
    pub fn redo(&mut self) -> bool {
        let Some(next) = self.future.pop() else {
            return false;
        };
        self.past.push(core::mem::replace(&mut self.document, next));
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::segment::Segment;
    use crate::source::{RecordingId, Source, SourceId};
    use crate::time::{OutputSpan, OutputTime, SourceSpan, SourceTime};

    const SECOND: u64 = 1_000_000_000;

    fn document() -> EditDocument {
        let source = SourceId::new(0);
        EditDocument::new("Ace")
            .with_source(Source::new(source, RecordingId::new("rec-1")))
            .with_segment(Segment::new(
                source,
                SourceSpan::new(SourceTime::ZERO, SourceTime::from_nanos(30 * SECOND))
                    .expect("the test span ends after it starts"),
            ))
    }

    fn at(seconds: u64) -> OutputTime {
        OutputTime::from_nanos(seconds * SECOND)
    }

    #[test]
    fn undo_and_redo_walk_back_and_forth_through_exactly_the_documents_that_were_there() {
        let start = document();
        let mut history = EditHistory::new(start.clone());
        assert!(!history.can_undo());
        assert!(!history.can_redo());

        // Three operations, keeping what the document looked like after each.
        let mut expected = vec![start.clone()];
        for operation in [
            EditOperation::TrimStart { at: at(2) },
            EditOperation::Split { at: at(10) },
            EditOperation::DeleteSection {
                range: OutputSpan::new(at(4), at(6)).expect("a valid range"),
            },
        ] {
            assert!(history.apply(operation).expect("the operation applies"));
            expected.push(history.document().clone());
        }

        // Back to the start, one state at a time.
        for state in expected.iter().rev().skip(1) {
            assert!(history.can_undo());
            assert!(history.undo());
            assert_eq!(
                history.document(),
                state,
                "undo must restore the exact prior state"
            );
            // Not merely equal as a value: the same document to whatever reads
            // it, down to the text that would reach the database.
            assert_eq!(
                history.document().write().expect("it saves"),
                state.write().expect("it saves")
            );
        }
        assert!(!history.can_undo());
        assert!(!history.undo());

        // And forward again through the same states.
        for state in expected.iter().skip(1) {
            assert!(history.can_redo());
            assert!(history.redo());
            assert_eq!(history.document(), state);
        }
        assert!(!history.can_redo());
        assert!(!history.redo());
    }

    #[test]
    fn an_operation_after_an_undo_abandons_what_was_ahead() {
        let mut history = EditHistory::new(document());
        history
            .apply(EditOperation::TrimEnd { at: at(20) })
            .expect("the operation applies");
        assert!(history.undo());
        assert!(history.can_redo());

        history
            .apply(EditOperation::TrimEnd { at: at(10) })
            .expect("the operation applies");

        assert!(
            !history.can_redo(),
            "the state that was ahead came from a document that no longer exists"
        );
        assert_eq!(
            history.document().output_duration_nanos(),
            Some(10 * SECOND)
        );
    }

    #[test]
    fn an_operation_with_nothing_to_do_records_no_step() {
        let mut history = EditHistory::new(document());
        history
            .apply(EditOperation::Split { at: at(10) })
            .expect("the operation applies");

        assert!(
            !history
                .apply(EditOperation::Split { at: at(10) })
                .expect("splitting a boundary that exists is not an error"),
            "the boundary is already there, so the document did not change"
        );

        assert!(history.undo());
        assert_eq!(
            history.document(),
            &document(),
            "one undo is enough to get back, because only one step was recorded"
        );
    }

    #[test]
    fn a_refused_operation_leaves_the_history_exactly_as_it_was() {
        let mut history = EditHistory::new(document());
        history
            .apply(EditOperation::TrimEnd { at: at(20) })
            .expect("the operation applies");
        assert!(history.undo());
        let before = history.clone();

        let refused = history.apply(EditOperation::Split { at: at(99) });

        assert!(refused.is_err());
        assert_eq!(
            history, before,
            "a refusal is not an edit: the document, the undo stack and the redo stack all stand"
        );
    }

    #[test]
    fn the_history_stops_growing_and_drops_the_oldest_step_first() {
        let mut history = EditHistory::new(document());

        // One nanosecond off the end at a time: enough operations to overflow
        // the history several times over, each one a real change.
        let steps = u64::try_from(MAX_UNDO_STEPS).expect("the depth is a small number") + 10;
        for step in 1..=steps {
            assert!(history
                .apply(EditOperation::TrimEnd {
                    at: OutputTime::from_nanos(30 * SECOND - step),
                })
                .expect("the operation applies"));
        }

        let mut undone = 0;
        while history.undo() {
            undone += 1;
        }
        assert_eq!(undone, MAX_UNDO_STEPS);
        assert_eq!(
            history.document().output_duration_nanos(),
            Some(30 * SECOND - 10),
            "the oldest states were dropped, so the earliest one still reachable is the \
             eleventh edit's"
        );
    }
}

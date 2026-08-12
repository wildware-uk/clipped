//! Case folding: the one text comparison the whole query language is built on.
//!
//! Every text comparison in a query — a bare word, a quoted phrase, `game:`,
//! `tag:` — is a case-insensitive substring test, and it is made here so that
//! there is exactly one answer to "does this text match?" for the matcher, for
//! the parser and for whatever eventually builds a database index
//! ([`fold`] is public for that third caller).

/// The case-folded form of `text`.
///
/// Folding is [`str::to_lowercase`], which is Unicode-aware, rather than
/// [`str::to_ascii_lowercase`], which is not. A library holds whatever the user
/// plays and whatever they typed into a tag, and `ЗАМЕС` failing to match
/// `замес` is not a nicer bug for being rarer than the same failure on `KILL`.
/// This is the same choice `clipped-game-detection` makes when it compares two
/// executable names, deliberately: two spellings of "the same text ignoring
/// case" in one product would be a difference nobody could predict.
///
/// **Folding changes length, in bytes and in characters.** `GROẞE` is the three
/// bytes of `U+1E9E` and folds to the two bytes of `ß`; `İ` is one character
/// and folds to two. Nothing in this module may compare lengths to
/// short-circuit a comparison — that check is exactly the bug that broke
/// non-ASCII matching in `clipped-game-detection`'s `equal_names`, because it
/// rejects the pairs case folding exists to accept. The comparison is made on
/// the folded forms and on nothing else.
///
/// Folding is *only* case. Diacritics are kept, so `pokémon` does not match a
/// search for `pokemon`; that is a deliberate limit, documented in
/// `docs/search.md`, and not something to fix by half.
///
/// It is also *lower-casing* rather than full Unicode case folding, and the two
/// differ in one place worth naming: `İ` (U+0130) lower-cases to `i` plus a
/// combining dot above, so an ASCII `istanbul` does not find `İSTANBUL`.
/// `lower_casing_is_not_full_case_folding_for_the_dotted_capital_i` pins that,
/// and `docs/search.md` says so where it says the same about diacritics.
#[must_use]
pub fn fold(text: &str) -> String {
    text.to_lowercase()
}

/// Text kept beside its [folded](fold) form.
///
/// Both halves are needed. The folded form is what comparisons are made
/// against, and the original is what a caller shows the user, writes back into
/// a search box, or binds into a query it builds elsewhere. Folding happens
/// once, when the value is created: a query is parsed once and matched against
/// many rows, and a row is built once and matched against many queries, so
/// neither side should be folding inside the matching loop.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FoldedText {
    text: String,
    folded: String,
}

impl FoldedText {
    /// Folds `text` and keeps both forms.
    #[must_use]
    pub fn new(text: impl Into<String>) -> Self {
        let text = text.into();
        let folded = fold(&text);
        Self { text, folded }
    }

    /// The text as it was written.
    #[must_use]
    pub fn text(&self) -> &str {
        &self.text
    }

    /// The folded form comparisons are made against.
    #[must_use]
    pub fn folded(&self) -> &str {
        &self.folded
    }

    /// Whether this text contains `needle`, ignoring case.
    ///
    /// An empty needle would match everything, which is why the parser refuses
    /// to build one (`game:""` is an error, not a query that selects the
    /// library).
    #[must_use]
    pub fn contains(&self, needle: &Self) -> bool {
        self.folded.contains(&needle.folded)
    }
}

#[cfg(test)]
mod tests {
    use super::{fold, FoldedText};

    #[test]
    fn folding_is_unicode_aware_rather_than_ascii_only() {
        assert_eq!(fold("ЗАМЕС"), "замес");
        assert_eq!(fold("ÉLAN"), "élan");
        assert_eq!(fold("Counter-Strike 2"), "counter-strike 2");
    }

    /// The regression `clipped-game-detection`'s `equal_names` documents: case
    /// folding changes length, so any comparison that checks lengths first
    /// rejects the pair it was supposed to accept.
    #[test]
    fn a_match_survives_folding_changing_the_length_of_the_text() {
        let capital_sharp_s = "GROẞE";
        assert_eq!(capital_sharp_s.len(), 7, "U+1E9E is three bytes");
        assert_eq!(fold(capital_sharp_s).len(), 6, "ß is two bytes");

        let row = FoldedText::new(capital_sharp_s);
        assert!(row.contains(&FoldedText::new("große")));
        assert!(FoldedText::new("große").contains(&FoldedText::new(capital_sharp_s)));

        let dotted_capital_i = "İ";
        assert_eq!(dotted_capital_i.chars().count(), 1);
        assert_eq!(
            fold(dotted_capital_i).chars().count(),
            2,
            "folding U+0130 yields `i` and a combining dot"
        );
        assert!(FoldedText::new("FINAL İN İSTANBUL").contains(&FoldedText::new("İstanbul")));
    }

    #[test]
    fn a_substring_is_found_whatever_case_either_side_was_written_in() {
        let title = FoldedText::new("Оверпасс: Финальный Раунд");
        assert!(title.contains(&FoldedText::new("финальный")));
        assert!(title.contains(&FoldedText::new("ФИНАЛЬНЫЙ")));
        assert!(!title.contains(&FoldedText::new("инферно")));
    }

    /// The limit of lower-casing, as opposed to full case folding. `İ` is one
    /// character that lower-cases to two, so `İSTANBUL` folds to `i`, a
    /// combining dot above, and `stanbul` — and a user typing the ordinary
    /// ASCII `istanbul` does not find it. Writing it down here is what keeps
    /// `docs/search.md` honest about which comparisons ignore case.
    #[test]
    fn lower_casing_is_not_full_case_folding_for_the_dotted_capital_i() {
        assert_eq!(fold("İSTANBUL"), "i\u{307}stanbul");
        assert!(
            !FoldedText::new("İSTANBUL").contains(&FoldedText::new("istanbul")),
            "an ASCII i does not reach a dotted capital I"
        );
        assert!(
            FoldedText::new("İSTANBUL").contains(&FoldedText::new("İstanbul")),
            "the same letter on both sides folds the same way, and does match"
        );
    }

    #[test]
    fn folding_leaves_diacritics_alone() {
        assert!(!FoldedText::new("Pokémon").contains(&FoldedText::new("pokemon")));
        assert!(FoldedText::new("Pokémon").contains(&FoldedText::new("POKÉMON")));
    }
}

//! Choosing one window out of the ones that were enumerated.
//!
//! Everything in this module is a pure function of a list of [`WindowInfo`] and
//! a [`TargetSelector`]. It calls no Windows API, and it is the half of target
//! selection that can be — and is — tested exhaustively on a machine with no
//! desktop, against desktops constructed in the test. Enumerating the real
//! desktop is [`crate::enumerate_windows`], and it is deliberately not mixed in
//! here: "which windows exist?" needs Windows, "which of these did the user
//! mean?" does not, and only one of those two questions has interesting edge
//! cases.
//!
//! # Ambiguity is an answer, not a problem to be smoothed over
//!
//! `--window chrome` on an ordinary desktop matches a dozen windows. Picking
//! the first, the largest or the topmost would be a guess, and the user would
//! find out it was the wrong one after recording the wrong window for twenty
//! minutes. [`resolve`] therefore reports every candidate and refuses to
//! choose, which is acceptance criterion 2 of
//! [issue #10](https://github.com/wildware-uk/clipped/issues/10).
//!
//! There is exactly one narrowing rule, [`Self`](self)'s only piece of
//! cleverness, and it is documented on [`resolve`]: a title that matches one
//! window *exactly* wins over the windows it merely appears inside.

use core::fmt;
use std::error::Error;

use crate::window::{WindowHandle, WindowInfo};

/// How the user named the window they want.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TargetSelector {
    /// A window whose title contains this text, ignoring case.
    WindowTitle(String),
    /// A window belonging to a process running this executable, such as
    /// `cs2.exe`. Case is ignored, and the `.exe` suffix is optional.
    ProcessName(String),
    /// A window belonging to this process.
    ProcessId(u32),
    /// This exact window.
    WindowHandle(WindowHandle),
}

impl fmt::Display for TargetSelector {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WindowTitle(title) => write!(formatter, "the window title containing `{title}`"),
            Self::ProcessName(name) => write!(formatter, "the process `{name}`"),
            Self::ProcessId(process_id) => write!(formatter, "process {process_id}"),
            Self::WindowHandle(handle) => write!(formatter, "the window {handle}"),
        }
    }
}

/// Why a selector did not name exactly one capturable window.
///
/// Each variant carries what the user needs in order to try again, because
/// "target not found" is not something anybody can act on (AGENTS.md
/// section 15).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    /// Nothing matched at all.
    NoMatch {
        /// What was asked for.
        selector: TargetSelector,
    },

    /// More than one capturable window matched, and choosing between them would
    /// be a guess.
    Ambiguous {
        /// What was asked for.
        selector: TargetSelector,
        /// Every window that matched, in enumeration order.
        candidates: Vec<WindowInfo>,
    },

    /// Something matched, but nothing that matched can be captured.
    ///
    /// Distinct from [`Self::NoMatch`] because the two ask for different things
    /// from the user: a typo to fix, or a window to un-minimise, restore from
    /// another virtual desktop, or stop being told to hide from capture.
    NotCapturable {
        /// What was asked for.
        selector: TargetSelector,
        /// Every window that matched, each with its reason.
        candidates: Vec<WindowInfo>,
    },
}

impl ResolveError {
    /// What was asked for.
    #[must_use]
    pub const fn selector(&self) -> &TargetSelector {
        match self {
            Self::NoMatch { selector }
            | Self::Ambiguous { selector, .. }
            | Self::NotCapturable { selector, .. } => selector,
        }
    }

    /// The windows that matched, which is empty only for [`Self::NoMatch`].
    #[must_use]
    pub fn candidates(&self) -> &[WindowInfo] {
        match self {
            Self::NoMatch { .. } => &[],
            Self::Ambiguous { candidates, .. } | Self::NotCapturable { candidates, .. } => {
                candidates
            }
        }
    }
}

/// How many candidates a message lists before it summarises the rest.
///
/// A single process can own sixty top-level windows — `explorer.exe` routinely
/// does — and an error that prints all sixty is one nobody reads. Ten is enough
/// to recognise the window that was meant, and the full list is still on
/// [`ResolveError::candidates`] for anything that wants it.
const MAX_LISTED_CANDIDATES: usize = 10;

impl fmt::Display for ResolveError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoMatch { selector } => {
                write!(formatter, "no window matches {selector}")
            }
            Self::Ambiguous {
                selector,
                candidates,
            } => {
                write!(
                    formatter,
                    "{} windows match {selector}, and choosing between them would be a guess:",
                    candidates.len()
                )?;
                list_candidates(formatter, candidates, false)
            }
            Self::NotCapturable {
                selector,
                candidates,
            } => {
                let plural = if candidates.len() == 1 { "" } else { "s" };
                write!(
                    formatter,
                    "{} window{plural} match {selector}, and none of them can be captured:",
                    candidates.len()
                )?;
                list_candidates(formatter, candidates, true)
            }
        }
    }
}

impl Error for ResolveError {}

/// Writes one indented line per candidate, up to [`MAX_LISTED_CANDIDATES`].
fn list_candidates(
    formatter: &mut fmt::Formatter<'_>,
    candidates: &[WindowInfo],
    with_reason: bool,
) -> fmt::Result {
    for candidate in candidates.iter().take(MAX_LISTED_CANDIDATES) {
        write!(formatter, "\n  {}", summarise(candidate))?;
        if with_reason {
            if let Some(reason) = candidate.exclusion() {
                write!(formatter, " — {}", reason.explanation())?;
            }
        }
    }

    if let Some(remaining) = candidates.len().checked_sub(MAX_LISTED_CANDIDATES) {
        if remaining > 0 {
            write!(formatter, "\n  … and {remaining} more")?;
        }
    }

    Ok(())
}

/// One line naming a window well enough to tell it from its siblings.
fn summarise(window: &WindowInfo) -> String {
    let title = if window.title().is_empty() {
        "(untitled)"
    } else {
        window.title()
    };
    format!(
        "{}  {} (pid {})  {title}",
        window.handle(),
        window.process_name().unwrap_or("unknown"),
        window.process_id(),
    )
}

/// Finds the one window in `windows` that `selector` names.
///
/// `windows` is the whole enumeration, capturable or not: knowing that the
/// match exists but is cloaked is a better answer than "not found", and only
/// the full list can give it.
///
/// # Narrowing
///
/// A title selector matches by case-insensitive substring, so `--window
/// counter-strike` finds "Counter-Strike 2". When that matches several windows,
/// one rule is applied before giving up: **a window whose title is exactly the
/// text given, ignoring case, wins over windows that merely contain it.**
/// Without it, `--window Discord` is ambiguous on any machine that also has
/// "Discord Updater" open, and there is no longer string the user could type to
/// mean the shorter title. Two windows with the same exact title stay
/// ambiguous, as they must.
///
/// Nothing else is narrowed. In particular, a process that owns several windows
/// is ambiguous rather than resolved to its "main" window: Windows has no such
/// concept, and every heuristic for inventing one is wrong for some
/// application.
///
/// # Errors
///
/// [`ResolveError::NoMatch`] if nothing matched, [`ResolveError::NotCapturable`]
/// if everything that matched is excluded, and [`ResolveError::Ambiguous`] if
/// more than one capturable window matched.
pub fn resolve<'a>(
    windows: &'a [WindowInfo],
    selector: &TargetSelector,
) -> Result<&'a WindowInfo, ResolveError> {
    let (capturable, excluded): (Vec<&WindowInfo>, Vec<&WindowInfo>) = windows
        .iter()
        .filter(|window| matches(window, selector))
        .partition(|window| window.is_capturable());

    match capturable.as_slice() {
        [] if excluded.is_empty() => Err(ResolveError::NoMatch {
            selector: selector.clone(),
        }),
        [] => Err(ResolveError::NotCapturable {
            selector: selector.clone(),
            candidates: cloned(&excluded),
        }),
        [only] => Ok(only),
        several => {
            if let TargetSelector::WindowTitle(wanted) = selector {
                let exact: Vec<&WindowInfo> = several
                    .iter()
                    .copied()
                    .filter(|window| equal_ignoring_case(window.title(), wanted))
                    .collect();
                if let [only] = exact.as_slice() {
                    return Ok(only);
                }
            }

            Err(ResolveError::Ambiguous {
                selector: selector.clone(),
                candidates: cloned(several),
            })
        }
    }
}

fn cloned(windows: &[&WindowInfo]) -> Vec<WindowInfo> {
    windows.iter().map(|window| (*window).clone()).collect()
}

/// Whether one window answers to the selector.
fn matches(window: &WindowInfo, selector: &TargetSelector) -> bool {
    match selector {
        // An empty substring is inside every title, so it would match the whole
        // desktop. Nobody means that, and the command line rejects it before it
        // reaches here; matching nothing is the safe reading of it in the one
        // place that cannot report a usage error.
        TargetSelector::WindowTitle(title) => {
            !title.is_empty() && contains_ignoring_case(window.title(), title)
        }
        TargetSelector::ProcessName(name) => window
            .process_name()
            .is_some_and(|actual| process_name_matches(actual, name)),
        TargetSelector::ProcessId(process_id) => window.process_id() == *process_id,
        TargetSelector::WindowHandle(handle) => window.handle() == *handle,
    }
}

/// Whether an executable name answers to what the user typed.
///
/// The `.exe` is optional because half of the people typing this have just read
/// the process name out of Task Manager, which shows it, and half have typed
/// the game's name. Extensions other than `.exe` are not implied: `cs2.bat` is
/// a different file from `cs2.exe`, and guessing between them would be worse
/// than asking.
fn process_name_matches(actual: &str, wanted: &str) -> bool {
    if equal_ignoring_case(actual, wanted) {
        return true;
    }

    // The stem of `actual` compared with what was typed. Split by byte index
    // rather than by suffix search because `.exe` is four ASCII bytes and the
    // rest of the name may not be ASCII at all.
    actual.len().checked_sub(".exe".len()).is_some_and(|stem| {
        actual.is_char_boundary(stem)
            && equal_ignoring_case(&actual[stem..], ".exe")
            && equal_ignoring_case(&actual[..stem], wanted)
    })
}

/// Case-insensitive equality.
///
/// `char::to_lowercase` rather than `eq_ignore_ascii_case`, because window
/// titles and executable names are not ASCII: a game shipped in Turkish or
/// Greek has a title that ASCII case folding gets wrong, and this comparison is
/// the difference between finding the window and not.
fn equal_ignoring_case(left: &str, right: &str) -> bool {
    left.chars()
        .flat_map(char::to_lowercase)
        .eq(right.chars().flat_map(char::to_lowercase))
}

/// Case-insensitive substring search.
///
/// Allocates two lowercase strings per comparison, which for a hundred windows
/// and one selector is a few microseconds once per invocation — not a cost
/// worth a hand-written matcher (AGENTS.md section 18).
fn contains_ignoring_case(haystack: &str, needle: &str) -> bool {
    haystack.to_lowercase().contains(&needle.to_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::PixelSize;
    use crate::monitor::MonitorHandle;
    use crate::window::Exclusion;
    use crate::window::WindowGeometry;

    /// Builds a capturable window. The fields the tests do not care about are
    /// plausible rather than empty, so that a failure message reads like a real
    /// desktop.
    fn window(handle: isize, title: &str, process: &str, process_id: u32) -> WindowInfo {
        WindowInfo::new(
            WindowHandle::from_raw(handle),
            title.to_owned(),
            process_id,
            Some(process.to_owned()),
            WindowGeometry::new(
                PixelSize::new(1920, 1080),
                96,
                MonitorHandle::from_raw(0x0001),
            ),
            false,
            None,
        )
    }

    fn excluded(window: WindowInfo, exclusion: Exclusion) -> WindowInfo {
        WindowInfo::new(
            window.handle(),
            window.title().to_owned(),
            window.process_id(),
            window.process_name().map(ToOwned::to_owned),
            window.geometry(),
            window.is_minimised(),
            Some(exclusion),
        )
    }

    fn desktop() -> Vec<WindowInfo> {
        vec![
            window(0x0001, "Counter-Strike 2", "cs2.exe", 4242),
            window(0x0002, "clipped - Visual Studio Code", "Code.exe", 700),
            window(
                0x0003,
                "selection.rs - clipped - Visual Studio Code",
                "Code.exe",
                700,
            ),
            window(0x0004, "Discord", "Discord.exe", 900),
            window(0x0005, "Discord Updater", "Discord.exe", 900),
        ]
    }

    fn title(text: &str) -> TargetSelector {
        TargetSelector::WindowTitle(text.to_owned())
    }

    #[test]
    fn a_title_substring_finds_the_one_window_that_contains_it() {
        let windows = desktop();
        let resolved = resolve(&windows, &title("counter-strike")).expect("one window matches");
        assert_eq!(resolved.handle(), WindowHandle::from_raw(0x0001));
    }

    #[test]
    fn matching_is_case_insensitive_in_both_directions() {
        let windows = desktop();
        for text in ["COUNTER-STRIKE 2", "counter-strike 2", "CoUnTeR-sTrIkE"] {
            let resolved = resolve(&windows, &title(text)).expect("one window matches");
            assert_eq!(resolved.handle(), WindowHandle::from_raw(0x0001), "{text}");
        }
    }

    #[test]
    fn a_title_matching_two_windows_reports_both_rather_than_choosing() {
        let windows = desktop();
        let error = resolve(&windows, &title("visual studio code"))
            .expect_err("two windows match, and neither is more right than the other");

        let ResolveError::Ambiguous { candidates, .. } = &error else {
            panic!("expected an ambiguity, got {error}");
        };
        let handles: Vec<_> = candidates.iter().map(WindowInfo::handle).collect();
        assert_eq!(
            handles,
            vec![
                WindowHandle::from_raw(0x0002),
                WindowHandle::from_raw(0x0003)
            ],
            "the candidates must be reported in enumeration order"
        );

        // The message has to be actionable on its own: whoever reads it needs
        // the handles, because that is the only thing that tells the two
        // windows apart on a command line.
        let message = error.to_string();
        for expected in ["0x00000002", "0x00000003", "Code.exe", "selection.rs"] {
            assert!(
                message.contains(expected),
                "{expected} is missing: {message}"
            );
        }
    }

    #[test]
    fn an_exact_title_wins_over_the_windows_it_appears_inside() {
        // "Discord" is inside "Discord Updater", so without this rule there is
        // no string a user could type to mean the shorter title.
        let windows = desktop();
        let resolved = resolve(&windows, &title("Discord")).expect("the exact title wins");
        assert_eq!(resolved.handle(), WindowHandle::from_raw(0x0004));
        assert_eq!(resolved.title(), "Discord");
    }

    #[test]
    fn two_windows_with_the_same_exact_title_stay_ambiguous() {
        let windows = vec![
            window(0x0010, "Notepad", "notepad.exe", 10),
            window(0x0011, "Notepad", "notepad.exe", 11),
        ];
        let error = resolve(&windows, &title("notepad")).expect_err("neither can be preferred");
        assert!(matches!(error, ResolveError::Ambiguous { .. }), "{error}");
        assert_eq!(error.candidates().len(), 2);
    }

    #[test]
    fn a_title_that_matches_nothing_is_not_found() {
        let windows = desktop();
        let error = resolve(&windows, &title("minecraft")).expect_err("nothing matches");
        assert!(matches!(error, ResolveError::NoMatch { .. }), "{error}");
        assert!(error.candidates().is_empty());
        assert_eq!(
            error.to_string(),
            "no window matches the window title containing `minecraft`"
        );
    }

    #[test]
    fn an_empty_title_matches_nothing_rather_than_everything() {
        let windows = desktop();
        let error = resolve(&windows, &title("")).expect_err("an empty selector selects nothing");
        assert!(matches!(error, ResolveError::NoMatch { .. }), "{error}");
    }

    #[test]
    fn a_process_identifier_owning_one_window_resolves() {
        let windows = desktop();
        let resolved =
            resolve(&windows, &TargetSelector::ProcessId(4242)).expect("one window matches");
        assert_eq!(resolved.title(), "Counter-Strike 2");
    }

    #[test]
    fn a_process_identifier_owning_several_windows_is_ambiguous() {
        let windows = desktop();
        let error = resolve(&windows, &TargetSelector::ProcessId(700))
            .expect_err("a process has no main window as far as Windows is concerned");
        assert!(matches!(error, ResolveError::Ambiguous { .. }), "{error}");
        assert_eq!(error.candidates().len(), 2);
    }

    #[test]
    fn a_process_name_matches_with_or_without_the_extension_and_ignoring_case() {
        let windows = desktop();
        for name in ["cs2.exe", "cs2", "CS2.EXE", "Cs2"] {
            let resolved = resolve(&windows, &TargetSelector::ProcessName(name.to_owned()))
                .unwrap_or_else(|error| panic!("`{name}` should match cs2.exe: {error}"));
            assert_eq!(resolved.process_id(), 4242);
        }
    }

    #[test]
    fn a_process_name_is_not_matched_by_a_prefix_of_it() {
        // `cs` must not find `cs2.exe`: process names are exact, and a prefix
        // rule would make `steam` match `steamwebhelper.exe`, which is the one
        // window nobody wants to record.
        let windows = desktop();
        for name in ["cs", "cs2.ex", "s2.exe", "cs2.bat"] {
            let error = resolve(&windows, &TargetSelector::ProcessName(name.to_owned()))
                .expect_err("a process name is not a substring match");
            assert!(
                matches!(error, ResolveError::NoMatch { .. }),
                "`{name}`: {error}"
            );
        }
    }

    #[test]
    fn a_window_handle_resolves_to_exactly_that_window_and_never_more() {
        let windows = desktop();
        let resolved = resolve(
            &windows,
            &TargetSelector::WindowHandle(WindowHandle::from_raw(0x0003)),
        )
        .expect("a handle names one window");
        assert_eq!(
            resolved.title(),
            "selection.rs - clipped - Visual Studio Code"
        );

        let error = resolve(
            &windows,
            &TargetSelector::WindowHandle(WindowHandle::from_raw(0x0bad)),
        )
        .expect_err("that handle is not in this enumeration");
        assert!(matches!(error, ResolveError::NoMatch { .. }), "{error}");
    }

    #[test]
    fn an_excluded_window_is_never_resolved_to_but_is_reported_as_the_reason() {
        let mut windows = desktop();
        windows[0] = excluded(windows[0].clone(), Exclusion::Cloaked);

        let error = resolve(&windows, &title("counter-strike"))
            .expect_err("the only match cannot be captured");

        let ResolveError::NotCapturable { candidates, .. } = &error else {
            panic!("expected the exclusion to be reported, got {error}");
        };
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].exclusion(), Some(Exclusion::Cloaked));

        let message = error.to_string();
        assert!(
            message.contains("another virtual desktop"),
            "the message must say what to do about it: {message}"
        );
    }

    #[test]
    fn an_excluded_window_does_not_make_a_capturable_match_ambiguous() {
        // The common case on a real desktop: a game has a visible window and a
        // hidden splash or message window in the same process, and asking for
        // the process must not be reported as a choice between them.
        let mut windows = desktop();
        windows.push(excluded(
            window(0x0006, "Counter-Strike 2", "cs2.exe", 4242),
            Exclusion::Invisible,
        ));

        let resolved =
            resolve(&windows, &TargetSelector::ProcessId(4242)).expect("only one is capturable");
        assert_eq!(resolved.handle(), WindowHandle::from_raw(0x0001));
    }

    #[test]
    fn a_message_listing_every_candidate_would_not_be_read_so_it_summarises_the_tail() {
        // `explorer.exe` alone owns sixty-odd top-level windows on Windows 11,
        // and `--pid` matching all of them is the case this cap exists for.
        let windows: Vec<WindowInfo> = (0..25)
            .map(|ordinal| window(ordinal, "Untitled", "explorer.exe", 10_004))
            .collect();

        let error = resolve(&windows, &TargetSelector::ProcessId(10_004))
            .expect_err("twenty-five windows match");
        let message = error.to_string();

        assert_eq!(
            error.candidates().len(),
            25,
            "every candidate is still available to a caller"
        );
        assert_eq!(
            message.lines().count(),
            1 + MAX_LISTED_CANDIDATES + 1,
            "the summary should be one headline, ten candidates and one tail: {message}"
        );
        assert!(message.ends_with("… and 15 more"), "{message}");
    }

    #[test]
    fn an_untitled_candidate_is_named_as_such_rather_than_as_a_blank() {
        let windows = vec![
            excluded(window(0x0030, "", "explorer.exe", 1), Exclusion::Invisible),
            excluded(window(0x0031, "", "explorer.exe", 1), Exclusion::Invisible),
        ];
        let error =
            resolve(&windows, &TargetSelector::ProcessId(1)).expect_err("neither is usable");
        assert!(
            error.to_string().contains("(untitled)"),
            "a blank column reads as a formatting bug: {error}"
        );
    }

    #[test]
    fn resolving_against_an_empty_desktop_is_not_found_rather_than_a_panic() {
        let error = resolve(&[], &title("anything")).expect_err("there is nothing to match");
        assert!(matches!(error, ResolveError::NoMatch { .. }), "{error}");
    }

    #[test]
    fn a_selector_reads_back_the_way_the_command_line_expressed_it() {
        assert_eq!(
            title("cs2").to_string(),
            "the window title containing `cs2`"
        );
        assert_eq!(
            TargetSelector::ProcessName("cs2.exe".to_owned()).to_string(),
            "the process `cs2.exe`"
        );
        assert_eq!(TargetSelector::ProcessId(4242).to_string(), "process 4242");
        assert_eq!(
            TargetSelector::WindowHandle(WindowHandle::from_raw(0x104ac)).to_string(),
            "the window 0x000104ac"
        );
    }

    #[test]
    fn titles_outside_ascii_are_matched_by_their_own_case_rules() {
        // The Turkish dotless i is the standard demonstration that ASCII case
        // folding is not case folding: `İ` lowercases to `i̇`, and a game
        // shipped with a Turkish title is findable only if the comparison knows
        // that.
        let windows = vec![window(0x0020, "SAVAŞ", "game.exe", 1)];
        let resolved = resolve(&windows, &title("savaş")).expect("case folding is not ASCII-only");
        assert_eq!(resolved.handle(), WindowHandle::from_raw(0x0020));
    }
}

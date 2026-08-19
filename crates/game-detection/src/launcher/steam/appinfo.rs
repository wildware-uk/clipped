//! What Steam says each application *is*, read from `appcache/appinfo.vdf`.
//!
//! # Why this exists
//!
//! Since [issue #664](https://github.com/wildware-uk/clipped/issues/664) a
//! process a launcher claims is a game whether or not the catalogue names it.
//! That is right for the overwhelming majority and wrong for a specific,
//! enumerable minority: **Steam lists tools exactly as it lists games.**
//! Measured on a real library of 89 installed applications, the change
//! identified 17 that no catalogue entry named; thirteen were games and four
//! were `Source SDK Base 2006`, `SteamVR`, `Satisfactory Dedicated Server` and
//! `s&box editor` ([issue #671](https://github.com/wildware-uk/clipped/issues/671)).
//!
//! Nothing `appmanifest_<id>.acf` exposes separates them — a tool and a game
//! carry the same fields, and SteamVR is the larger of the two. `LastPlayed` is
//! a trap: it is non-zero for both, and a game installed but never launched has
//! `LastPlayed = 0`, so using it would refuse to record precisely the first
//! session.
//!
//! `appinfo.vdf` carries a type, and it separates them cleanly.
//!
//! # Failing soft is the design constraint
//!
//! This is an undocumented binary format that Valve versions and changes. A
//! parser that stopped detection when the format moved would turn a Steam
//! update into "Clipped records nothing", which is far worse than the problem
//! it fixes. So **every failure answers [`AppTypes::unknown`]**, which claims
//! nothing about any application and leaves detection exactly as it was before
//! this file existed. A refusal is only ever made on a type that was actually
//! read.
//!
//! # The format, as measured
//!
//! ```text
//! u32  magic          0x07564427 (v27), 0x07564428 (v28), 0x07564429 (v29)
//! u32  universe
//! i64  string table offset                                        -- v29 only
//! repeated, until an appid of 0:
//!   u32  appid
//!   u32  size          bytes following this field, for this entry
//!   u32  info state
//!   u32  last updated
//!   u64  PICS token
//!   [20] SHA-1 of the text vdf
//!   u32  change number
//!   [20] SHA-1 of the binary vdf                                   -- v28+
//!   binary key-values
//! at the string table offset:
//!   u32  count, then that many NUL-terminated UTF-8 strings
//! ```
//!
//! Binary key-values are a type byte, a key and a value. In v29 the key is a
//! `u32` index into the string table; before it, a NUL-terminated string. Only
//! v29 is read here, because it is what Steam ships and because a revision this
//! code has not seen is exactly the case that has to degrade rather than guess.
//!
//! The tree is walked, and only `appinfo.common.type` is kept.

use std::collections::HashMap;
use std::path::Path;

/// The magic of the only revision this parser reads.
const MAGIC_V29: u32 = 0x0756_4429;

/// Type byte for a nested object.
const KV_OBJECT: u8 = 0x00;
/// Type byte for a NUL-terminated string.
const KV_STRING: u8 = 0x01;
/// Type byte for a little-endian `i32`.
const KV_INT32: u8 = 0x02;
/// Type byte for a little-endian `u64`.
const KV_UINT64: u8 = 0x07;
/// Type byte closing the current object.
const KV_END: u8 = 0x08;

/// How deep the walk goes before giving up.
///
/// A malformed file can describe an object nesting into itself for as long as
/// the buffer lasts, and this walk is recursive. Steam's own tree is four deep;
/// sixteen is far past anything real and far short of a stack overflow.
const MAXIMUM_DEPTH: usize = 16;

/// What Steam calls an application, for the one question asked of it.
///
/// Deliberately not an enumeration of every value Valve uses. The question is
/// "would somebody want a recording of this", the answer for an unrecognised
/// value is *yes*, and a closed enumeration would need extending every time
/// Valve added one — with the failure landing as a game that silently stopped
/// recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AppKind {
    /// Something somebody plays: `Game`, `Demo`, `Beta`, or anything this
    /// parser does not recognise.
    Playable,
    /// Something nobody plays: `Tool`, `Config`, `DLC`, `Music`, `Video` or
    /// `Series`.
    NotPlayable,
}

impl AppKind {
    /// Reads Steam's own word for it.
    ///
    /// Compared without case, which is not tidiness: the file on the machine
    /// this was written against holds **326 applications typed `Game` and 100
    /// typed `game`**, and a case-sensitive comparison would have quietly
    /// refused a hundred of them.
    fn from_steam_type(value: &str) -> Self {
        match value.to_ascii_lowercase().as_str() {
            "tool" | "config" | "dlc" | "music" | "video" | "series" => Self::NotPlayable,
            _ => Self::Playable,
        }
    }
}

/// What Steam says about the applications on this machine.
///
/// Empty is a valid and safe state: it answers [`None`] for everything, which
/// leaves every claim as it was before this file existed.
#[derive(Debug, Clone, Default)]
pub(super) struct AppTypes {
    kinds: HashMap<u32, AppKind>,
}

impl AppTypes {
    /// Nothing is known, so nothing is refused.
    #[must_use]
    pub(super) fn unknown() -> Self {
        Self::default()
    }

    /// Whether anything at all was read.
    #[must_use]
    pub(super) fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// How many applications carry a type.
    #[must_use]
    pub(super) fn len(&self) -> usize {
        self.kinds.len()
    }

    /// What Steam calls the application with this identifier.
    ///
    /// [`None`] when the file could not be read, the identifier is not in it,
    /// or the entry carried no type — every one of which means "no opinion",
    /// never "not a game".
    #[must_use]
    pub(super) fn kind_of(&self, app_id: &str) -> Option<AppKind> {
        self.kinds.get(&app_id.parse::<u32>().ok()?).copied()
    }

    /// Reads `appcache/appinfo.vdf` under a Steam installation.
    ///
    /// Never fails, by construction: see the module documentation. A missing
    /// file, an unreadable one, a revision this parser does not know and a
    /// truncated one all answer [`AppTypes::unknown`].
    #[must_use]
    pub(super) fn read(steam_installation: &Path) -> Self {
        let path = steam_installation.join("appcache").join("appinfo.vdf");
        let Ok(bytes) = std::fs::read(path) else {
            tracing::debug!(
                "Steam's application catalogue could not be read, so no application type is \
                 known and every claim stands as it would have without it"
            );
            return Self::unknown();
        };
        Self::parse(&bytes)
    }

    /// The same, from bytes, so the walk is testable without a Steam
    /// installation.
    #[must_use]
    pub(super) fn parse(bytes: &[u8]) -> Self {
        match parse_v29(bytes) {
            Some(kinds) => {
                tracing::debug!(
                    applications = kinds.len(),
                    "read Steam's application catalogue"
                );
                Self { kinds }
            }
            None => {
                tracing::debug!(
                    "Steam's application catalogue is not a revision this build reads, so no \
                     application type is known and every claim stands as it would have without it"
                );
                Self::unknown()
            }
        }
    }
}

/// Reads a little-endian `u32`, or [`None`] past the end.
fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    let end = at.checked_add(4)?;
    let slice: [u8; 4] = bytes.get(at..end)?.try_into().ok()?;
    Some(u32::from_le_bytes(slice))
}

/// Reads a little-endian `i64`, or [`None`] past the end.
fn i64_at(bytes: &[u8], at: usize) -> Option<i64> {
    let end = at.checked_add(8)?;
    let slice: [u8; 8] = bytes.get(at..end)?.try_into().ok()?;
    Some(i64::from_le_bytes(slice))
}

/// The whole walk. [`None`] for any file this cannot read with confidence.
fn parse_v29(bytes: &[u8]) -> Option<HashMap<u32, AppKind>> {
    if u32_at(bytes, 0)? != MAGIC_V29 {
        return None;
    }

    let table_offset = usize::try_from(i64_at(bytes, 8)?).ok()?;
    let strings = string_table(bytes, table_offset)?;

    // The header is magic, universe and the table offset; entries follow. The
    // table bounds them: an entry beginning at or past it is a file that
    // disagrees with its own header, and is not guessed at.
    let mut kinds = HashMap::new();
    let mut at = 16;
    while at < table_offset {
        let app_id = u32_at(bytes, at)?;
        if app_id == 0 {
            break;
        }
        let size = usize::try_from(u32_at(bytes, at + 4)?).ok()?;

        // Info state, last updated, PICS token, SHA-1, change number, SHA-1.
        let values_at = at.checked_add(8 + 4 + 4 + 8 + 20 + 4 + 20)?;
        if let Some(kind) = kind_in(bytes, values_at, &strings) {
            kinds.insert(app_id, kind);
        }

        at = at.checked_add(8)?.checked_add(size)?;
    }

    // A file whose entries yielded nothing is one this parser did not
    // understand, whatever it made of the header. Answering "unknown" is the
    // safe reading; answering "every application is untyped" would be the same
    // thing dressed as a result.
    (!kinds.is_empty()).then_some(kinds)
}

/// The string table: a count, then that many NUL-terminated strings.
fn string_table(bytes: &[u8], at: usize) -> Option<Vec<&str>> {
    let count = usize::try_from(u32_at(bytes, at)?).ok()?;
    let mut strings = Vec::with_capacity(count.min(64 * 1024));
    let mut cursor = at.checked_add(4)?;
    for _ in 0..count {
        let rest = bytes.get(cursor..)?;
        let end = rest.iter().position(|byte| *byte == 0)?;
        strings.push(std::str::from_utf8(&rest[..end]).ok()?);
        cursor = cursor.checked_add(end)?.checked_add(1)?;
    }
    Some(strings)
}

/// `appinfo.common.type` for one entry, without keeping the rest of the tree.
fn kind_in(bytes: &[u8], at: usize, strings: &[&str]) -> Option<AppKind> {
    let mut found = None;
    walk(bytes, at, strings, 0, &mut |path, value| {
        if path == ["appinfo", "common", "type"] {
            found = Some(AppKind::from_steam_type(value));
        }
    })?;
    found
}

/// Walks one object, calling `visit` for every string value with its path.
///
/// Returns the position just past the object, or [`None`] the moment anything
/// does not read — a partly-walked entry contributes nothing rather than a
/// guess.
fn walk(
    bytes: &[u8],
    mut at: usize,
    strings: &[&str],
    depth: usize,
    visit: &mut dyn FnMut(&[&str], &str),
) -> Option<usize> {
    if depth > MAXIMUM_DEPTH {
        return None;
    }
    loop {
        let kind = *bytes.get(at)?;
        at += 1;
        if kind == KV_END {
            return Some(at);
        }

        let key = *strings.get(usize::try_from(u32_at(bytes, at)?).ok()?)?;
        at = at.checked_add(4)?;

        match kind {
            KV_OBJECT => {
                // A nested walk reports paths relative to itself, so this key
                // goes in front of whatever it reports.
                let mut nested = |inner: &[&str], value: &str| {
                    let mut full = Vec::with_capacity(inner.len() + 1);
                    full.push(key);
                    full.extend_from_slice(inner);
                    visit(&full, value);
                };
                at = walk(bytes, at, strings, depth + 1, &mut nested)?;
            }
            KV_STRING => {
                let rest = bytes.get(at..)?;
                let end = rest.iter().position(|byte| *byte == 0)?;
                if let Ok(value) = std::str::from_utf8(&rest[..end]) {
                    visit(&[key], value);
                }
                at = at.checked_add(end)?.checked_add(1)?;
            }
            KV_INT32 => at = at.checked_add(4)?,
            KV_UINT64 => at = at.checked_add(8)?,
            _ => return None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a v29 `appinfo.vdf` holding the applications given, so the walk is
    /// exercised without a Steam installation.
    ///
    /// A builder rather than a checked-in binary because the interesting cases
    /// are malformed ones, and a fixture that can be bent is what makes "does it
    /// fail soft" answerable at all.
    struct Builder {
        strings: Vec<String>,
        apps: Vec<(u32, Option<String>)>,
        magic: u32,
    }

    impl Builder {
        fn new() -> Self {
            Self {
                strings: ["appinfo", "common", "type", "name"]
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                apps: Vec::new(),
                magic: MAGIC_V29,
            }
        }

        fn app(mut self, app_id: u32, steam_type: Option<&str>) -> Self {
            self.apps.push((app_id, steam_type.map(str::to_owned)));
            self
        }

        fn magic(mut self, magic: u32) -> Self {
            self.magic = magic;
            self
        }

        fn index_of(&self, value: &str) -> u32 {
            u32::try_from(
                self.strings
                    .iter()
                    .position(|candidate| candidate == value)
                    .expect("the fixture only uses keys it declared"),
            )
            .expect("a fixture never has four billion strings")
        }

        fn build(&self) -> Vec<u8> {
            let mut entries: Vec<u8> = Vec::new();
            for (app_id, steam_type) in &self.apps {
                let mut values: Vec<u8> = Vec::new();
                values.push(KV_OBJECT);
                values.extend(self.index_of("appinfo").to_le_bytes());
                if let Some(steam_type) = steam_type {
                    values.push(KV_OBJECT);
                    values.extend(self.index_of("common").to_le_bytes());
                    values.push(KV_STRING);
                    values.extend(self.index_of("type").to_le_bytes());
                    values.extend(steam_type.as_bytes());
                    values.push(0);
                    values.push(KV_END);
                }
                values.push(KV_END); // closes `appinfo`
                values.push(KV_END); // closes the root object that holds it

                let mut body: Vec<u8> = Vec::new();
                body.extend(0_u32.to_le_bytes()); // info state
                body.extend(0_u32.to_le_bytes()); // last updated
                body.extend(0_u64.to_le_bytes()); // PICS token
                body.extend([0_u8; 20]); // SHA-1 of the text vdf
                body.extend(0_u32.to_le_bytes()); // change number
                body.extend([0_u8; 20]); // SHA-1 of the binary vdf
                body.extend(&values);

                entries.extend(app_id.to_le_bytes());
                entries.extend(
                    u32::try_from(body.len())
                        .expect("a fixture entry is small")
                        .to_le_bytes(),
                );
                entries.extend(&body);
            }
            entries.extend(0_u32.to_le_bytes()); // the terminating appid

            let mut table: Vec<u8> = Vec::new();
            table.extend(
                u32::try_from(self.strings.len())
                    .expect("a fixture has few strings")
                    .to_le_bytes(),
            );
            for value in &self.strings {
                table.extend(value.as_bytes());
                table.push(0);
            }

            let mut bytes: Vec<u8> = Vec::new();
            bytes.extend(self.magic.to_le_bytes());
            bytes.extend(1_u32.to_le_bytes()); // universe
            let offset = i64::try_from(16 + entries.len()).expect("a fixture is small");
            bytes.extend(offset.to_le_bytes());
            bytes.extend(&entries);
            bytes.extend(&table);
            bytes
        }
    }

    #[test]
    fn a_tool_and_a_game_are_told_apart() {
        let bytes = Builder::new()
            .app(427_520, Some("Game"))
            .app(250_820, Some("Tool"))
            .build();
        let types = AppTypes::parse(&bytes);

        assert_eq!(types.kind_of("427520"), Some(AppKind::Playable));
        assert_eq!(types.kind_of("250820"), Some(AppKind::NotPlayable));
    }

    #[test]
    fn the_type_is_read_without_case() {
        // Not tidiness. The file this was written against holds 326
        // applications typed `Game` and 100 typed `game`; a case-sensitive read
        // would have refused a hundred games.
        let bytes = Builder::new()
            .app(1, Some("game"))
            .app(2, Some("GAME"))
            .app(3, Some("tool"))
            .app(4, Some("TOOL"))
            .build();
        let types = AppTypes::parse(&bytes);

        assert_eq!(types.kind_of("1"), Some(AppKind::Playable));
        assert_eq!(types.kind_of("2"), Some(AppKind::Playable));
        assert_eq!(types.kind_of("3"), Some(AppKind::NotPlayable));
        assert_eq!(types.kind_of("4"), Some(AppKind::NotPlayable));
    }

    #[test]
    fn a_type_this_build_has_never_seen_is_playable() {
        // The open-world half of the decision. A value Valve adds later must
        // read as "somebody plays this": the cost of being wrong that way is a
        // recording nobody wanted, and the cost of being wrong the other way is
        // a game that silently stopped recording.
        let bytes = Builder::new()
            .app(1, Some("Game"))
            .app(2, Some("SomethingValveAddedLater"))
            .build();
        let types = AppTypes::parse(&bytes);

        assert_eq!(types.kind_of("2"), Some(AppKind::Playable));
    }

    #[test]
    fn a_demo_and_a_beta_are_things_somebody_plays() {
        // Eleven of the seventeen applications issue #671 measured were demos,
        // prologues and playtests. Refusing them would lose most of what issue
        // #664 gained.
        let bytes = Builder::new()
            .app(1, Some("Demo"))
            .app(2, Some("Beta"))
            .app(3, Some("DLC"))
            .app(4, Some("Music"))
            .build();
        let types = AppTypes::parse(&bytes);

        assert_eq!(types.kind_of("1"), Some(AppKind::Playable));
        assert_eq!(types.kind_of("2"), Some(AppKind::Playable));
        assert_eq!(types.kind_of("3"), Some(AppKind::NotPlayable));
        assert_eq!(types.kind_of("4"), Some(AppKind::NotPlayable));
    }

    #[test]
    fn an_application_with_no_type_is_no_opinion_rather_than_a_refusal() {
        let bytes = Builder::new().app(1, Some("Game")).app(2, None).build();
        let types = AppTypes::parse(&bytes);

        assert_eq!(types.kind_of("2"), None, "no type read is no opinion");
    }

    #[test]
    fn a_revision_this_build_does_not_know_claims_nothing() {
        // The whole safety property. A Steam update that moves the format has to
        // leave detection as it was, not stop it.
        let bytes = Builder::new()
            .magic(0x0756_4427)
            .app(427_520, Some("Game"))
            .build();
        let types = AppTypes::parse(&bytes);

        assert!(types.is_empty(), "an unknown revision is read as unknown");
        assert_eq!(types.kind_of("250820"), None);
    }

    #[test]
    fn a_truncated_file_claims_nothing() {
        let whole = Builder::new()
            .app(427_520, Some("Game"))
            .app(250_820, Some("Tool"))
            .build();

        for cut in [1, 8, 17, whole.len() / 2, whole.len() - 1] {
            let types = AppTypes::parse(&whole[..cut]);
            assert!(
                types.is_empty(),
                "a file cut at {cut} of {} bytes was read as though it said something",
                whole.len()
            );
        }
    }

    #[test]
    fn an_empty_file_and_a_file_of_rubbish_claim_nothing() {
        assert!(AppTypes::parse(&[]).is_empty());
        assert!(AppTypes::parse(&[0xff; 256]).is_empty());
        assert!(AppTypes::unknown().is_empty());
        assert_eq!(AppTypes::unknown().kind_of("427520"), None);
    }

    #[test]
    fn an_identifier_that_is_not_a_number_is_no_opinion() {
        // Every other launcher's identifiers would reach `kind_of` through this
        // same code if a caller were careless, and none of them are numbers.
        let bytes = Builder::new().app(427_520, Some("Game")).build();
        let types = AppTypes::parse(&bytes);

        assert_eq!(types.kind_of("league_of_legends"), None);
        assert_eq!(types.kind_of(""), None);
    }
}

//! The picture Steam has already downloaded for an application.
//!
//! # Where the icon actually is
//!
//! Steam keeps what it has downloaded under `appcache\librarycache`, a directory
//! per application. Most of the files in one are named for what they are —
//! `library_600x900.jpg`, `header.jpg`, `logo.png` — and the application icon is
//! not: it is named for the SHA-1 that `appinfo.vdf` records for it.
//!
//! ```text
//! appcache\librarycache\730\
//!     8dbc71957312bbd3baea65848b545be9eae2a355.jpg    32x32     the icon
//!     library_600x900.jpg                             300x450   the capsule
//!     library_hero.jpg                                1920x620
//!     logo.png
//! ```
//!
//! An earlier version of this module reported the capsule and said an icon could
//! not be had without reading `appinfo.vdf`. That was wrong, and the machine it
//! was written on says so: of its 660 cached applications, **none** has the
//! `<appid>_icon.jpg` file that version looked for first, and 511 have exactly
//! this hashed JPEG sitting in the application's own directory. The hash is only
//! needed to *name* the file; it is not needed to *find* it, because the
//! directory is already the application's.
//!
//! # Found by shape, because the name is a hash
//!
//! A hashed name cannot be recognised by spelling alone — the 3712x3712 artwork
//! in `1999270` is hashed too — so the file is recognised by what it is: a JPEG
//! whose frame header says [`ICON_SIZE`] square. Reading two numbers out of a
//! JPEG header is a documented format and about forty lines
//! ([`jpeg_dimensions`]); it is not the guess at an undocumented binary layout
//! that reading `appinfo.vdf` would be.
//!
//! Checked against the same 660 directories: taking the *smallest* hashed JPEG
//! instead would have reported a 2048x2048 image as an icon for four of them, so
//! the size check earns its place rather than merely tightening a rule that was
//! already right.
//!
//! # What is reported when there is no icon
//!
//! The artwork, in the order a caller would want it: the portrait capsule, the
//! header, the logo. That is not an icon and the caller can see it is not, from
//! the file name; it is better than nothing for an application whose icon Steam
//! has not cached. `None` when Steam has cached nothing at all, which is
//! ordinary for an application installed but never shown in the library.
//!
//! Nothing here fetches anything, and nothing here decodes an image. Every
//! answer is a path to a file Steam downloaded.

use std::fs;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

/// The width and height, in pixels, of Steam's application icon.
const ICON_SIZE: u16 = 32;

/// The length of the SHA-1 in a cached icon's file name, in hex characters.
const HASH_LENGTH: usize = 40;

/// The artwork files with names that say what they are, best first.
///
/// Reported only when no icon is cached. The capsule before the header because
/// it is the picture Steam itself shows for a game, and the logo last because it
/// is usually the title alone on transparency.
const ARTWORK: [&str; 3] = ["library_600x900.jpg", "header.jpg", "logo.png"];

/// How many JPEG segments are walked before a file is given up on.
///
/// A frame header appears within the first few segments of every real JPEG. The
/// cap is what stops a malformed or hostile file from holding the reader in a
/// loop; these are files downloaded from the internet by another program.
const MAX_SEGMENTS: usize = 64;

/// The best picture Steam has cached for an application, if it has cached one.
pub(super) fn icon(root: &Path, app_id: &str) -> Option<PathBuf> {
    let cache = root.join("appcache").join("librarycache");
    let directory = cache.join(app_id);

    if let Some(icon) = cached_icon(&directory) {
        return Some(icon);
    }

    // The layout Steam used before it moved to a directory per application. No
    // machine this was checked against still has one, so it is a fallback for an
    // older client rather than something observed; it costs one `stat` and it is
    // an icon rather than artwork, which is why it is tried before the capsule.
    let legacy = cache.join(format!("{app_id}_icon.jpg"));
    if legacy.is_file() {
        return Some(legacy);
    }

    ARTWORK
        .into_iter()
        .map(|name| directory.join(name))
        .find(|candidate| candidate.is_file())
}

/// The application icon in an application's own cache directory.
///
/// Every candidate is a JPEG named for a SHA-1; the one that is [`ICON_SIZE`]
/// square is the icon. Candidates are sorted so that a directory holding two of
/// them answers the same way twice — `fs::read_dir` promises no order.
fn cached_icon(directory: &Path) -> Option<PathBuf> {
    let entries = fs::read_dir(directory).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| is_hashed_jpeg(path))
        .collect();
    candidates.sort();

    candidates
        .into_iter()
        .find(|candidate| jpeg_dimensions(candidate) == Some((ICON_SIZE, ICON_SIZE)))
}

/// Whether a file is named for a SHA-1 and is a `.jpg`.
fn is_hashed_jpeg(path: &Path) -> bool {
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    let Some(hash) = name.strip_suffix(".jpg") else {
        return false;
    };
    hash.len() == HASH_LENGTH && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// The pixel dimensions a JPEG's frame header declares, without decoding it.
///
/// A JPEG is a sequence of segments, each introduced by `0xFF` and a marker
/// byte. The one that carries the dimensions is a start-of-frame — any of
/// `0xC0`–`0xCF` except the three in that range that mean something else — and
/// it holds one byte of sample precision followed by the height and the width,
/// each two bytes, most significant first. Every other segment is skipped by the
/// length it declares.
///
/// `None` for anything that is not a JPEG, is truncated, or reaches its
/// compressed data without ever declaring a frame. Nothing here is fatal: an
/// unreadable file simply is not the icon.
fn jpeg_dimensions(path: &Path) -> Option<(u16, u16)> {
    let mut reader = BufReader::new(fs::File::open(path).ok()?);
    if read_bytes::<2>(&mut reader)? != [0xFF, 0xD8] {
        // Not a JPEG: no start-of-image.
        return None;
    }

    for _ in 0..MAX_SEGMENTS {
        // A marker may be preceded by any number of `0xFF` fill bytes.
        let mut marker = read_bytes::<1>(&mut reader)?[0];
        if marker != 0xFF {
            return None;
        }
        while marker == 0xFF {
            marker = read_bytes::<1>(&mut reader)?[0];
        }

        match marker {
            // Start of frame: baseline, extended, progressive, lossless and
            // their arithmetic-coded and hierarchical variants. `0xC4`, `0xC8`
            // and `0xCC` share the range and are not frames.
            0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF => {
                // Two bytes of segment length, then the precision, which are
                // together the five bytes before the dimensions.
                let header = read_bytes::<7>(&mut reader)?;
                let height = u16::from_be_bytes([header[3], header[4]]);
                let width = u16::from_be_bytes([header[5], header[6]]);
                return Some((width, height));
            }
            // Start of scan, end of image: the dimensions were never declared.
            0xDA | 0xD9 => return None,
            // Standalone markers, which carry no payload to skip.
            0x01 | 0xD0..=0xD8 => {}
            _ => {
                let length = u16::from_be_bytes(read_bytes::<2>(&mut reader)?);
                // The length counts itself, so a segment claiming fewer than two
                // bytes is malformed and would leave the reader going backwards.
                let payload = length.checked_sub(2)?;
                reader.seek(SeekFrom::Current(i64::from(payload))).ok()?;
            }
        }
    }
    None
}

/// Exactly `N` bytes, or `None` if the file ends first or refuses to be read.
fn read_bytes<const N: usize>(reader: &mut impl Read) -> Option<[u8; N]> {
    let mut buffer = [0_u8; N];
    reader.read_exact(&mut buffer).ok().map(|()| buffer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hashed_jpeg_is_recognised_and_a_named_one_is_not() {
        let hash = "8dbc71957312bbd3baea65848b545be9eae2a355";
        assert!(is_hashed_jpeg(Path::new(&format!(
            "C:/cache/730/{hash}.jpg"
        ))));
        assert!(!is_hashed_jpeg(Path::new("C:/cache/730/header.jpg")));
        assert!(
            !is_hashed_jpeg(Path::new(&format!("C:/cache/730/{hash}.png"))),
            "the icon Steam caches is a JPEG"
        );
        assert!(
            !is_hashed_jpeg(Path::new(&format!("C:/cache/730/{}.jpg", &hash[1..]))),
            "a SHA-1 is forty characters"
        );
        assert!(
            !is_hashed_jpeg(Path::new(&format!("C:/cache/730/{}z.jpg", &hash[1..]))),
            "and all of them are hex"
        );
    }

    #[test]
    fn a_frame_header_gives_up_its_dimensions() {
        let directory = crate::launcher::steam::tests::scratch("jpeg-dimensions");
        let path = directory.join("icon.jpg");
        fs::write(&path, jpeg(0xC0, 32, 32)).expect("the file can be written");
        assert_eq!(jpeg_dimensions(&path), Some((32, 32)));

        let progressive = directory.join("progressive.jpg");
        fs::write(&progressive, jpeg(0xC2, 300, 450)).expect("the file can be written");
        assert_eq!(
            jpeg_dimensions(&progressive),
            Some((300, 450)),
            "a progressive frame declares its size in the same place"
        );
    }

    #[test]
    fn a_file_that_is_not_a_jpeg_has_no_dimensions() {
        let directory = crate::launcher::steam::tests::scratch("jpeg-rubbish");

        let empty = directory.join("empty.jpg");
        fs::write(&empty, b"").expect("the file can be written");
        assert_eq!(jpeg_dimensions(&empty), None);

        let png = directory.join("logo.png");
        fs::write(&png, b"\x89PNG\r\n\x1a\n").expect("the file can be written");
        assert_eq!(jpeg_dimensions(&png), None);

        let truncated = directory.join("truncated.jpg");
        let mut bytes = jpeg(0xC0, 32, 32);
        bytes.truncate(6);
        fs::write(&truncated, bytes).expect("the file can be written");
        assert_eq!(jpeg_dimensions(&truncated), None);

        let missing = directory.join("not-here.jpg");
        assert_eq!(jpeg_dimensions(&missing), None);

        // A file that does not open with the JPEG marker is not a JPEG, whatever
        // it goes on to contain. Without that check, two bytes of anything
        // followed by a frame-shaped sequence would be read as an image.
        let impostor = directory.join("impostor.jpg");
        let mut bytes = vec![0x00, 0x00];
        bytes.extend(frame(0xC0, 32, 32));
        fs::write(&impostor, bytes).expect("the file can be written");
        assert_eq!(jpeg_dimensions(&impostor), None);
    }

    #[test]
    fn a_frame_behind_a_metadata_segment_is_still_found() {
        // Every JPEG Steam caches carries at least a JFIF or an Exif segment
        // before its frame, so skipping segments by their declared length is not
        // an edge case: it is the ordinary path.
        let directory = crate::launcher::steam::tests::scratch("jpeg-segments");
        let path = directory.join("exif.jpg");

        let mut bytes = vec![0xFF, 0xD8];
        bytes.extend(exif(4096));
        bytes.extend(frame(0xC0, 32, 32));
        fs::write(&path, bytes).expect("the file can be written");

        assert_eq!(jpeg_dimensions(&path), Some((32, 32)));
    }

    #[test]
    fn a_file_of_nothing_but_segment_headers_is_given_up_on() {
        // A hostile or corrupt file must end as `None` rather than hold the
        // reader in a loop; these are files another program downloaded.
        let directory = crate::launcher::steam::tests::scratch("jpeg-hostile");
        let path = directory.join("hostile.jpg");

        let mut bytes = vec![0xFF, 0xD8];
        for _ in 0..(MAX_SEGMENTS * 4) {
            bytes.extend(exif(0));
        }
        bytes.extend(frame(0xC0, 32, 32));
        fs::write(&path, bytes).expect("the file can be written");

        assert_eq!(
            jpeg_dimensions(&path),
            None,
            "the frame past the cap is not reached"
        );
    }

    #[test]
    fn a_segment_that_claims_an_impossible_length_is_refused() {
        // A segment's length counts its own two bytes, so anything below two is
        // impossible and the file is malformed. Treating it as "skip nothing"
        // instead would carry on reading from the middle of a segment — and the
        // frame this file puts there proves the difference, because a reader
        // that carried on would answer 32x32 about a file that is not one.
        let directory = crate::launcher::steam::tests::scratch("jpeg-length");
        let path = directory.join("short.jpg");

        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x00];
        bytes.extend(frame(0xC0, 32, 32));
        fs::write(&path, bytes).expect("the file can be written");

        assert_eq!(jpeg_dimensions(&path), None);
    }

    #[test]
    fn the_icon_is_the_square_one_and_not_merely_the_first_hashed_file() {
        // The case that made the size check necessary rather than decorative:
        // four of the 660 cached applications on the machine this was written
        // against hold a large hashed JPEG and no icon at all.
        let directory = crate::launcher::steam::tests::scratch("icon-among-artwork");
        let cache = directory.join("appcache").join("librarycache").join("730");
        fs::create_dir_all(&cache).expect("the cache can be created");

        let large = cache.join(format!("{}.jpg", "0".repeat(HASH_LENGTH)));
        fs::write(&large, jpeg(0xC0, 2048, 2048)).expect("the file can be written");
        let small = cache.join(format!("{}.jpg", "f".repeat(HASH_LENGTH)));
        fs::write(&small, jpeg(0xC0, 32, 32)).expect("the file can be written");

        assert_eq!(icon(&directory, "730"), Some(small));
    }

    /// A JPEG stream: start of image, then a frame declaring `width` by
    /// `height`. Header-valid and deliberately not decodable — nothing here
    /// decodes an image, and a real icon would be somebody's copyrighted
    /// artwork.
    fn jpeg(marker: u8, width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8];
        bytes.extend(frame(marker, width, height));
        bytes
    }

    /// A start-of-frame segment.
    fn frame(marker: u8, width: u16, height: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, marker, 0x00, 0x0B, 0x08];
        bytes.extend(height.to_be_bytes());
        bytes.extend(width.to_be_bytes());
        // One component, which is what makes the declared length of 11 right.
        bytes.extend([0x01, 0x01, 0x11, 0x00]);
        bytes
    }

    /// An `APP1` segment of `payload` bytes, standing in for the Exif or JFIF
    /// block that precedes the frame in a real file.
    fn exif(payload: u16) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xE1];
        bytes.extend((payload + 2).to_be_bytes());
        bytes.resize(bytes.len() + usize::from(payload), 0);
        bytes
    }
}

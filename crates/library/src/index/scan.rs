//! Walking the directories a user keeps recordings in.
//!
//! The walk is the cheap half of indexing and the half most likely to meet
//! something odd: a drive that is not plugged in, a folder Windows will not let
//! the recorder read, a junction pointing at its own parent, a network share
//! that answers slowly. None of those may stop the rest of the library being
//! indexed, and none of them may be read as "the files are gone" — that is what
//! [`UnavailableRoot`] is for.
//!
//! # What it is bounded by
//!
//! - **Depth**, so a directory tree that refers to itself cannot spin. Symbolic
//!   links and Windows junctions are not followed at all, which is the other
//!   half of the same guarantee.
//! - **Cancellation**, checked on every entry, so a user closing the
//!   application does not wait for a walk of a slow drive to finish.
//!
//! It is deliberately not bounded by a file count or a time budget. A walk that
//! stopped early would leave the caller unable to tell "these recordings are
//! gone" from "I did not look", and acting on that difference is the whole of
//! [`super::presence`].

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use tracing::debug;

use super::sidecar;
use super::{normalise, IndexControl};

/// The media file extensions a walk recognises, lower case.
///
/// Matroska is what Clipped records into
/// ([ADR 0001](https://github.com/wildware-uk/clipped/blob/main/docs/adr/0001-mkv-archival-container.md)),
/// and MP4 is here because an exported clip is expected to be one and because a
/// user's own footage sitting in the same folder is worth reporting rather than
/// stepping over.
const MEDIA_EXTENSIONS: &[&str] = &["mkv", "mp4"];

/// Whether a root could be looked at, and if not, why not.
///
/// The distinction matters more than anything else in this module. A root that
/// could not be read is not evidence about the files under it, so nothing under
/// it may be marked missing (AGENTS.md section 56): the usual cause is an
/// external drive that is not plugged in, and the user is about to plug it back
/// in.
#[derive(Debug)]
pub struct UnavailableRoot {
    /// The directory that could not be walked.
    pub path: PathBuf,
    /// What the filesystem said.
    pub error: io::Error,
}

/// What one walk found.
#[derive(Debug, Default)]
pub(crate) struct Walk {
    /// Every session sidecar found, in the order the walk met them.
    pub(crate) sidecars: Vec<PathBuf>,
    /// Every media file found.
    pub(crate) media: Vec<PathBuf>,
    /// The roots that could be read, normalised for comparison.
    pub(crate) available_roots: Vec<String>,
    /// The roots that could not.
    pub(crate) unavailable_roots: Vec<UnavailableRoot>,
    /// Directories inside an available root that could not be listed.
    pub(crate) unreadable_directories: Vec<UnavailableRoot>,
    /// Whether the walk stopped because it was cancelled.
    pub(crate) cancelled: bool,
}

impl Walk {
    /// Whether `path` lies under a root this walk was able to read.
    ///
    /// This is the question "did I look here?", and it is asked before any row
    /// is marked missing. Comparison is case-insensitive because Windows file
    /// names are (SPEC.md section 3): a recording written to `D:\Clips` and a
    /// root configured as `d:\clips` are the same place.
    pub(crate) fn covers(&self, path: &Path) -> bool {
        let candidate = normalise(path);
        self.available_roots
            .iter()
            .any(|root| candidate.starts_with(root.as_str()))
    }
}

/// Walks `roots`, finding session sidecars and media files.
///
/// `max_depth` is how many directories deep the walk may go below a root; zero
/// means the root itself only.
pub(crate) fn walk(roots: &[PathBuf], max_depth: usize, control: &IndexControl) -> Walk {
    let mut found = Walk::default();
    // A user can configure two roots where one contains the other, and a file
    // counted twice would be reported as unindexed media that is in fact
    // indexed. Directories are visited once.
    let mut visited: HashSet<String> = HashSet::new();

    for root in roots {
        if control.is_cancelled() {
            found.cancelled = true;
            return found;
        }

        match fs::metadata(root) {
            Ok(metadata) if metadata.is_dir() => {}
            Ok(_) => {
                found.unavailable_roots.push(UnavailableRoot {
                    path: root.clone(),
                    error: io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "the recording folder is a file, not a directory",
                    ),
                });
                continue;
            }
            Err(error) => {
                // The disconnected drive. Reported, and nothing under it is
                // judged.
                debug!(
                    root = %root.display(),
                    %error,
                    "a recording folder could not be reached, so nothing under it is reconciled"
                );
                found.unavailable_roots.push(UnavailableRoot {
                    path: root.clone(),
                    error,
                });
                continue;
            }
        }

        let mut normalised_root = normalise(root);
        if !normalised_root.ends_with(std::path::MAIN_SEPARATOR) {
            normalised_root.push(std::path::MAIN_SEPARATOR);
        }
        found.available_roots.push(normalised_root);

        let mut stack = vec![(root.clone(), 0usize)];
        while let Some((directory, depth)) = stack.pop() {
            if control.is_cancelled() {
                found.cancelled = true;
                return found;
            }
            if !visited.insert(normalise(&directory)) {
                continue;
            }

            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) => {
                    found.unreadable_directories.push(UnavailableRoot {
                        path: directory,
                        error,
                    });
                    continue;
                }
            };

            for entry in entries {
                if control.is_cancelled() {
                    found.cancelled = true;
                    return found;
                }
                let Ok(entry) = entry else {
                    continue;
                };
                let path = entry.path();
                // `file_type` from a directory entry does not follow links,
                // which is what keeps a junction pointing at its own parent
                // from being descended into.
                let Ok(file_type) = entry.file_type() else {
                    continue;
                };

                if file_type.is_symlink() {
                    debug!(
                        path = %path.display(),
                        "a link inside a recording folder was not followed"
                    );
                    continue;
                }
                if file_type.is_dir() {
                    if depth < max_depth {
                        stack.push((path, depth + 1));
                    }
                    continue;
                }
                if sidecar::is_sidecar(&path) {
                    found.sidecars.push(path);
                } else if is_media(&path) {
                    found.media.push(path);
                }
            }
        }
    }

    found
}

/// Whether `path` names a file this walk counts as media.
fn is_media(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            MEDIA_EXTENSIONS
                .iter()
                .any(|known| extension.eq_ignore_ascii_case(known))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::test_support::scratch_directory;

    #[test]
    fn a_walk_finds_sidecars_and_media_and_ignores_everything_else() {
        let directory = scratch_directory("walk-finds");
        fs::write(directory.join("clipped-a.session.json"), "{}").expect("a sidecar");
        fs::write(directory.join("clipped-a.mkv"), "").expect("a recording");
        fs::write(directory.join("exported.MP4"), "").expect("a clip");
        fs::write(directory.join("clipped-a.session.json.tmp"), "{}").expect("a temporary file");
        fs::write(directory.join("notes.txt"), "").expect("something else");

        let found = walk(
            std::slice::from_ref(&directory),
            8,
            &IndexControl::default(),
        );

        assert_eq!(found.sidecars.len(), 1, "{:?}", found.sidecars);
        assert_eq!(found.media.len(), 2, "{:?}", found.media);
        assert!(found.unavailable_roots.is_empty());
    }

    #[test]
    fn a_root_that_is_not_there_is_reported_and_not_treated_as_empty() {
        // The external drive that is not plugged in. Everything downstream of
        // this depends on the difference between "no files" and "no answer".
        let directory = scratch_directory("walk-absent");
        let absent = directory.join("not-plugged-in");

        let found = walk(std::slice::from_ref(&absent), 8, &IndexControl::default());

        assert_eq!(found.unavailable_roots.len(), 1);
        assert_eq!(found.unavailable_roots[0].path, absent);
        assert!(found.available_roots.is_empty());
        assert!(
            !found.covers(&absent.join("clipped-a.mkv")),
            "a root that could not be read must not be reported as looked at"
        );
    }

    #[test]
    fn a_walk_stops_when_it_is_cancelled() {
        let directory = scratch_directory("walk-cancelled");
        fs::write(directory.join("clipped-a.session.json"), "{}").expect("a sidecar");
        let control = IndexControl::default();
        control.cancel();

        let found = walk(&[directory], 8, &control);

        assert!(found.cancelled);
        assert!(found.sidecars.is_empty());
    }

    #[test]
    fn a_walk_does_not_descend_past_its_depth_limit() {
        let directory = scratch_directory("walk-depth");
        let deep = directory.join("one").join("two").join("three");
        fs::create_dir_all(&deep).expect("the tree can be created");
        fs::write(deep.join("clipped-deep.session.json"), "{}").expect("a sidecar");
        fs::write(
            directory.join("one").join("clipped-shallow.session.json"),
            "{}",
        )
        .expect("a sidecar");

        let shallow = walk(
            std::slice::from_ref(&directory),
            1,
            &IndexControl::default(),
        );
        let full = walk(&[directory], 8, &IndexControl::default());

        assert_eq!(shallow.sidecars.len(), 1, "{:?}", shallow.sidecars);
        assert_eq!(full.sidecars.len(), 2, "{:?}", full.sidecars);
    }

    #[test]
    fn a_directory_inside_two_roots_is_walked_once() {
        let directory = scratch_directory("walk-overlap");
        let inner = directory.join("inner");
        fs::create_dir_all(&inner).expect("the tree can be created");
        fs::write(inner.join("clipped-a.mkv"), "").expect("a recording");

        let found = walk(&[directory, inner], 8, &IndexControl::default());

        assert_eq!(found.media.len(), 1, "{:?}", found.media);
    }

    #[test]
    fn a_root_is_covered_whatever_case_it_is_written_in() {
        let directory = scratch_directory("walk-case");
        let found = walk(
            std::slice::from_ref(&directory),
            8,
            &IndexControl::default(),
        );

        let shouted = PathBuf::from(directory.to_string_lossy().to_uppercase()).join("a.mkv");

        assert!(
            found.covers(&shouted),
            "Windows file names are case-insensitive: {shouted:?} against {:?}",
            found.available_roots
        );
    }
}

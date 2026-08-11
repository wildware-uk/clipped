//! What kinds of file a library is made of.
//!
//! The category exists because a total is not enough on its own. A user looking
//! at "212 GB" wants to know what it is made of before they agree to lose any of
//! it, the settings screen shows a breakdown
//! ([issue #95](https://github.com/wildware-uk/clipped/issues/95)), and a cleanup
//! has to treat a thumbnail — which can be regenerated in a second — differently
//! from the recording it was made from, which cannot be regenerated at all.
//!
//! Every category listed here is counted towards usage. That is the point of the
//! list: a quota that omits the replay buffer's disk backing or the trash reports
//! a figure smaller than the disk shows, and a user whose disk fills anyway
//! stops believing any of it.

use core::fmt;

/// A kind of file the library occupies disk with.
///
/// Each variant corresponds to one [`StorageRoot`](crate::accounting::StorageRoot):
/// the caller says where each kind lives, because the on-disk layout belongs to
/// `clipped-storage` and not here.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[non_exhaustive]
pub enum StorageCategory {
    /// Captured sessions. The irreplaceable part of a library, and the largest.
    Recordings,
    /// Clips cut from recordings. Irreplaceable in their own right: a clip's
    /// source may itself have been deleted (SPEC.md section 19).
    Clips,
    /// Screenshots (SPEC.md section 26).
    Screenshots,
    /// Thumbnail images generated from recordings and clips. Regenerable.
    Thumbnails,
    /// Audio waveform data generated for the timeline. Regenerable.
    Waveforms,
    /// The replay buffer's disk backing
    /// ([issue #36](https://github.com/wildware-uk/clipped/issues/36)).
    ///
    /// Transient and owned by a running recorder: it holds video that has been
    /// captured but not yet saved to anything. It is counted because it occupies
    /// the disk, and it is never a cleanup candidate because deleting it damages
    /// a recording in progress.
    ReplayBuffer,
    /// Deleted media awaiting its retention period
    /// ([issue #94](https://github.com/wildware-uk/clipped/issues/94)).
    ///
    /// Counted, and counted deliberately: footage in the trash still occupies
    /// the disk, and a user who deletes 40 GB and sees no change in free space
    /// is owed an explanation rather than a surprise.
    Trash,
    /// Diagnostic logs (`docs/logging.md`).
    ///
    /// Small and already bounded by rotation, so this is never a cleanup
    /// candidate; it is counted so that the reported total agrees with the disk.
    Logs,
    /// The database and any sidecar metadata files.
    ///
    /// Never a cleanup candidate: it is what makes the media meaningful, and it
    /// is small.
    Metadata,
}

impl StorageCategory {
    /// Every category, in the order a breakdown reads best.
    pub const ALL: &'static [Self] = &[
        Self::Recordings,
        Self::Clips,
        Self::Screenshots,
        Self::Thumbnails,
        Self::Waveforms,
        Self::ReplayBuffer,
        Self::Trash,
        Self::Logs,
        Self::Metadata,
    ];

    /// Whether files of this kind are the user's own media.
    ///
    /// The distinction that matters for AGENTS.md section 56: losing one of
    /// these loses something a user cannot get back. It is a statement about the
    /// file, not a deletion rule — the rules are issue #111's.
    #[must_use]
    pub const fn holds_user_media(self) -> bool {
        matches!(
            self,
            Self::Recordings | Self::Clips | Self::Screenshots | Self::Trash | Self::ReplayBuffer
        )
    }

    /// Whether files of this kind can be produced again from what remains.
    ///
    /// A thumbnail deleted is a thumbnail regenerated on next view; a recording
    /// deleted is gone.
    #[must_use]
    pub const fn is_regenerable(self) -> bool {
        matches!(self, Self::Thumbnails | Self::Waveforms)
    }

    /// A short lower-case name, for logs and diagnostics.
    ///
    /// Not user-facing copy: the words a screen shows are the interface's
    /// (AGENTS.md section 28), and a crate under `crates/` has no business
    /// deciding them.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Recordings => "recordings",
            Self::Clips => "clips",
            Self::Screenshots => "screenshots",
            Self::Thumbnails => "thumbnails",
            Self::Waveforms => "waveforms",
            Self::ReplayBuffer => "replay-buffer",
            Self::Trash => "trash",
            Self::Logs => "logs",
            Self::Metadata => "metadata",
        }
    }
}

impl fmt::Display for StorageCategory {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_category_is_listed_in_all() {
        // `ALL` is what a breakdown iterates, so a category added to the
        // enumeration and forgotten here would be a category no screen ever
        // shows. Matching exhaustively is what makes the compiler enforce that:
        // adding a variant fails this match until it is listed.
        for category in StorageCategory::ALL {
            match category {
                StorageCategory::Recordings
                | StorageCategory::Clips
                | StorageCategory::Screenshots
                | StorageCategory::Thumbnails
                | StorageCategory::Waveforms
                | StorageCategory::ReplayBuffer
                | StorageCategory::Trash
                | StorageCategory::Logs
                | StorageCategory::Metadata => {}
            }
        }

        assert_eq!(StorageCategory::ALL.len(), 9);
    }

    #[test]
    fn the_categories_holding_user_media_are_exactly_the_irreplaceable_ones() {
        // The list AGENTS.md section 56 is about. Spelled out rather than
        // derived, so that moving a category into or out of it is a visible
        // change to a test rather than a quiet change to a `matches!`.
        let media: Vec<_> = StorageCategory::ALL
            .iter()
            .copied()
            .filter(|category| category.holds_user_media())
            .collect();

        assert_eq!(
            media,
            vec![
                StorageCategory::Recordings,
                StorageCategory::Clips,
                StorageCategory::Screenshots,
                StorageCategory::ReplayBuffer,
                StorageCategory::Trash,
            ]
        );
    }

    #[test]
    fn nothing_is_both_user_media_and_regenerable() {
        for category in StorageCategory::ALL {
            assert!(
                !(category.holds_user_media() && category.is_regenerable()),
                "{category} cannot be both"
            );
        }
    }

    #[test]
    fn each_category_has_its_own_name() {
        let mut names: Vec<_> = StorageCategory::ALL
            .iter()
            .map(|category| category.as_str())
            .collect();
        names.sort_unstable();
        names.dedup();

        assert_eq!(names.len(), StorageCategory::ALL.len());
    }
}

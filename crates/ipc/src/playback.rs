//! Opening a recording so that the desktop window can play it.
//!
//! # Why the recorder is asked at all
//!
//! Two things the window cannot do for itself, and one it should not.
//!
//! - **It cannot read the file.** The Tauri host has no file-system permission
//!   granted to the interface, and
//!   `tests/integration/tests/workspace_layering.rs` permits
//!   `apps/desktop/src-tauri` exactly one crate of the workspace, `clipped-ipc`
//!   — so the window links no demuxer and can no more list a recording's audio
//!   tracks than it can decode one.
//! - **A `<video>` cannot choose an audio track.**
//!   `HTMLMediaElement.audioTracks` is not implemented in Chromium, which
//!   WebView2 is, so a multi-track file handed to a media element plays
//!   whichever track its demuxer reaches first and offers no way off it.
//!   Choosing one therefore means being handed a file that holds one, and the
//!   only process here that can make one is the recorder
//!   ([issue #304](https://github.com/wildware-uk/clipped/issues/304)).
//!
//! So `open_playback` names a recording and a track, and the answer is a file to
//! play and the list of tracks that could have been chosen instead.
//!
//! # What is deliberately not in the answer
//!
//! **A duration, and the picture's dimensions.** The element measures both from
//! the media it is given, and reports them in `loadedmetadata`; a figure sent
//! from here would be a second answer to the same question, arrived at by a
//! different route, and the two would disagree for exactly the files where it
//! matters — a recording a killed recorder left, whose container may have no
//! duration written into it at all
//! ([issue #283](https://github.com/wildware-uk/clipped/issues/283)).
//! `docs/desktop-ui.md` records that decision beside the screen that draws it.

use serde::{Deserialize, Serialize};

/// Which recording to open for playback, and which of its tracks to hear.
///
/// The source is a file the caller already has, because it read it out of the
/// library ([`LibraryRecording::path`](crate::LibraryRecording)) or was told it
/// by the recorder when the recording started. There is no "the recording you
/// just made" shorthand, for the reason
/// [`ExportRecording`](crate::ExportRecording) has none.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenPlayback {
    /// The recording to play, as the library reported its path.
    ///
    /// Required, and with no `serde` default, for the reason
    /// [`ExportRecording::source`](crate::ExportRecording) has none: a request
    /// that leaves it out is [`ErrorCode::InvalidParameters`](crate::ErrorCode)
    /// naming the field, rather than a request carrying an empty path that
    /// something further down has to recognise as "not given".
    ///
    /// It is opened for reading and is not modified, whatever the answer.
    pub source: String,
    /// Which of the recording's sound tracks to hear, as a stream index of the
    /// source.
    ///
    /// Absent means the one a player should choose on its own: the track the
    /// container flags as the default, which for a Clipped recording is the
    /// compatibility mix (`docs/muxing.md`, SPEC.md section 13). It is a
    /// **stream index of the file** rather than an ordinal among the sound
    /// tracks, because that is what [`PlaybackTrack::index`] carries and the
    /// two differ by however many picture tracks come first.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_track: Option<usize>,
}

/// A recording, ready to be played, and what could be heard instead.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackStream {
    /// The file to play.
    ///
    /// The recording itself when the chosen track is the one a media element
    /// would reach anyway, and a copy carrying that track alone when it is not
    /// — see [`Self::prepared`].
    pub path: String,
    /// The source stream index whose sound this carries.
    ///
    /// Absent only for a recording with no sound at all, which is a real case
    /// (a capture that found no audio device) and one a window has to be able
    /// to tell from a track that failed to play.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio_track: Option<usize>,
    /// Every sound track of the recording, in the order the container declares
    /// them.
    ///
    /// The recording's, not the file being played: a prepared copy holds one
    /// track, and this is the list somebody chooses the next one from.
    #[serde(default)]
    pub audio_tracks: Vec<PlaybackTrack>,
    /// Whether [`Self::path`] is a copy made for this choice rather than the
    /// recording itself.
    ///
    /// Worth carrying because it is the difference between an answer that cost
    /// nothing and one that cost a pass over the whole file, and because a
    /// prepared copy is a cache entry rather than something anybody's library
    /// knows about — nothing may present it to a user as their recording.
    #[serde(default)]
    pub prepared: bool,
}

/// One sound track of a recording, as a window offers it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlaybackTrack {
    /// The stream index the container declares it at, which is what
    /// [`OpenPlayback::audio_track`] takes.
    pub index: usize,
    /// What the track is called — `Microphone`, `Game` — where the recording
    /// named it.
    ///
    /// Absent for a file that named none, which is anything not written by
    /// Clipped. A window shows the position instead rather than inventing a
    /// name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// The track's language tag, where the recording carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    /// Whether the container flags this as the track a player should choose on
    /// its own.
    ///
    /// For a Clipped recording that is the compatibility mix, and it leads the
    /// file (`clipped_muxer::AudioSource`). It is **not** a promise about what
    /// a media element will play: Chromium ignores the flag and takes the first
    /// sound track it finds, which is why `open_playback` decides what is
    /// served rather than leaving it to the element.
    #[serde(default)]
    pub default: bool,
}

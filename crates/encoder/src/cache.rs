//! Remembering what was detected, and knowing when to forget it.
//!
//! # Where it lives
//!
//! `%LOCALAPPDATA%\Clipped\encoder-capabilities.json` — the same per-user
//! directory the logs and the log-level file use (docs/logging.md). One file,
//! JSON, small enough to read in a text editor, which matters because "what did
//! Clipped think my GPU could do?" is a question a user can then answer for
//! themselves.
//!
//! # What invalidates it
//!
//! Two things, because two things can make a stored answer wrong.
//!
//! The machine changing, as a [`HardwareSignature`]: every adapter's vendor,
//! device and locally unique identifier, and its **driver version**. A driver
//! update changes the last of those, and a driver update is exactly the event
//! that adds or removes a codec — AV1 encoding reached hardware that had
//! already shipped — so a cache that ignored it would keep answering with last
//! month's capabilities for as long as the machine kept its GPU. Adding,
//! removing or swapping a card changes the rest.
//!
//! Clipped changing, as [`DETECTION_REVISION`]. The reference table and the
//! rules that read it are this project's own content, and correcting either
//! changes the report a machine produces without changing the machine — so
//! without that number, the one thing guaranteed to change would be the one
//! thing that could never invalidate the file, and every existing installation
//! would serve the old answer until its GPU was replaced.
//!
//! The signature is computed from the cheap half of a probe, so checking it
//! costs a DXGI enumeration rather than a Media Foundation startup.
//!
//! # What happens when it is stale, corrupt or unwritable
//!
//! All three are the same thing to a caller: the cache does not answer, the
//! machine is asked again, and the file is overwritten with the new answer.
//! None of them is an error, and none of them can fail a detection — a
//! recorder that would not tell you about your GPU because a cache file was
//! truncated by a power cut would be choosing its own bookkeeping over the user
//! (AGENTS.md section 17). Every case is logged with its reason, so a machine
//! that is somehow never caching says so in the diagnostics rather than just
//! being slow.
//!
//! With one exception, and it is about not losing something the user paid for.
//! Two runs on the same machine no longer produce the same report: one that
//! opened an encoder session knows the encoder's own limits and one that did
//! not knows the published ones. So "the file is overwritten" holds for a run
//! that knows at least as much, and a run that opened no session leaves a
//! stored measurement alone rather than replacing it with a table entry
//! (AGENTS.md section 56). [`CapabilityCache::stored`] is how a writer asks
//! what it would be replacing; [`crate::detect_cached`] holds the rule.
//!
//! The file is written to a temporary name and renamed into place, so a process
//! that dies mid-write leaves the previous answer intact rather than a
//! half-written one.

use core::fmt;
use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use clipped_logging::application_directory;
use serde::{Deserialize, Serialize};

use crate::adapter::Adapter;
use crate::detection::CapabilityReport;

/// The file name inside Clipped's per-user directory.
pub const CACHE_FILE_NAME: &str = "encoder-capabilities.json";

/// The layout version of the cache file.
///
/// Bumped whenever the stored **shape** changes. An older or newer number is
/// treated as a miss rather than as an error: a cache is a derived artefact,
/// and the correct response to one this build cannot read is to detect again
/// and overwrite it.
pub const CACHE_FORMAT: u32 = 2;

/// The revision of the answers this crate gives.
///
/// [`CACHE_FORMAT`] covers the shape of the file and [`HardwareSignature`]
/// covers the machine. This covers the third thing that can make a stored
/// report wrong: Clipped itself. The published limits in [`crate::reference`]
/// and the rules in [`crate::detection`] are content, not layout, so correcting
/// a maximum resolution or tightening an availability rule changes the report
/// for a machine that has not changed at all — and every installation that had
/// already cached the old answer would go on serving it, because neither the
/// hardware nor the file's shape moved.
///
/// **Increment this in any change that would make detection produce a different
/// report for the same machine.** It costs one re-probe per installation, which
/// is the cheaper of the two mistakes (AGENTS.md section 17).
///
/// Revision 2: the Quick Sync backend
/// ([#17](https://github.com/wildware-uk/clipped/issues/17)) made detection
/// probe the same list of Intel runtime names the backend loads from, which
/// added `libmfx64.dll` to the two already there — so a machine that has not
/// changed gets an extra measured line, and one that installs only that name
/// changes availability outright.
///
/// Revision 3: encoder sessions can now be asked for their own limits
/// ([#133](https://github.com/wildware-uk/clipped/issues/133)). A stored
/// revision 2 report has the published limits everywhere, marked inferred, and
/// this build would answer with measured ones for the same machine — so
/// without this bump every installation that had already cached would go on
/// showing yesterday's inferred numbers until its GPU changed.
///
/// # Why the key did not have to change with it
///
/// The stored report now depends on something outside the key: whether the run
/// that wrote it opened an encoder session. That does not make the key wrong,
/// because a measured report and an inferred one are both true of the same
/// machine — one simply says more. Everything that could make a *measurement*
/// stale is already in the key, since the encoder's own limits change when the
/// adapter or its driver changes and at no other time.
///
/// What follows from that is worth stating, because it is the one surprising
/// behaviour: a plain `capabilities` run after a driver update finds the cache
/// stale, re-probes without opening a session, and replaces measured limits
/// with published ones. That is correct — the measurements described the
/// previous driver — and `capabilities --refresh` takes them again.
pub const DETECTION_REVISION: u32 = 3;

/// What a display adapter set looks like, condensed to one line.
///
/// Two machines with the same signature have the same cards with the same
/// drivers, and the same detection would produce the same report. Comparing the
/// string is the whole of the invalidation rule.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HardwareSignature(String);

impl HardwareSignature {
    /// Computes the signature of a set of adapters.
    ///
    /// Sorted by identifier, so that a machine whose adapters DXGI happens to
    /// enumerate in a different order after a reboot is still the same machine.
    #[must_use]
    pub fn of(adapters: &[Adapter]) -> Self {
        let mut parts: Vec<String> = adapters
            .iter()
            .map(|adapter| {
                format!(
                    "{}:{:04x}:{:04x}:{}",
                    adapter.id(),
                    adapter.vendor().pci_id(),
                    adapter.device_id(),
                    adapter
                        .driver_version()
                        .map_or_else(|| "no-driver-version".to_owned(), |v| v.to_string())
                )
            })
            .collect();
        parts.sort();
        Self(format!("v{CACHE_FORMAT}|{}", parts.join("|")))
    }

    /// The signature as it is stored.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for HardwareSignature {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why the cache did not answer.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum StaleReason {
    /// There is no cache file yet, which is the state of every new
    /// installation.
    NotCached,
    /// No cache is in use, because the environment describes no per-user
    /// directory to put one in.
    Disabled,
    /// There is a cache and the caller asked for it to be ignored, as
    /// `clipped-recorder capabilities --refresh` does. The new answer is still
    /// stored.
    Refreshed,
    /// The adapters or their drivers have changed since it was written.
    HardwareChanged,
    /// It was written by a build with a different cache layout.
    FormatChanged {
        /// The format number found in the file.
        found: u32,
    },
    /// It was written by a build whose detection would answer differently, so
    /// the machine is the same and the answer is not.
    DetectionChanged {
        /// The revision found in the file.
        found: u32,
    },
    /// It could not be read.
    Unreadable(String),
    /// It is not the JSON this build expects, which normally means a truncated
    /// or hand-edited file.
    Unparsable(String),
}

impl fmt::Display for StaleReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotCached => formatter.write_str("nothing has been cached yet"),
            Self::Disabled => formatter.write_str("there is nowhere to keep a cache"),
            Self::Refreshed => formatter.write_str("the stored answer was ignored on request"),
            Self::HardwareChanged => {
                formatter.write_str("the adapters or their drivers have changed")
            }
            Self::FormatChanged { found } => write!(
                formatter,
                "the cache was written in format {found} and this build reads {CACHE_FORMAT}"
            ),
            Self::DetectionChanged { found } => write!(
                formatter,
                "the cache was written by detection revision {found} and this build is \
                 {DETECTION_REVISION}, so the stored answer is not the one this build \
                 would give"
            ),
            Self::Unreadable(error) => write!(formatter, "the cache could not be read: {error}"),
            Self::Unparsable(error) => write!(formatter, "the cache is not readable JSON: {error}"),
        }
    }
}

/// Whether the cache answered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CacheState {
    /// It did, and this is what it said.
    Fresh {
        /// The stored report.
        report: CapabilityReport,
        /// When it was originally measured.
        detected_at: SystemTime,
    },
    /// It did not, for this reason.
    Stale(StaleReason),
}

/// Why a cache could not be written.
#[derive(Debug)]
#[non_exhaustive]
pub enum CacheError {
    /// The file or its directory could not be written.
    Io(io::Error),
    /// The report could not be turned into JSON, which would be a bug in this
    /// crate rather than a problem with the machine.
    Serialisation(serde_json::Error),
}

impl fmt::Display for CacheError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "{error}"),
            Self::Serialisation(error) => {
                write!(formatter, "the report could not be serialised: {error}")
            }
        }
    }
}

impl Error for CacheError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Serialisation(error) => Some(error),
        }
    }
}

impl From<io::Error> for CacheError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<serde_json::Error> for CacheError {
    fn from(error: serde_json::Error) -> Self {
        Self::Serialisation(error)
    }
}

/// What is actually written to the file.
#[derive(Debug, Serialize, Deserialize)]
struct CacheFile {
    /// The layout version, first so that it is the first thing a reader sees.
    format: u32,
    /// The revision of the detection that produced the report.
    detection_revision: u32,
    /// The hardware this report describes.
    signature: String,
    /// When it was measured, in seconds since the Unix epoch. A plain number
    /// rather than a formatted time, because nothing in this crate parses
    /// dates and a clock that has moved backwards should not make the file
    /// unreadable.
    detected_at_unix_seconds: u64,
    /// The report.
    report: CapabilityReport,
}

/// A capability report kept between runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CapabilityCache {
    /// Where the report is kept, or `None` when there is nowhere to keep one.
    path: Option<PathBuf>,
    /// Whether to ignore what is stored. Set by `--refresh`, which still writes
    /// the new answer: a user who asks for a fresh look wants the next run to
    /// benefit from it too.
    ignore_stored: bool,
}

impl CapabilityCache {
    /// A cache stored at `path`.
    ///
    /// Tests use this with a directory of their own; the application uses
    /// [`default_path`](Self::default_path).
    #[must_use]
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self {
            path: Some(path.into()),
            ignore_stored: false,
        }
    }

    /// A cache that neither reads nor writes.
    ///
    /// For the case where there is nowhere to put one. Detection then simply
    /// runs every time, which is slower and not wrong.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            path: None,
            ignore_stored: false,
        }
    }

    /// The same cache, ignoring whatever is stored in it.
    ///
    /// What `capabilities --refresh` uses. Storing still happens, so the run
    /// that pays for a probe leaves its answer behind for the next one.
    #[must_use]
    pub fn ignoring_stored(mut self) -> Self {
        self.ignore_stored = true;
        self
    }

    /// Where the cache lives:
    /// `%LOCALAPPDATA%\Clipped\encoder-capabilities.json`.
    ///
    /// `None` when the environment describes no per-user directory, which on
    /// Windows means `%LOCALAPPDATA%` is unset. Detection then simply runs
    /// every time.
    #[must_use]
    pub fn default_path() -> Option<PathBuf> {
        Some(application_directory()?.join(CACHE_FILE_NAME))
    }

    /// The file this cache reads and writes, or `None` when it is disabled.
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Reads the cache, if it still describes this machine.
    #[must_use]
    pub fn load(&self, signature: &HardwareSignature) -> CacheState {
        if self.path.is_none() {
            return CacheState::Stale(StaleReason::Disabled);
        }
        if self.ignore_stored {
            return CacheState::Stale(StaleReason::Refreshed);
        }
        self.stored(signature)
    }

    /// What is on disk for this machine, whatever the caller asked to ignore.
    ///
    /// [`load`](Self::load) answers "should this run use the stored report?",
    /// and `--refresh` answers no to that. This answers a different question —
    /// "what would writing now replace?" — which is not the same question and
    /// must not be given the same answer: a run that opened no encoder session
    /// has to know that the file it is about to overwrite holds measurements it
    /// did not take (see [`crate::detect_cached`]).
    ///
    /// Asked immediately before the write, because the gap between asking and
    /// writing is the only window in which another process can still lose a
    /// measurement here.
    #[must_use]
    pub fn stored(&self, signature: &HardwareSignature) -> CacheState {
        let Some(path) = self.path.as_deref() else {
            return CacheState::Stale(StaleReason::Disabled);
        };

        let contents = match fs::read_to_string(path) {
            Ok(contents) => contents,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return CacheState::Stale(StaleReason::NotCached)
            }
            Err(error) => return CacheState::Stale(StaleReason::Unreadable(error.to_string())),
        };

        let file: CacheFile = match serde_json::from_str(&contents) {
            Ok(file) => file,
            Err(error) => return CacheState::Stale(StaleReason::Unparsable(error.to_string())),
        };

        if file.format != CACHE_FORMAT {
            return CacheState::Stale(StaleReason::FormatChanged { found: file.format });
        }
        if file.detection_revision != DETECTION_REVISION {
            return CacheState::Stale(StaleReason::DetectionChanged {
                found: file.detection_revision,
            });
        }
        if file.signature != signature.as_str() {
            return CacheState::Stale(StaleReason::HardwareChanged);
        }

        CacheState::Fresh {
            report: file.report,
            detected_at: UNIX_EPOCH + Duration::from_secs(file.detected_at_unix_seconds),
        }
    }

    /// Writes a report, replacing whatever was there.
    ///
    /// Written to a neighbouring temporary file and renamed into place, so an
    /// interrupted write leaves the previous answer rather than a truncated
    /// one. The temporary name carries the process identifier, because two
    /// processes detecting at once — which the recorder's own integration tests
    /// produce — would otherwise share one temporary file and could rename a
    /// half-written copy of it into place.
    ///
    /// # Errors
    ///
    /// [`CacheError`] if the directory cannot be created or the file cannot be
    /// written. Callers are expected to carry on: see
    /// [`crate::detect_cached`].
    pub fn store(
        &self,
        signature: &HardwareSignature,
        report: &CapabilityReport,
    ) -> Result<(), CacheError> {
        let Some(path) = self.path.as_deref() else {
            // Nowhere to write is not a failure to write: a disabled cache has
            // done everything it was asked to.
            return Ok(());
        };

        let file = CacheFile {
            format: CACHE_FORMAT,
            detection_revision: DETECTION_REVISION,
            signature: signature.as_str().to_owned(),
            detected_at_unix_seconds: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
            report: report.clone(),
        };

        if let Some(directory) = path
            .parent()
            .filter(|directory| !directory.as_os_str().is_empty())
        {
            fs::create_dir_all(directory)?;
        }

        let temporary = temporary_path(path);
        fs::write(&temporary, serde_json::to_vec_pretty(&file)?)?;
        fs::rename(&temporary, path)?;
        Ok(())
    }
}

/// The temporary file [`CapabilityCache::store`] writes before renaming.
///
/// One per process. A fixed name would be shared by every process writing the
/// same cache, and two of them interleaving write and rename can leave a
/// truncated file where the finished one should be — which the documented
/// atomicity promises will not happen.
fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension(format!("json.{}.tmp", std::process::id()))
}

/// A directory of one test's own, removed when it is dropped.
///
/// Lives here rather than in a test module because the cache is not the only
/// thing whose tests need a cache file nobody else is writing:
/// [`crate::detect_cached`]'s are about what happens to that file, and two
/// copies of this would be two chances to leave a temporary directory behind
/// (AGENTS.md section 55).
#[cfg(test)]
#[derive(Debug)]
pub(crate) struct TestDirectory(PathBuf);

#[cfg(test)]
impl TestDirectory {
    /// A directory named for `label`, the process and the thread, so that tests
    /// running in parallel cannot share one.
    pub(crate) fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "clipped-encoder-cache-{label}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("the temporary directory can be created");
        Self(path)
    }

    /// A cache inside it.
    pub(crate) fn cache(&self) -> CapabilityCache {
        CapabilityCache::at(self.0.join(CACHE_FILE_NAME))
    }
}

#[cfg(test)]
impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::adapter::{AdapterId, DriverVersion};
    use crate::codec::Vendor;
    use crate::detect;
    use crate::probe::{EncoderObservations, SystemFacts};

    fn cache_path(cache: &CapabilityCache) -> PathBuf {
        cache.path().expect("this cache has a path").to_path_buf()
    }

    fn card(driver: u64) -> Adapter {
        Adapter::new(
            AdapterId::from_luid(1, 0),
            "NVIDIA GeForce RTX 4090",
            Vendor::Nvidia,
            0x2684,
            24 * 1024 * 1024 * 1024,
            false,
        )
        .with_driver_version(Some(DriverVersion::from_raw(driver)))
    }

    fn report_for(adapters: Vec<Adapter>) -> CapabilityReport {
        detect(&SystemFacts::new(adapters, EncoderObservations::none()))
    }

    #[test]
    fn a_report_survives_a_round_trip_through_the_file() {
        let directory = TestDirectory::new("round-trip");
        let cache = directory.cache();
        let adapters = vec![card(1)];
        let signature = HardwareSignature::of(&adapters);
        let report = report_for(adapters);

        cache
            .store(&signature, &report)
            .expect("the cache is written");

        match cache.load(&signature) {
            CacheState::Fresh { report: stored, .. } => assert_eq!(stored, report),
            CacheState::Stale(reason) => panic!("the cache should have answered: {reason}"),
        }
    }

    #[test]
    fn a_run_that_ignores_the_stored_report_can_still_see_what_it_would_replace() {
        // The two questions the module doc separates. `--refresh` answers "no"
        // to "should I use this?" and still has to be able to ask "what is
        // there?", because the rule that a cheap probe does not overwrite a
        // measurement is enforced where the write happens.
        let directory = TestDirectory::new("stored-while-ignored");
        let adapters = vec![card(1)];
        let signature = HardwareSignature::of(&adapters);
        let report = report_for(adapters);
        directory
            .cache()
            .store(&signature, &report)
            .expect("the cache is written");

        let ignoring = directory.cache().ignoring_stored();
        assert_eq!(
            ignoring.load(&signature),
            CacheState::Stale(StaleReason::Refreshed)
        );
        match ignoring.stored(&signature) {
            CacheState::Fresh { report: stored, .. } => assert_eq!(stored, report),
            CacheState::Stale(reason) => {
                panic!("the file is there and describes this machine: {reason}")
            }
        }
    }

    #[test]
    fn a_driver_update_invalidates_the_cache() {
        let directory = TestDirectory::new("driver-update");
        let cache = directory.cache();
        let before = vec![card(1)];
        cache
            .store(&HardwareSignature::of(&before), &report_for(before.clone()))
            .expect("the cache is written");

        // Same card, new driver: the one event most likely to change what the
        // encoder can do.
        let after = vec![card(2)];
        assert_eq!(
            cache.load(&HardwareSignature::of(&after)),
            CacheState::Stale(StaleReason::HardwareChanged)
        );
    }

    #[test]
    fn adding_a_second_card_invalidates_the_cache() {
        let directory = TestDirectory::new("second-card");
        let cache = directory.cache();
        let before = vec![card(1)];
        cache
            .store(&HardwareSignature::of(&before), &report_for(before.clone()))
            .expect("the cache is written");

        let mut after = before;
        after.push(Adapter::new(
            AdapterId::from_luid(2, 0),
            "AMD Radeon(TM) Graphics",
            Vendor::Amd,
            0x164E,
            0,
            false,
        ));
        assert_eq!(
            cache.load(&HardwareSignature::of(&after)),
            CacheState::Stale(StaleReason::HardwareChanged)
        );
    }

    #[test]
    fn the_order_adapters_are_enumerated_in_does_not_change_the_signature() {
        let first = card(1);
        let second = Adapter::new(
            AdapterId::from_luid(2, 0),
            "AMD Radeon(TM) Graphics",
            Vendor::Amd,
            0x164E,
            0,
            false,
        );

        assert_eq!(
            HardwareSignature::of(&[first.clone(), second.clone()]),
            HardwareSignature::of(&[second, first])
        );
    }

    #[test]
    fn a_corrupt_cache_is_a_miss_with_a_reason_rather_than_a_failure() {
        let directory = TestDirectory::new("corrupt");
        let cache = directory.cache();
        fs::write(cache_path(&cache), b"{\"format\": 1, this is not JSON")
            .expect("the file can be written");

        match cache.load(&HardwareSignature::of(&[card(1)])) {
            CacheState::Stale(StaleReason::Unparsable(_)) => {}
            other => panic!("a truncated cache should be an unparsable miss, got {other:?}"),
        }
    }

    #[test]
    fn a_cache_from_another_format_is_a_miss_that_names_the_format() {
        let directory = TestDirectory::new("format");
        let cache = directory.cache();
        let adapters = vec![card(1)];
        let signature = HardwareSignature::of(&adapters);
        cache
            .store(&signature, &report_for(adapters))
            .expect("the cache is written");

        // Rewrite the stored format as a version this build does not read.
        let contents = fs::read_to_string(cache_path(&cache)).expect("the cache can be read");
        let mut value: serde_json::Value =
            serde_json::from_str(&contents).expect("the cache is JSON");
        value["format"] = serde_json::json!(CACHE_FORMAT + 1);
        fs::write(cache_path(&cache), value.to_string()).expect("the file can be written");

        assert_eq!(
            cache.load(&signature),
            CacheState::Stale(StaleReason::FormatChanged {
                found: CACHE_FORMAT + 1
            })
        );
    }

    #[test]
    fn a_missing_cache_is_an_ordinary_miss() {
        let directory = TestDirectory::new("missing");
        assert_eq!(
            directory.cache().load(&HardwareSignature::of(&[card(1)])),
            CacheState::Stale(StaleReason::NotCached)
        );
    }

    #[test]
    fn storing_leaves_no_temporary_file_behind() {
        let directory = TestDirectory::new("no-temporary");
        let cache = directory.cache();
        let adapters = vec![card(1)];
        cache
            .store(&HardwareSignature::of(&adapters), &report_for(adapters))
            .expect("the cache is written");

        assert!(!temporary_path(&cache_path(&cache)).exists());
    }

    #[test]
    fn the_temporary_file_belongs_to_one_process() {
        // Two processes writing the same cache must not write the same
        // temporary file: interleaving their writes and renames is how a
        // truncated report gets renamed into place, which is precisely what
        // the atomicity in the documentation promises cannot happen.
        let temporary = temporary_path(Path::new("C:/anywhere/encoder-capabilities.json"));
        let name = temporary
            .file_name()
            .and_then(|name| name.to_str())
            .expect("the temporary file has a name");

        assert!(name.ends_with(".tmp"), "{name}");
        assert!(
            name.contains(&std::process::id().to_string()),
            "the temporary name must be this process's own: {name}"
        );
    }

    #[test]
    fn a_cache_from_an_older_detection_is_a_miss_even_though_the_machine_is_the_same() {
        // The invalidation the hardware signature cannot provide: same cards,
        // same drivers, different Clipped. A build that corrected a published
        // limit or an availability rule must not serve the answer the previous
        // build gave.
        let directory = TestDirectory::new("detection-revision");
        let cache = directory.cache();
        let adapters = vec![card(1)];
        let signature = HardwareSignature::of(&adapters);
        cache
            .store(&signature, &report_for(adapters))
            .expect("the cache is written");

        let contents = fs::read_to_string(cache_path(&cache)).expect("the cache can be read");
        let mut value: serde_json::Value =
            serde_json::from_str(&contents).expect("the cache is JSON");
        value["detection_revision"] = serde_json::json!(DETECTION_REVISION - 1);
        fs::write(cache_path(&cache), value.to_string()).expect("the file can be written");

        assert_eq!(
            cache.load(&signature),
            CacheState::Stale(StaleReason::DetectionChanged {
                found: DETECTION_REVISION - 1
            })
        );
    }

    #[test]
    fn the_default_path_is_inside_clipped_s_own_directory() {
        // `%LOCALAPPDATA%` is always set on Windows and the XDG fallback is
        // always derivable from `$HOME`, so on any machine that runs the tests
        // there is a path; what is asserted is where it points.
        let path = CapabilityCache::default_path().expect("a per-user directory exists");
        assert_eq!(
            path.file_name().and_then(|name| name.to_str()),
            Some(CACHE_FILE_NAME)
        );
        // `Clipped` spelled out rather than taken from `clipped-logging`, so
        // that this asserts where the file goes rather than that a constant
        // equals itself.
        let directory = path.parent().expect("the file is inside a directory");
        assert!(
            directory.ends_with("Clipped") || directory.ends_with("clipped"),
            "the cache should sit in Clipped's own directory, not in {}",
            directory.display()
        );
    }
}

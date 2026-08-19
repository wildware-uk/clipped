//! `get_diagnostics`: how this recorder is capturing, and what this machine can
//! encode.
//!
//! Two of SPEC.md section 36's twelve diagnostics, carried to the desktop
//! application at last ([issue #302](https://github.com/wildware-uk/clipped/issues/302)).
//! Neither is new work — both were already being produced inside this process,
//! and nothing carried either out of it:
//!
//! - **The capture backend** was `clipped_capture::CaptureStatus`, which belongs
//!   to the capture thread and borrows its change list, so it could not leave.
//!   `clipped_session::CaptureAccounting` is the copy that can, and this is what
//!   reads it.
//! - **The encoder** was what `clipped-recorder capabilities` prints, which is a
//!   terminal and not a window. `crate::capabilities::detection` is that same
//!   reading, and this maps it onto the protocol.
//!
//! # Why the mapping is here and not in `clipped-ipc`
//!
//! `clipped-ipc` depends on no other crate of this workspace and must not start
//! to: the desktop application links it, and neither the capture engine nor
//! `clipped-encoder` — which on Windows brings four FFmpeg libraries with it —
//! may come along
//! (`tests/integration/tests/workspace_layering.rs`). So the protocol has wire
//! types and this process owns the translation, exactly as `crate::hotkeys`
//! translates a `clipped_hotkeys::Registration`.
//!
//! # What this costs a recording
//!
//! Nothing on any capture path (AGENTS.md sections 17 and 20). The capture
//! account is one clone out of a mutex the recording thread wrote once, before
//! its first frame. The capability report is
//! [`Probing::WithoutSessions`](clipped_encoder::Probing) — adapter enumeration,
//! a `LoadLibrary` per vendor runtime and a transform enumeration, answered from
//! a cache on a machine whose driver has not changed — and it **never** opens an
//! encoder session. Opening one takes a slot from a game that may be mid-match,
//! and the only thing in this build that does it is `capabilities --refresh`,
//! which is a subcommand that records nothing.
//!
//! # What is not here
//!
//! The other ten of section 36's diagnostics. `docs/diagnostics.md` is the
//! honest table of which and why; the short of it is that nothing measures them
//! yet, and a figure nobody took is not a figure.
//!
//! **No path.** The capability cache lives under the user's account name, the
//! terminal report prints where it is, and this does not (AGENTS.md section 13).
//! What crosses is whether the machine was asked and when the stored answer was
//! taken, which is the part that changes how a bug report reads.

use clipped_encoder::{
    Adapter, AdapterId, Availability, CapabilityReport, CodecSupport, Detection, DetectionSource,
    EncoderKind, EncoderReport, Resolution, Vendor,
};
use clipped_ipc::{
    AdapterSummary, CaptureAccount, CaptureMethodChange, CodecSummary, Diagnostics,
    EffectiveSetting, EncoderAccount, EncoderSummary, ErrorCode, ProtocolError,
};

/// The resolution the framerate ceiling is quoted at.
///
/// The one `clipped-recorder capabilities` quotes it at, for the reason that
/// command gives: 1080p is what most recordings are made at (SPEC.md section
/// 10), and the ceiling at any other size is this one scaled.
const FRAMERATE_REFERENCE: Resolution = Resolution::HD_1080P;

/// Everything the recorder can say about capture and encoding, right now.
///
/// # Errors
///
/// [`ErrorCode::Internal`] when the graphics adapters cannot be enumerated, so
/// no encoder can be detected. That is a refusal rather than an empty report on
/// purpose: a machine with no hardware encoder is a report saying so, and a
/// machine that could not be asked is a different answer that a screen must not
/// draw as the first (AGENTS.md section 27).
pub(crate) fn diagnostics(
    capture: Option<&clipped_session::CaptureAccount>,
    settings: Option<&[clipped_session::config::EffectiveSetting]>,
) -> Result<Diagnostics, ProtocolError> {
    let detection = crate::capabilities::detection().map_err(|error| {
        ProtocolError::new(
            ErrorCode::Internal,
            format!("this machine's encoders could not be detected: {error}"),
        )
    })?;

    Ok(Diagnostics {
        capture: capture.map(capture_account),
        encoders: encoder_account(&detection),
        settings: settings.map(effective_settings),
    })
}

/// What the recording in progress is running with.
///
/// A rename and nothing else: the value and the layer are `clipped_session`'s
/// own answer, spelled in the vocabulary the settings file uses, and this crate
/// does not get to have an opinion about either.
fn effective_settings(
    settings: &[clipped_session::config::EffectiveSetting],
) -> Vec<EffectiveSetting> {
    settings
        .iter()
        .map(|setting| EffectiveSetting {
            setting: setting.key().name().to_owned(),
            value: setting.value().to_owned(),
            source: setting.source().token().to_owned(),
        })
        .collect()
}

/// How the recording in progress is capturing.
fn capture_account(account: &clipped_session::CaptureAccount) -> CaptureAccount {
    CaptureAccount {
        setting: account.setting().to_string(),
        started_with: account.started_with().to_string(),
        current: account.current().to_string(),
        changes: account
            .changes()
            .iter()
            .map(|change| CaptureMethodChange {
                from: change.from().to_string(),
                to: change.to().to_string(),
                restart: change.is_restart(),
                // The same token the log line for this change carries, so that a
                // report and the log about one recording use one vocabulary.
                trigger: change.trigger().log_value().to_owned(),
                reason: change.reason().to_owned(),
            })
            .collect(),
    }
}

/// What this machine can encode.
fn encoder_account(detection: &Detection) -> EncoderAccount {
    let report = detection.report();
    let capture_adapter = report.capture_adapter().map(Adapter::id);

    EncoderAccount {
        probed: matches!(detection.source(), DetectionSource::Probed),
        detected_at: match detection.source() {
            DetectionSource::Probed => None,
            DetectionSource::Cached { detected_at } => Some(timestamp(detected_at)),
        },
        elapsed_ms: u64::try_from(detection.elapsed().as_millis()).unwrap_or(u64::MAX),
        adapters: report
            .adapters()
            .iter()
            .map(|adapter| adapter_summary(adapter, capture_adapter))
            .collect(),
        encoders: report
            .encoders()
            .iter()
            .map(|encoder| encoder_summary(encoder, report))
            .collect(),
    }
}

/// One graphics adapter.
fn adapter_summary(adapter: &Adapter, capture_adapter: Option<AdapterId>) -> AdapterSummary {
    AdapterSummary {
        description: adapter.description().to_owned(),
        vendor: vendor_token(adapter.vendor()),
        kind: adapter.kind().log_value().to_owned(),
        video_memory_bytes: adapter.dedicated_video_memory(),
        driver_version: adapter.driver_version().map(|version| version.to_string()),
        // Compared by identifier rather than by the description that crosses
        // the wire: a machine with two identical cards publishes the same model
        // name twice, and marking both as the one capture runs on would be
        // wrong on exactly the machine this field exists to explain.
        captures: capture_adapter == Some(adapter.id()),
    }
}

/// One encoder family.
fn encoder_summary(encoder: &EncoderReport, report: &CapabilityReport) -> EncoderSummary {
    let availability = encoder.availability();
    EncoderSummary {
        encoder: encoder_token(encoder.kind()).to_owned(),
        label: encoder.kind().to_string(),
        available: availability.is_available(),
        unavailable: unavailable_reason(availability),
        // Asked of the encoder rather than written out here, for the reason
        // `capabilities` asks it: a sentence typed by hand goes stale the day a
        // backend lands, and this one already did twice (issue #167).
        implemented: encoder.kind().is_implemented(),
        adapter: encoder
            .adapter()
            .and_then(|id| report.adapter(id))
            .map(|adapter| adapter.description().to_owned()),
        asked: encoder.was_asked(),
        codecs: encoder.codecs().iter().map(codec_summary).collect(),
    }
}

/// What is known about one codec.
fn codec_summary(support: &CodecSupport) -> CodecSummary {
    let resolution = support.max_resolution();
    let framerate = support.max_framerate_at(FRAMERATE_REFERENCE);
    CodecSummary {
        codec: support.codec().log_value().to_owned(),
        supported: support.supported().value().copied(),
        max_width: resolution.value().map(|size| size.width),
        max_height: resolution.value().map(|size| size.height),
        max_framerate_1080p: framerate.value().copied(),
        // True when *anything* here came from the published table. A report that
        // marked only some cells would leave a reader to work out which of four
        // numbers was the inferred one, and the thing they need to know is
        // whether to trust the row (`crate::capabilities`).
        inferred: [
            support.supported().evidence(),
            resolution.evidence(),
            framerate.evidence(),
        ]
        .into_iter()
        .any(|evidence| evidence == Some(clipped_encoder::Evidence::Inferred)),
    }
}

/// Why an encoder cannot be used, or [`None`] when it can.
///
/// The recorder's own sentence, because only this process knows whether the
/// runtime is missing, the driver is damaged, or the silicon belongs to an
/// adapter no recording here will hand a frame to.
fn unavailable_reason(availability: Availability) -> Option<String> {
    match availability {
        Availability::Available => None,
        Availability::OnAnotherAdapter { capture } => Some(format!(
            "it is present and working, and no recording on this machine can use it: frames are \
             captured on the {capture} adapter"
        )),
        Availability::Unavailable(reason) => Some(reason.to_string()),
    }
}

/// The wire word for a graphics vendor.
///
/// A `match` rather than a rendering of [`Vendor`]'s own `Display`, which writes
/// `NVIDIA` and `PCI vendor 0x8086` — labels for a person rather than tokens for
/// a client. An exhaustive one, so a vendor added to `clipped-encoder` stops
/// this compiling rather than reaching a window unnamed.
fn vendor_token(vendor: Vendor) -> String {
    match vendor {
        Vendor::Nvidia => "nvidia".to_owned(),
        Vendor::Amd => "amd".to_owned(),
        Vendor::Intel => "intel".to_owned(),
        Vendor::Microsoft => "microsoft".to_owned(),
        // Named by its identifier rather than as "other". A vendor Clipped does
        // not recognise is still a particular one, and the number is what makes
        // a bug report about it actionable.
        Vendor::Other(id) => format!("pci:0x{id:04X}"),
    }
}

/// The wire word for an encoder family.
///
/// Exhaustive for the reason [`vendor_token`] is.
const fn encoder_token(kind: EncoderKind) -> &'static str {
    match kind {
        EncoderKind::Nvenc => "nvenc",
        EncoderKind::Amf => "amf",
        EncoderKind::QuickSync => "quick_sync",
        EncoderKind::Software => "software",
    }
}

/// A moment, as RFC 3339 with an offset.
///
/// The same format the protocol carries every other wall-clock reading in, so a
/// window has one thing to parse. A clock that cannot be represented — which
/// means a stored answer with a nonsensical timestamp — is reported as the epoch
/// rather than failing the whole reply, because the encoder report beside it is
/// still worth having.
fn timestamp(at: std::time::SystemTime) -> String {
    time::OffsetDateTime::from(at)
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
mod tests {
    use clipped_encoder::{EncoderKind, Vendor};

    use super::{encoder_token, vendor_token};

    #[test]
    fn every_encoder_family_has_a_wire_word_of_its_own() {
        let mut tokens: Vec<&str> = EncoderKind::ALL.into_iter().map(encoder_token).collect();
        tokens.sort_unstable();
        let count = tokens.len();
        tokens.dedup();

        assert_eq!(
            tokens.len(),
            count,
            "two encoder families sharing a wire word would be indistinguishable to a window"
        );
    }

    #[test]
    fn a_vendor_clipped_does_not_recognise_is_named_by_its_identifier() {
        // Not "other". A window showing "other" for an adapter is a window that
        // has told the user nothing, and the identifier is the one thing that
        // makes the report actionable.
        assert_eq!(vendor_token(Vendor::Other(0x1B36)), "pci:0x1B36");
        assert_eq!(vendor_token(Vendor::from_pci_id(Vendor::AMD)), "amd");
    }
}

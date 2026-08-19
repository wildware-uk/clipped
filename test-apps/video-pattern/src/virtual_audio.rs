//! Which of this machine's audio endpoints belong to a *software* device, and
//! which pairs of them might be the two ends of one virtual audio cable.
//!
//! # Why a test needs this
//!
//! A microphone track can only be measured if a test can decide what the
//! microphone hears, and the only way to decide that is a **virtual audio
//! device**: a render endpoint whose audio reappears on a matching capture
//! endpoint, so that a tone played into one end is captured at the other.
//! VB-Audio's Virtual Cable, VoiceMeeter and — on the machine this was written
//! on — Steam's streaming microphone all provide one.
//!
//! Two rules govern which endpoint a test is allowed to touch, and they pull in
//! opposite directions:
//!
//! - AGENTS.md section 25 forbids assuming a contributor's machine has any
//!   particular device, so the name of one cannot be written down here.
//! - AGENTS.md section 14 forbids opening somebody's real microphone, which
//!   would record whatever room the test was run in.
//!
//! So the question this module answers is not "is *the* virtual cable here" but
//! "**which endpoints have no microphone behind them**" — and the answer has to
//! be reached without opening anything, because by the time a capture client is
//! open on a headset the room is already being recorded.
//!
//! # The property that answers it
//!
//! `PKEY_Device_EnumeratorName`, read from the endpoint's own property store.
//! It names the PnP enumerator that produced the device the endpoint belongs
//! to, and hardware is enumerated by the bus it is plugged into while software
//! is enumerated by [`ROOT`]. On the machine this was written on, in full:
//!
//! ```text
//! render   Speakers (Razer BlackShark V2 Pro 2.4)      USB
//! render   Speakers (Steam Streaming Microphone)       ROOT
//! render   SPDIF Interface (Realtek USB2.0 Audio)      USB
//! render   Headphones (Realtek USB2.0 Audio)           USB
//! render   Speakers (Steam Streaming Speakers)         ROOT
//! capture  Microphone (NexiGo N930E FHD Webcam Audio)  USB
//! capture  Microphone (Realtek USB2.0 Audio)           USB
//! capture  Microphone (Razer BlackShark V2 Pro 2.4)    USB
//! capture  Microphone (Steam Streaming Microphone)     ROOT
//! capture  Webcam 1..4 (NDI Webcam Audio)              ROOT
//! ```
//!
//! Every endpoint with a microphone element behind it — the headset, the
//! webcam, the motherboard's input jack — is `USB`; a display's HDMI audio is
//! `HDAUDIO`. Nothing a person could speak into is `ROOT`, because `ROOT` is
//! precisely the enumerator Windows uses for a device that was installed rather
//! than plugged in. **That is what makes this a gate rather than a heuristic**:
//! it is not "this name looks virtual", it is "Windows says there is no
//! hardware here".
//!
//! Name matching alone could not do this job. `Speakers (Razer BlackShark V2
//! Pro 2.4)` and `Microphone (Razer BlackShark V2 Pro 2.4)` are a matching
//! render/capture pair by every name test that would match a virtual cable —
//! and they are a headset somebody is wearing.
//!
//! # What this module deliberately does not decide
//!
//! **Whether a pair actually loops back.** Two software endpoints on the same
//! device usually are two ends of a cable and sometimes are not, and the honest
//! way to tell is to play a tone into one and listen for it on the other. That
//! belongs to the test that plays tones (`tests/audio/track_isolation.rs`), and
//! it is why [`Endpoints::pairs`] returns *candidates*: this module narrows the
//! search to endpoints that are safe to open, and the measurement decides which
//! one works.

use windows::core::PCWSTR;
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::PROPERTYKEY;
use windows::Win32::Media::Audio::{
    eCapture, eRender, EDataFlow, IMMDeviceEnumerator, MMDeviceEnumerator, DEVICE_STATE_ACTIVE,
};
use windows::Win32::System::Com::StructuredStorage::PropVariantToStringAlloc;
use windows::Win32::System::Com::{
    CoCreateInstance, CoIncrementMTAUsage, CoTaskMemFree, CLSCTX_ALL, STGM_READ,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

/// `PKEY_Device_EnumeratorName`: which PnP enumerator produced the device this
/// endpoint belongs to.
///
/// Not among the `windows` crate's generated constants, so it is written out.
/// The GUID is the one [`PKEY_Device_FriendlyName`] also uses — the two are
/// properties 24 and 14 of one property set.
const PKEY_DEVICE_ENUMERATOR_NAME: PROPERTYKEY = PROPERTYKEY {
    fmtid: windows::core::GUID::from_u128(0xa45c_254e_df1c_4efd_8020_67d1_46a8_50e0),
    pid: 24,
};

/// The enumerator name of a device that has no hardware behind it.
///
/// Windows enumerates a device by the bus it was found on — `USB`, `HDAUDIO`,
/// `BTHENUM`, `INTELAUDIO` — and root-enumerates one that was installed by
/// software and found on no bus at all. A capture endpoint under `ROOT` is
/// therefore not a microphone: there is no microphone element for it to be.
const ROOT: &str = "ROOT";

/// One audio endpoint that belongs to a software device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    id: String,
    name: String,
    device: String,
}

impl Endpoint {
    /// The endpoint identifier WASAPI opens it by.
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// The name Windows shows for it, such as
    /// `Microphone (Steam Streaming Microphone)`.
    ///
    /// This is what a recording's device setting names a device by, so it is
    /// the string a test hands to `AudioSourceSetting::Named`.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The device the endpoint belongs to, as [`device_of`] reads it out of
    /// [`name`](Self::name).
    #[must_use]
    pub fn device(&self) -> &str {
        &self.device
    }
}

/// Every endpoint on this machine that belongs to a software device, and how
/// many were left alone because they belong to hardware.
#[derive(Debug, Clone, Default)]
pub struct Endpoints {
    render: Vec<Endpoint>,
    capture: Vec<Endpoint>,
    hardware_capture: usize,
}

impl Endpoints {
    /// The software render endpoints, in the order Windows listed them.
    #[must_use]
    pub fn render(&self) -> &[Endpoint] {
        &self.render
    }

    /// The software capture endpoints, in the order Windows listed them.
    #[must_use]
    pub fn capture(&self) -> &[Endpoint] {
        &self.capture
    }

    /// How many capture endpoints were rejected for having hardware behind
    /// them.
    ///
    /// Reported so that a test which cannot run can say *why* in a sentence
    /// that distinguishes "this machine has no microphone at all" from "this
    /// machine has three and every one of them is somebody's room".
    #[must_use]
    pub fn hardware_capture(&self) -> usize {
        self.hardware_capture
    }

    /// Render/capture pairs worth playing a tone through, most likely first.
    ///
    /// Every pair is safe to open — both ends came through the [`ROOT`] gate —
    /// and the order is the only heuristic in this module: a render and a
    /// capture endpoint that name the **same device** are the two ends of one
    /// virtual cable far more often than two that do not, so those are tried
    /// first and, on a machine that has one, nothing else is ever opened at
    /// all.
    ///
    /// It is an ordering rather than a filter on purpose. A device whose two
    /// ends are named differently would be excluded by a name rule, and is
    /// found by a tone.
    #[must_use]
    pub fn pairs(&self) -> Vec<(&Endpoint, &Endpoint)> {
        let mut pairs: Vec<(&Endpoint, &Endpoint)> = self
            .render
            .iter()
            .flat_map(|render| self.capture.iter().map(move |capture| (render, capture)))
            .collect();
        pairs.sort_by_key(|(render, capture)| u8::from(render.device() != capture.device()));
        pairs
    }
}

/// Reads every active endpoint and keeps the ones with no hardware behind them.
///
/// Opens nothing: an endpoint's property store is read from the enumerator and
/// no audio client is activated. That is the whole point — a machine whose only
/// microphone is a real one must reach the end of this function without having
/// recorded a sound.
///
/// # Errors
///
/// Why this machine's endpoints could not be listed, as a sentence: COM is
/// unavailable, or the device enumerator refused. Both are legitimate outcomes
/// for a test to skip on rather than faults (AGENTS.md section 16).
pub fn software_endpoints() -> Result<Endpoints, String> {
    // SAFETY: `CoIncrementMTAUsage` takes a process-wide reference to the
    // multi-threaded apartment, deliberately never given back — the same
    // reasoning as `crate::render_stream` and
    // `crates/audio/src/windows/apartment.rs`.
    unsafe { CoIncrementMTAUsage() }.map_err(|error| format!("COM is unavailable: {error}"))?;

    (|| -> windows::core::Result<Endpoints> {
        // SAFETY: `MMDeviceEnumerator` is the class identifier for
        // `IMMDeviceEnumerator`, which is the interface the binding asks for.
        let enumerator: IMMDeviceEnumerator =
            unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }?;

        let (render, _) = software_endpoints_of(&enumerator, eRender)?;
        let (capture, hardware_capture) = software_endpoints_of(&enumerator, eCapture)?;
        Ok(Endpoints {
            render,
            capture,
            hardware_capture,
        })
    })()
    .map_err(|error| format!("this machine's audio endpoints could not be listed: {error}"))
}

/// The software endpoints of one direction, and how many hardware ones there
/// were.
fn software_endpoints_of(
    enumerator: &IMMDeviceEnumerator,
    flow: EDataFlow,
) -> windows::core::Result<(Vec<Endpoint>, usize)> {
    // SAFETY: `flow` is a value of the enumeration named and the enumerator is
    // live. Only active endpoints are asked for: an unplugged or disabled one
    // cannot carry a tone.
    let collection = unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }?;
    // SAFETY: `collection` is live.
    let count = unsafe { collection.GetCount() }?;

    let mut software = Vec::new();
    let mut hardware = 0;
    for index in 0..count {
        // SAFETY: `index` is below the count just read.
        let device = unsafe { collection.Item(index) }?;
        // SAFETY: `device` is live. The identifier is a `CoTaskMemAlloc`
        // allocation this function now owns and gives back below.
        let id = unsafe { device.GetId() }?;
        // SAFETY: `id` points at a NUL-terminated wide string until it is
        // freed, which happens after this.
        let identifier = unsafe { PCWSTR(id.0).to_string() }.unwrap_or_default();
        // SAFETY: `id` came from `GetId`, has not been freed, and is not used
        // again.
        unsafe { CoTaskMemFree(Some(id.0.cast())) };

        // SAFETY: `device` is live and `STGM_READ` is the access the read-only
        // calls below need.
        let store = unsafe { device.OpenPropertyStore(STGM_READ) }?;
        let name = property(&store, &PKEY_Device_FriendlyName);
        let produced_by = property(&store, &PKEY_DEVICE_ENUMERATOR_NAME);

        // Anything this could not read is counted as hardware. The gate has to
        // fail closed: a property that would not answer is a question this
        // cannot settle, and the answer it must not guess at is the one that
        // opens a microphone.
        if !produced_by.eq_ignore_ascii_case(ROOT) || identifier.is_empty() || name.is_empty() {
            hardware += 1;
            continue;
        }
        software.push(Endpoint {
            id: identifier,
            device: device_of(&name).to_owned(),
            name,
        });
    }
    Ok((software, hardware))
}

/// Reads one string property, or an empty string if it is missing.
fn property(store: &IPropertyStore, key: &PROPERTYKEY) -> String {
    // SAFETY: `store` is live and `key` is a valid property key. A property the
    // endpoint does not carry is reported as an error rather than read.
    let Ok(value) = (unsafe { store.GetValue(key) }) else {
        return String::new();
    };
    // SAFETY: `value` is a live `PROPVARIANT`; the helper copies whatever it
    // holds into a new allocation and reports a type it cannot render as an
    // error.
    let Ok(text) = (unsafe { PropVariantToStringAlloc(&value) }) else {
        return String::new();
    };
    // SAFETY: `text` is a `CoTaskMemAlloc` allocation holding a NUL-terminated
    // wide string, and `to_string` copies it out.
    let read = unsafe { text.to_string() }.unwrap_or_default();
    // SAFETY: `text` came from `PropVariantToStringAlloc`, has been copied out,
    // and is not used again (AGENTS.md section 58).
    unsafe { CoTaskMemFree(Some(text.0.cast())) };
    read
}

/// The device an endpoint's friendly name belongs to.
///
/// Windows names an endpoint `<role> (<device>)` — `Microphone (Steam Streaming
/// Microphone)`, `CABLE Output (VB-Audio Virtual Cable)` — and it is the device
/// half that two ends of one cable share. A name in any other shape is its own
/// device, which is the safe answer: it can only make [`Endpoints::pairs`] try
/// a pair later than it might have, never skip one.
#[must_use]
pub fn device_of(name: &str) -> &str {
    let trimmed = name.trim();
    let Some(open) = trimmed.rfind(" (") else {
        return trimmed;
    };
    let Some(inner) = trimmed[open + 2..].strip_suffix(')') else {
        return trimmed;
    };
    if inner.is_empty() {
        return trimmed;
    }
    inner
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The names here are real: the Steam ones were read off the machine this
    /// was written on by `PKEY_Device_FriendlyName`, and the VB-Audio and
    /// VoiceMeeter ones are what those products install.
    #[test]
    fn the_device_half_of_a_name_is_what_two_ends_of_a_cable_share() {
        assert_eq!(
            device_of("Microphone (Steam Streaming Microphone)"),
            "Steam Streaming Microphone"
        );
        assert_eq!(
            device_of("Speakers (Steam Streaming Microphone)"),
            "Steam Streaming Microphone"
        );
        assert_eq!(
            device_of("CABLE Input (VB-Audio Virtual Cable)"),
            "VB-Audio Virtual Cable"
        );
        assert_eq!(
            device_of("CABLE Output (VB-Audio Virtual Cable)"),
            "VB-Audio Virtual Cable"
        );
        assert_eq!(
            device_of("VoiceMeeter Input (VB-Audio VoiceMeeter VAIO)"),
            "VB-Audio VoiceMeeter VAIO"
        );
    }

    #[test]
    fn a_name_in_any_other_shape_is_its_own_device() {
        assert_eq!(device_of("Line In"), "Line In");
        assert_eq!(device_of("Speakers ()"), "Speakers ()");
        assert_eq!(
            device_of("Speakers (unterminated"),
            "Speakers (unterminated"
        );
    }

    /// The ordering [`Endpoints::pairs`] promises, and the reason it is an
    /// ordering rather than a filter: the pair that names one device is tried
    /// first, and the pair that names two is still tried.
    #[test]
    fn pairs_try_the_two_ends_of_one_device_before_anything_else() {
        let endpoint = |name: &str| Endpoint {
            id: format!("id:{name}"),
            device: device_of(name).to_owned(),
            name: name.to_owned(),
        };
        let endpoints = Endpoints {
            render: vec![
                endpoint("Speakers (Steam Streaming Speakers)"),
                endpoint("Speakers (Steam Streaming Microphone)"),
            ],
            capture: vec![
                endpoint("Webcam 1 (NDI Webcam Audio)"),
                endpoint("Microphone (Steam Streaming Microphone)"),
            ],
            hardware_capture: 3,
        };

        let pairs = endpoints.pairs();
        assert_eq!(pairs.len(), 4, "every safe combination stays available");
        assert_eq!(
            (pairs[0].0.name(), pairs[0].1.name()),
            (
                "Speakers (Steam Streaming Microphone)",
                "Microphone (Steam Streaming Microphone)"
            ),
            "the pair naming one device is tried first"
        );
    }
}

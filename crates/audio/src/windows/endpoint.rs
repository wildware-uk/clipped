//! Finding the endpoint a capture records, and reading what shape its audio is.
//!
//! Clipped records two kinds of endpoint — the one Windows plays through, in
//! loopback mode, and a microphone — and they differ in so little that one
//! description of "which endpoint, and how" serves both: [`EndpointSource`].
//! Everything that follows from it, from opening the stream to filling the gap
//! left by a device that was unplugged, is the same code
//! (`endpoint_capture.rs`).

use core::num::{NonZeroU16, NonZeroU32};

use clipped_logging::AudioSource;
use windows::core::{GUID, HRESULT, HSTRING, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eCapture, eConsole, eRender, EDataFlow, IAudioClient, IMMDevice, IMMDeviceEnumerator,
    MMDeviceEnumerator, AUDCLNT_STREAMFLAGS_LOOPBACK, DEVICE_STATE_ACTIVE, WAVEFORMATEX,
    WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
};
use windows::Win32::Media::KernelStreaming::{KSDATAFORMAT_SUBTYPE_PCM, WAVE_FORMAT_EXTENSIBLE};
use windows::Win32::Media::Multimedia::{KSDATAFORMAT_SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_IEEE_FLOAT};
use windows::Win32::System::Com::StructuredStorage::PropVariantClear;
use windows::Win32::System::Com::{CoCreateInstance, CoTaskMemFree, CLSCTX_ALL, STGM_READ};
use windows::Win32::System::Variant::VT_LPWSTR;

use crate::error::AudioError;
use crate::format::{AudioFormat, ChannelMask, SampleFormat};

/// What `GetDefaultAudioEndpoint` returns when the machine has no endpoint for
/// the role asked for.
///
/// `HRESULT_FROM_WIN32(ERROR_NOT_FOUND)`, spelled out because windows-rs
/// generates the Win32 error code and the `HRESULT` constants in different
/// namespaces and neither is `E_NOTFOUND`.
const E_NOTFOUND: HRESULT = HRESULT((0x8007_0000 | ERROR_NOT_FOUND.0) as i32);

/// Wraps a Windows failure as an [`AudioError::Platform`] naming the attempt.
pub(super) fn platform_error(operation: &'static str, error: windows::core::Error) -> AudioError {
    AudioError::Platform {
        operation,
        source: Box::new(error),
    }
}

/// Creates the device enumerator, which is also what endpoint notifications are
/// registered on.
pub(super) fn create_enumerator() -> Result<IMMDeviceEnumerator, AudioError> {
    // SAFETY: `MMDeviceEnumerator` is the documented class identifier for
    // `IMMDeviceEnumerator`, and windows-rs infers the interface identifier
    // from the return type, so the two cannot disagree. The apartment the call
    // needs is ensured by the caller before this runs.
    unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }
        .map_err(|error| platform_error("creating the audio device enumerator", error))
}

/// Which side of the audio stack a capture records, and what its audio is
/// called.
///
/// The two captures Clipped runs differ in three things and no more: whether
/// the endpoints they open render or capture, whether the stream is a loopback
/// of a render endpoint, and the words a log line describes them with. So they
/// are one engine parameterised by this rather than two implementations of the
/// same WASAPI stream, the same timeline and the same device-change handling
/// (AGENTS.md section 55).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SourceKind {
    /// The endpoint Windows plays through, recorded in loopback mode.
    SystemAudio,
    /// An input device, recorded directly.
    Microphone,
}

impl SourceKind {
    /// Which side of the audio stack this kind's endpoints are on.
    pub(super) fn flow(self) -> EDataFlow {
        match self {
            Self::SystemAudio => eRender,
            Self::Microphone => eCapture,
        }
    }

    /// The flags `IAudioClient::Initialize` opens the stream with, beside the
    /// event-driven flag the stream adds when the audio engine accepts it.
    ///
    /// A render endpoint can only be recorded in loopback mode; a capture
    /// endpoint is recorded as it is, and asking for loopback on one fails.
    pub(super) fn stream_flags(self) -> u32 {
        match self {
            Self::SystemAudio => AUDCLNT_STREAMFLAGS_LOOPBACK,
            Self::Microphone => 0,
        }
    }

    /// What the `audio_source` field on this capture's log lines says, so that
    /// two captures running in one recording can be told apart in a log
    /// (AGENTS.md section 35).
    ///
    /// `clipped-logging`'s enumeration rather than words chosen here: the
    /// values `audio_source` may take are a closed list in `docs/logging.md`,
    /// and the point of that list is that somebody searching a user's log
    /// months later types one term rather than guessing which of several
    /// spellings a crate used.
    pub(super) fn audio_source(self) -> AudioSource {
        match self {
            Self::SystemAudio => AudioSource::SystemAudio,
            Self::Microphone => AudioSource::Microphone,
        }
    }
}

/// Which device of that kind a capture records.
///
/// The name is carried beside the identifier so that a message about a device
/// that is not there can say which device the user chose, which is the
/// difference between the message AGENTS.md section 45 asks for and an error
/// code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum DeviceSelection {
    /// Whatever Windows currently makes the default, following it when it
    /// moves.
    Default,
    /// One particular device, and only that one. A capture on a chosen device
    /// waits for it to come back rather than quietly recording a different
    /// one: a microphone track that silently became the webcam's microphone
    /// would be worse than a silent one.
    Device {
        /// `IMMDevice::GetId`, which is stable across reboots and is therefore
        /// what a stored choice holds.
        id: String,
        /// The name to use when talking about it, including while it is
        /// missing and cannot be asked for one.
        name: String,
    },
}

/// Which endpoint a capture opens, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EndpointSource {
    pub(super) kind: SourceKind,
    pub(super) selection: DeviceSelection,
}

impl EndpointSource {
    /// The endpoint Windows is playing through, recorded in loopback mode.
    pub(super) fn system_audio() -> Self {
        Self {
            kind: SourceKind::SystemAudio,
            selection: DeviceSelection::Default,
        }
    }

    /// A microphone: the default input device, or a chosen one.
    pub(super) fn microphone(selection: DeviceSelection) -> Self {
        Self {
            kind: SourceKind::Microphone,
            selection,
        }
    }

    /// Whether Windows moving the default endpoint concerns this capture.
    ///
    /// It does not when the user chose a device: the default moving to the
    /// laptop's built-in microphone because a headset was unplugged must not
    /// silently move a recording onto it.
    pub(super) fn follows_default(&self) -> bool {
        matches!(self.selection, DeviceSelection::Default)
    }

    /// The device this source refers to, or [`None`] if it is not there.
    ///
    /// "Not there" is a state to report rather than an error to raise: a laptop
    /// with every output unplugged genuinely has no default render endpoint, a
    /// machine part-way through installing a driver has no endpoint at all, and
    /// a chosen microphone is unplugged every time its owner leaves the desk.
    /// The caller decides whether that ends an attempt to start recording or is
    /// something to keep waiting through.
    pub(super) fn resolve(
        &self,
        enumerator: &IMMDeviceEnumerator,
    ) -> Result<Option<IMMDevice>, AudioError> {
        match &self.selection {
            DeviceSelection::Default => default_endpoint(enumerator, self.kind.flow()),
            DeviceSelection::Device { id, .. } => active_device(enumerator, id),
        }
    }

    /// How the device is named in a log line, including while there is no
    /// device to ask.
    pub(super) fn device_description(&self) -> &str {
        match (&self.selection, self.kind) {
            (DeviceSelection::Device { name, .. }, _) => name,
            (DeviceSelection::Default, SourceKind::SystemAudio) => "the default output device",
            (DeviceSelection::Default, SourceKind::Microphone) => "the default microphone",
        }
    }
}

/// The default endpoint for a data flow, or [`None`] if the machine has none.
///
/// `eConsole` is the role Windows plays games and media through and records
/// from by default, which is the audio a recording is about. `eCommunications`
/// — which a headset may hold while speakers and a desk microphone hold the
/// console role — is the role a chat application picks, and following it would
/// mean a recording that changed device whenever a call started.
pub(super) fn default_endpoint(
    enumerator: &IMMDeviceEnumerator,
    flow: EDataFlow,
) -> Result<Option<IMMDevice>, AudioError> {
    // SAFETY: `enumerator` is a live `IMMDeviceEnumerator`, and both arguments
    // are values of the enumerations the signature names.
    match unsafe { enumerator.GetDefaultAudioEndpoint(flow, eConsole) } {
        Ok(device) => Ok(Some(device)),
        // The documented answer when there is no endpoint for the role, rather
        // than a failure of the call.
        Err(error) if error.code() == E_NOTFOUND => Ok(None),
        Err(error) => Err(platform_error(
            "asking Windows for the default audio device",
            error,
        )),
    }
}

/// A device by identifier, or [`None`] if it is not present and usable.
///
/// Windows remembers a device long after it was last plugged in, and
/// `GetDevice` answers happily for one that is unplugged or disabled — the
/// stream then fails at `Activate`. So the state is what decides, and a device
/// that is not active is reported the same way as one Windows has never heard
/// of: not there, wait for it.
fn active_device(
    enumerator: &IMMDeviceEnumerator,
    id: &str,
) -> Result<Option<IMMDevice>, AudioError> {
    // SAFETY: `enumerator` is a live `IMMDeviceEnumerator` and `id` is a
    // null-terminated wide string that outlives the call.
    let device = match unsafe { enumerator.GetDevice(&HSTRING::from(id)) } {
        Ok(device) => device,
        Err(error) if error.code() == E_NOTFOUND => return Ok(None),
        Err(error) => {
            return Err(platform_error(
                "asking Windows for the audio device this recording is set to use",
                error,
            ))
        }
    };

    // SAFETY: `device` is the live `IMMDevice` just returned.
    let state = unsafe { device.GetState() }
        .map_err(|error| platform_error("reading the audio device's state", error))?;
    Ok((state == DEVICE_STATE_ACTIVE).then_some(device))
}

/// Every active endpoint on one side of the audio stack, in the order Windows
/// lists them.
///
/// Only active endpoints: a device that is unplugged or disabled cannot be
/// recorded, and offering it in a list of microphones to choose from would be
/// offering a choice that fails.
pub(super) fn active_endpoints(
    enumerator: &IMMDeviceEnumerator,
    flow: EDataFlow,
) -> Result<Vec<EndpointIdentity>, AudioError> {
    // SAFETY: `enumerator` is a live `IMMDeviceEnumerator` and both arguments
    // are values of the types the signature names.
    let devices = unsafe { enumerator.EnumAudioEndpoints(flow, DEVICE_STATE_ACTIVE) }
        .map_err(|error| platform_error("listing the machine's audio devices", error))?;
    // SAFETY: `devices` is the live `IMMDeviceCollection` just returned.
    let count = unsafe { devices.GetCount() }
        .map_err(|error| platform_error("counting the machine's audio devices", error))?;

    let mut endpoints = Vec::with_capacity(count as usize);
    for index in 0..count {
        // SAFETY: `index` is below the count the collection just reported.
        let device = unsafe { devices.Item(index) }
            .map_err(|error| platform_error("reading an audio device from the list", error))?;
        endpoints.push(EndpointIdentity::of(&device)?);
    }
    Ok(endpoints)
}

/// The mute switch Windows keeps for an endpoint.
///
/// A muted microphone is the commonest reason a microphone track is silent, and
/// it is invisible from the capture stream: WASAPI delivers packets of silence
/// exactly as it would for a quiet room. Reading the switch is what lets Clipped
/// say which of the two it is.
///
/// Best effort throughout. A device whose endpoint volume cannot be activated —
/// which a virtual device may well refuse — records perfectly well, so a failure
/// here answers "unknown" rather than ending anything.
///
/// The `IAudioEndpointVolume` is owned by this value and released when it drops
/// (AGENTS.md section 58); windows-rs releases the reference in its own `Drop`.
#[derive(Debug)]
pub(super) struct EndpointMute(IAudioEndpointVolume);

impl EndpointMute {
    /// Activates the endpoint volume interface on a device, if Windows offers
    /// one.
    pub(super) fn of(device: &IMMDevice) -> Option<Self> {
        // SAFETY: `device` is a live `IMMDevice`; windows-rs infers the
        // interface identifier from the return type, so the activation cannot
        // ask for one interface and be typed as another.
        unsafe { device.Activate(CLSCTX_ALL, None) }.ok().map(Self)
    }

    /// Whether the device is muted at the operating-system level, or [`None`]
    /// if Windows would not say.
    pub(super) fn is_muted(&self) -> Option<bool> {
        // SAFETY: `self.0` is a live `IAudioEndpointVolume` and `GetMute` takes
        // no arguments.
        unsafe { self.0.GetMute() }
            .ok()
            .map(|muted| muted.as_bool())
    }
}

/// How an endpoint is named in a log line.
///
/// The identifier is what correlates two log lines about the same device; the
/// name is what makes a support conversation possible. Neither is user content:
/// they are the machine's description of its own hardware, which
/// `docs/privacy.md` lists among the things diagnostics may contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EndpointIdentity {
    /// `IMMDevice::GetId`, stable across reboots for the same device.
    pub(super) id: String,
    /// The name Windows shows in the volume flyout, when it could be read.
    pub(super) name: String,
}

impl EndpointIdentity {
    /// Reads both from the device, tolerating a missing name.
    pub(super) fn of(device: &IMMDevice) -> Result<Self, AudioError> {
        // SAFETY: `device` is a live `IMMDevice`. `GetId` returns a
        // `CoTaskMemAlloc`'d string this call owns; `to_string` copies it, and
        // `CoTaskMemFree` releases it below whether or not the copy succeeded.
        let raw = unsafe { device.GetId() }
            .map_err(|error| platform_error("reading the audio device's identifier", error))?;
        // SAFETY: `raw` is a null-terminated wide string Windows just returned.
        let id = unsafe { raw.to_string() }.unwrap_or_else(|_| "<unreadable>".to_owned());
        // SAFETY: `raw` came from `CoTaskMemAlloc` inside `GetId`, which
        // documents the caller as its owner, and nothing else refers to it now
        // that the copy has been taken.
        unsafe { CoTaskMemFree(Some(raw.as_ptr().cast())) };

        Ok(Self {
            id,
            name: friendly_name(device).unwrap_or_else(|| "<unnamed>".to_owned()),
        })
    }
}

/// The device's display name, or [`None`] if Windows would not give one.
///
/// Everything here is best effort: a device with no property store, or one
/// whose name is not a string, still records perfectly well, and failing a
/// recording because a log line would be less readable would be absurd.
fn friendly_name(device: &IMMDevice) -> Option<String> {
    // SAFETY: `device` is a live `IMMDevice` and `STGM_READ` is a valid access
    // mode for `OpenPropertyStore`.
    let store = unsafe { device.OpenPropertyStore(STGM_READ) }.ok()?;
    // SAFETY: `store` is a live `IPropertyStore` and `PKEY_Device_FriendlyName`
    // is a property key constant. The `PROPVARIANT` returned is owned by this
    // function and cleared below.
    let mut value = unsafe { store.GetValue(&PKEY_Device_FriendlyName) }.ok()?;

    // SAFETY: reading the discriminant and then the matching arm of the union
    // is exactly the contract of a `PROPVARIANT`: `vt` says which member is
    // live, and `VT_LPWSTR` means `pwszVal` is a null-terminated wide string.
    let name = unsafe {
        let contents = &value.Anonymous.Anonymous;
        if contents.vt == VT_LPWSTR {
            contents.Anonymous.pwszVal.to_string().ok()
        } else {
            None
        }
    };

    // SAFETY: `value` is a live `PROPVARIANT` this function owns, and clearing
    // it is what releases the string inside. It is not read after this.
    let _ = unsafe { PropVariantClear(&mut value) };
    name
}

/// An endpoint's mix format, and the `WAVEFORMATEX` that has to be handed back
/// to `IAudioClient::Initialize`.
///
/// The two are kept together because they must agree: initialising a stream
/// with a format other than the one that was interpreted would produce audio
/// this crate then converts wrongly. Dropping this frees the Windows-allocated
/// structure, which is the only thing that does (AGENTS.md section 58).
#[derive(Debug)]
pub(super) struct MixFormat {
    raw: *mut WAVEFORMATEX,
    audio: AudioFormat,
}

impl MixFormat {
    /// Reads and interprets `IAudioClient::GetMixFormat`.
    pub(super) fn of(client: &IAudioClient) -> Result<Self, AudioError> {
        // SAFETY: `client` is a live `IAudioClient` that has not been
        // initialised, which is when `GetMixFormat` is valid. The returned
        // pointer is `CoTaskMemAlloc`'d and owned by this value from here on.
        let raw = unsafe { client.GetMixFormat() }
            .map_err(|error| platform_error("reading the audio device's mix format", error))?;

        match interpret(raw) {
            Ok(audio) => Ok(Self { raw, audio }),
            Err(error) => {
                // SAFETY: `raw` is the pointer `GetMixFormat` just returned and
                // nothing else refers to it.
                unsafe { CoTaskMemFree(Some(raw.cast())) };
                Err(error)
            }
        }
    }

    /// The interpreted format.
    pub(super) fn audio(&self) -> AudioFormat {
        self.audio
    }

    /// The structure to pass to `IAudioClient::Initialize`.
    pub(super) fn as_ptr(&self) -> *const WAVEFORMATEX {
        self.raw
    }
}

impl Drop for MixFormat {
    fn drop(&mut self) {
        // SAFETY: `raw` came from `CoTaskMemAlloc` inside `GetMixFormat`, this
        // value has owned it since, and nothing refers to it after the drop.
        unsafe { CoTaskMemFree(Some(self.raw.cast())) };
    }
}

/// Interprets a `WAVEFORMATEX`, which may or may not be a
/// `WAVEFORMATEXTENSIBLE`.
///
/// Both structures are declared `#[repr(packed)]`, so every field is read with
/// `read_unaligned` rather than through a reference. That is not defensive
/// style: taking a reference to a field of a packed structure is undefined
/// behaviour, and the compiler refuses it.
fn interpret(raw: *const WAVEFORMATEX) -> Result<AudioFormat, AudioError> {
    // SAFETY: `raw` is the pointer `GetMixFormat` returned, which Windows
    // documents as a `WAVEFORMATEX` — the header every mix format starts with —
    // followed by `cbSize` further bytes.
    let header = unsafe { raw.read_unaligned() };

    let sample_rate = NonZeroU32::new(header.nSamplesPerSec).ok_or_else(|| {
        AudioError::unsupported_format("the device reports a sample rate of zero")
    })?;
    let channels = NonZeroU16::new(header.nChannels)
        .ok_or_else(|| AudioError::unsupported_format("the device reports zero channels"))?;

    let (kind, channel_mask) = if header.wFormatTag == WAVE_FORMAT_EXTENSIBLE as u16
        && usize::from(header.cbSize)
            >= size_of::<WAVEFORMATEXTENSIBLE>() - size_of::<WAVEFORMATEX>()
    {
        // SAFETY: the format tag and `cbSize` together are exactly what
        // Windows documents as meaning the allocation is a
        // `WAVEFORMATEXTENSIBLE`, and both are checked immediately above.
        let extensible = unsafe { raw.cast::<WAVEFORMATEXTENSIBLE>().read_unaligned() };
        (
            Subtype::Guid(extensible.SubFormat),
            extensible.dwChannelMask,
        )
    } else {
        (Subtype::Tag(header.wFormatTag), 0)
    };

    let endpoint_samples = sample_format(kind, header.wBitsPerSample).ok_or_else(|| {
        AudioError::unsupported_format(format!(
            "tag {:#06x}, {} bits per sample, {kind}",
            { header.wFormatTag },
            { header.wBitsPerSample }
        ))
    })?;

    // A block that is not exactly one sample per channel means the interleaving
    // this crate assumes is wrong, and reading it as though it were right would
    // produce noise rather than audio. No shipping endpoint does this; refusing
    // is still better than trusting arithmetic that has not been checked.
    let expected_block = usize::from(channels.get()) * endpoint_samples.bytes_per_sample();
    if usize::from(header.nBlockAlign) != expected_block {
        return Err(AudioError::unsupported_format(format!(
            "a block of {} bytes for {} channels of {endpoint_samples}, where {expected_block} \
             were expected",
            { header.nBlockAlign },
            channels.get()
        )));
    }

    Ok(AudioFormat::new(
        sample_rate,
        channels,
        ChannelMask::from_bits(channel_mask),
        endpoint_samples,
    ))
}

/// How a format names its sample type: an old-style tag, or an extensible
/// subformat GUID.
#[derive(Debug, Clone, Copy)]
enum Subtype {
    Tag(u16),
    Guid(GUID),
}

impl core::fmt::Display for Subtype {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Tag(tag) => write!(f, "format tag {tag:#06x}"),
            Self::Guid(guid) => write!(f, "subformat {guid:?}"),
        }
    }
}

/// The sample format a tag or subformat and a container width describe.
///
/// `bits` is the *container* size for an extensible format, which is what
/// determines how the bytes are laid out;
/// `wValidBitsPerSample` may be smaller (24 valid bits in a 32-bit container is
/// common), and the unused low bits are zero, so the conversion is the same as
/// for the full width.
fn sample_format(kind: Subtype, bits: u16) -> Option<SampleFormat> {
    let float = match kind {
        Subtype::Tag(tag) => u32::from(tag) == WAVE_FORMAT_IEEE_FLOAT,
        Subtype::Guid(guid) => guid == KSDATAFORMAT_SUBTYPE_IEEE_FLOAT,
    };
    let integer = match kind {
        Subtype::Tag(tag) => u32::from(tag) == WAVE_FORMAT_PCM,
        Subtype::Guid(guid) => guid == KSDATAFORMAT_SUBTYPE_PCM,
    };

    match (float, integer, bits) {
        (true, _, 32) => Some(SampleFormat::Float32),
        (_, true, 16) => Some(SampleFormat::Int16),
        (_, true, 24) => Some(SampleFormat::Int24),
        (_, true, 32) => Some(SampleFormat::Int32),
        _ => None,
    }
}

/// Whether a device identifier from a notification refers to `id`.
///
/// Windows passes a null pointer when there is no device — the default endpoint
/// changing to "nothing at all" — and comparing that against anything must say
/// no rather than fault.
pub(super) fn identifier_matches(candidate: &PCWSTR, id: &str) -> bool {
    if candidate.is_null() {
        return false;
    }
    // SAFETY: a non-null `PCWSTR` from an `IMMNotificationClient` callback is a
    // null-terminated wide string owned by the caller and valid for the
    // duration of the call, which is where this runs.
    unsafe { candidate.to_string() }.is_ok_and(|candidate| candidate == id)
}

/// Interprets a raw `WAVEFORMATEX` in memory, for tests that need to describe
/// an endpoint this machine does not have.
#[cfg(test)]
fn interpret_bytes(bytes: &[u8]) -> Result<AudioFormat, AudioError> {
    assert!(bytes.len() >= size_of::<WAVEFORMATEX>());
    interpret(bytes.as_ptr().cast())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds the bytes of a `WAVEFORMATEX`, optionally with the extensible
    /// tail, exactly as Windows lays them out.
    fn wave_format(
        tag: u16,
        channels: u16,
        rate: u32,
        bits: u16,
        extensible: Option<(u16, u32, GUID)>,
    ) -> Vec<u8> {
        let block = channels * bits / 8;
        let mut bytes = Vec::new();
        bytes.extend(tag.to_le_bytes());
        bytes.extend(channels.to_le_bytes());
        bytes.extend(rate.to_le_bytes());
        bytes.extend((rate * u32::from(block)).to_le_bytes());
        bytes.extend(block.to_le_bytes());
        bytes.extend(bits.to_le_bytes());
        match extensible {
            None => bytes.extend(0u16.to_le_bytes()),
            Some((valid, mask, subformat)) => {
                bytes.extend(22u16.to_le_bytes());
                bytes.extend(valid.to_le_bytes());
                bytes.extend(mask.to_le_bytes());
                // A GUID is not sixteen big-endian bytes in memory: the first
                // three fields are little-endian integers and only the last
                // eight bytes are in the order they are printed in.
                bytes.extend(subformat.data1.to_le_bytes());
                bytes.extend(subformat.data2.to_le_bytes());
                bytes.extend(subformat.data3.to_le_bytes());
                bytes.extend(subformat.data4);
            }
        }
        bytes
    }

    #[test]
    fn the_mix_format_windows_actually_reports_here_is_interpreted_correctly() {
        // Measured on Windows 11 build 26200: tag 0xfffe, 2 channels, 48 kHz,
        // 32 container bits, 32 valid bits, mask 0x3, subformat
        // KSDATAFORMAT_SUBTYPE_IEEE_FLOAT.
        let bytes = wave_format(
            WAVE_FORMAT_EXTENSIBLE as u16,
            2,
            48_000,
            32,
            Some((32, 0x3, KSDATAFORMAT_SUBTYPE_IEEE_FLOAT)),
        );
        let format = interpret_bytes(&bytes).expect("this is the ordinary Windows mix format");

        assert_eq!(format.sample_rate().get(), 48_000);
        assert_eq!(format.channels().get(), 2);
        assert_eq!(format.channel_mask().bits(), 0x3);
        assert_eq!(format.endpoint_samples(), SampleFormat::Float32);
    }

    #[test]
    fn an_endpoint_that_is_not_forty_eight_kilohertz_stereo_float_is_still_handled() {
        // The assumption this test exists to break. A 44.1 kHz 5.1 endpoint
        // presenting 24 valid bits in a 32-bit container is unusual and
        // entirely legitimate.
        let bytes = wave_format(
            WAVE_FORMAT_EXTENSIBLE as u16,
            6,
            44_100,
            32,
            Some((24, 0x3f, KSDATAFORMAT_SUBTYPE_PCM)),
        );
        let format = interpret_bytes(&bytes).expect("a 24-in-32 endpoint is supported");

        assert_eq!(format.sample_rate().get(), 44_100);
        assert_eq!(format.channels().get(), 6);
        assert_eq!(format.endpoint_samples(), SampleFormat::Int32);
        assert_eq!(
            format.channel_mask().to_string(),
            "front left, front right, front centre, low frequency, back left, back right"
        );
    }

    #[test]
    fn a_plain_wave_format_without_the_extensible_tail_is_interpreted_too() {
        let bytes = wave_format(WAVE_FORMAT_PCM as u16, 2, 48_000, 16, None);
        let format = interpret_bytes(&bytes).expect("plain 16-bit PCM is supported");
        assert_eq!(format.endpoint_samples(), SampleFormat::Int16);
        assert_eq!(
            format.channel_mask().to_string(),
            "unspecified",
            "a format with no extensible part says nothing about speaker positions, and \
             inventing an answer is how a surround recording ends up mislabelled"
        );
    }

    #[test]
    fn a_format_this_crate_cannot_convert_is_refused_with_its_numbers_in_the_message() {
        // 8-bit PCM: representable in a `WAVEFORMATEX` and not something this
        // crate converts. Reinterpreting it as 16-bit would produce full-scale
        // noise rather than quiet audio, so it is refused.
        let bytes = wave_format(WAVE_FORMAT_PCM as u16, 2, 48_000, 8, None);
        let error = interpret_bytes(&bytes).expect_err("8-bit PCM is not supported");
        let message = error.to_string();
        assert!(message.contains("8 bits per sample"), "{message}");
        assert!(matches!(error, AudioError::UnsupportedFormat { .. }));
    }

    #[test]
    fn a_block_that_does_not_match_the_channels_and_width_is_refused() {
        // Interleaving is assumed from the channel count, so a block size that
        // contradicts it would silently deinterleave into noise.
        let mut bytes = wave_format(WAVE_FORMAT_PCM as u16, 2, 48_000, 16, None);
        bytes[12] = 6; // nBlockAlign, which should be 4 for stereo 16-bit.
        let error = interpret_bytes(&bytes).expect_err("an inconsistent block size is refused");
        assert!(error.to_string().contains("6 bytes"), "{error}");
    }

    #[test]
    fn a_null_device_identifier_from_a_notification_matches_nothing() {
        // Windows passes null when the default endpoint has become "none at
        // all", and that must not be read as "the device I am capturing".
        assert!(!identifier_matches(
            &PCWSTR::null(),
            "{0.0.0.00000000}.{abc}"
        ));
    }
}

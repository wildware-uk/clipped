//! Finding the default render endpoint, and reading what shape its audio is.

use core::num::{NonZeroU16, NonZeroU32};

use windows::core::{GUID, HRESULT, PCWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::ERROR_NOT_FOUND;
use windows::Win32::Media::Audio::{
    eConsole, eRender, IAudioClient, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator,
    WAVEFORMATEX, WAVEFORMATEXTENSIBLE, WAVE_FORMAT_PCM,
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

/// The default render endpoint, or [`None`] if the machine has none.
///
/// "None" is a state to report rather than an error to raise: a laptop with
/// every output unplugged genuinely has no default render endpoint, and so does
/// a machine part-way through installing a driver. The caller decides whether
/// that ends an attempt to start recording or is something to keep waiting
/// through.
pub(super) fn default_render_endpoint(
    enumerator: &IMMDeviceEnumerator,
) -> Result<Option<IMMDevice>, AudioError> {
    // `eConsole` is the role Windows plays games and media through, which is
    // the audio a recording is about. `eCommunications` — which a headset may
    // be default for while speakers stay default for everything else — belongs
    // to microphone capture and voice chat
    // (issue #20, https://github.com/wildware-uk/clipped/issues/20).
    //
    // SAFETY: `enumerator` is a live `IMMDeviceEnumerator`, and both arguments
    // are values of the enumerations the signature names.
    match unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) } {
        Ok(device) => Ok(Some(device)),
        // The documented answer when there is no endpoint for the role, rather
        // than a failure of the call.
        Err(error) if error.code() == E_NOTFOUND => Ok(None),
        Err(error) => Err(platform_error(
            "asking Windows for the default audio output device",
            error,
        )),
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

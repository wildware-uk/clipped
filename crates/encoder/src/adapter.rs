//! Display adapters, as DXGI describes them.
//!
//! Everything here is measured: DXGI is asked and this is what it answered. The
//! one exception is [`AdapterKind`], which DXGI does not report and which is
//! derived from the memory figures by a rule stated on the type itself.

use core::fmt;

use serde::{Deserialize, Serialize};

use crate::codec::Vendor;

/// The locally unique identifier Windows gives an adapter.
///
/// Stable for as long as the adapter is present, which is what makes it usable
/// in a cache key and in a log line. It is how one adapter is told from
/// another; it is not evidence about which adapter an encoder runs on, because
/// nothing this crate measures says that (see `crate::detection`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AdapterId(u64);

impl AdapterId {
    /// Builds an identifier from the two halves of a Windows `LUID`.
    #[must_use]
    pub const fn from_luid(low: u32, high: i32) -> Self {
        Self(((high as u32 as u64) << 32) | low as u64)
    }
}

impl fmt::Display for AdapterId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{:016x}", self.0)
    }
}

/// A driver's user-mode version, as `IDXGIAdapter::CheckInterfaceSupport`
/// returns it.
///
/// This is the value the capability cache is keyed on. A driver update changes
/// it, and a driver update is exactly the event that can add a codec — AV1
/// encoding arrived on hardware that had shipped months earlier — or take one
/// away, so a cache that ignored it would go on reporting yesterday's answer
/// for as long as the machine kept its GPU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct DriverVersion(u64);

impl DriverVersion {
    /// Wraps the packed 64-bit version Windows reports.
    #[must_use]
    pub const fn from_raw(raw: u64) -> Self {
        Self(raw)
    }

    /// The four parts, most significant first, as a driver is normally quoted:
    /// `32.0.15.7688`.
    #[must_use]
    pub const fn parts(self) -> [u16; 4] {
        [
            (self.0 >> 48) as u16,
            (self.0 >> 32) as u16,
            (self.0 >> 16) as u16,
            self.0 as u16,
        ]
    }
}

impl fmt::Display for DriverVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let [a, b, c, d] = self.parts();
        write!(formatter, "{a}.{b}.{c}.{d}")
    }
}

/// What sort of adapter this is, in the only terms DXGI can actually support.
///
/// It would be more useful to say "discrete" and "integrated", and this
/// deliberately does not, because DXGI cannot tell you that and pretending
/// otherwise gets it wrong. What DXGI reports is how much video memory an
/// adapter has of its own, and an AMD APU with a BIOS memory carve-out reports
/// gigabytes of it while being as integrated as a GPU can be — the machine this
/// was written on does exactly that. So the words here are the measurement:
/// memory of its own, or memory shared with the CPU.
///
/// The one genuinely inferred part is [`Software`](Self::Software), which comes
/// from the flag Windows sets on a rasteriser and from Microsoft's own PCI
/// vendor identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterKind {
    /// The adapter has video memory of its own. Every discrete card, and the
    /// integrated parts that were given a carve-out.
    OwnVideoMemory,
    /// The adapter has none, and shares system memory with the CPU.
    SharedVideoMemory,
    /// Not silicon: the Microsoft Basic Render Driver or WARP.
    Software,
}

impl AdapterKind {
    /// How this is written in a structured log field: stable, snake_case and
    /// searchable, unlike [`Display`](fmt::Display), which is the label a user
    /// reads.
    #[must_use]
    pub const fn log_value(self) -> &'static str {
        match self {
            Self::OwnVideoMemory => "own_video_memory",
            Self::SharedVideoMemory => "shared_video_memory",
            Self::Software => "software",
        }
    }
}

impl fmt::Display for AdapterKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::OwnVideoMemory => "own video memory",
            Self::SharedVideoMemory => "shared system memory",
            Self::Software => "software rasteriser",
        })
    }
}

/// One display adapter.
///
/// The description is the model name the driver publishes — "NVIDIA GeForce RTX
/// 4090" — which is hardware, not user content, and is the single most useful
/// thing in a bug report about encoding. AGENTS.md section 14 keeps window
/// titles, file paths and account names out of the logs; a GPU model is none of
/// those.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adapter {
    id: AdapterId,
    description: String,
    vendor: Vendor,
    device_id: u32,
    kind: AdapterKind,
    dedicated_video_memory: u64,
    driver_version: Option<DriverVersion>,
}

impl Adapter {
    /// Describes an adapter.
    ///
    /// `dedicated_video_memory` is in bytes and decides
    /// [`kind`](Self::kind) together with `software`, following the rule on
    /// [`AdapterKind`].
    #[must_use]
    pub fn new(
        id: AdapterId,
        description: impl Into<String>,
        vendor: Vendor,
        device_id: u32,
        dedicated_video_memory: u64,
        software: bool,
    ) -> Self {
        let kind = if software || vendor == Vendor::Microsoft {
            AdapterKind::Software
        } else if dedicated_video_memory == 0 {
            AdapterKind::SharedVideoMemory
        } else {
            AdapterKind::OwnVideoMemory
        };

        Self {
            id,
            description: description.into(),
            vendor,
            device_id,
            kind,
            dedicated_video_memory,
            driver_version: None,
        }
    }

    /// Records the driver version, where the adapter reported one.
    ///
    /// Not every adapter does: `CheckInterfaceSupport` answers for Direct3D 10
    /// and later drivers and returns "unsupported" otherwise, and a missing
    /// version is reported as missing rather than as zero, because the cache
    /// key has to be able to say "this machine does not tell us".
    #[must_use]
    pub const fn with_driver_version(mut self, version: Option<DriverVersion>) -> Self {
        self.driver_version = version;
        self
    }

    /// The adapter's locally unique identifier.
    #[must_use]
    pub const fn id(&self) -> AdapterId {
        self.id
    }

    /// The model name the driver publishes.
    #[must_use]
    pub fn description(&self) -> &str {
        &self.description
    }

    /// Which vendor made it.
    #[must_use]
    pub const fn vendor(&self) -> Vendor {
        self.vendor
    }

    /// The PCI device identifier, which distinguishes models within a vendor.
    #[must_use]
    pub const fn device_id(&self) -> u32 {
        self.device_id
    }

    /// Whether it has video memory of its own; see [`AdapterKind`].
    #[must_use]
    pub const fn kind(&self) -> AdapterKind {
        self.kind
    }

    /// Video memory of the adapter's own, in bytes.
    #[must_use]
    pub const fn dedicated_video_memory(&self) -> u64 {
        self.dedicated_video_memory
    }

    /// The user-mode driver version, where the adapter reported one.
    #[must_use]
    pub const fn driver_version(&self) -> Option<DriverVersion> {
        self.driver_version
    }

    /// Whether this adapter can host a hardware encoder at all.
    ///
    /// A software rasteriser cannot, and reporting NVENC as unavailable because
    /// the only adapter present is the Basic Render Driver is a much more
    /// useful answer than reporting it as unavailable with no reason.
    #[must_use]
    pub const fn can_host_hardware_encoder(&self) -> bool {
        !matches!(self.kind, AdapterKind::Software)
    }
}

/// The adapter a vendor's hardware encoder is attributed to, if any.
///
/// Always a guess, and worth saying so plainly: nothing this crate measures
/// says which GPU a hardware encoder belongs to. Media Foundation documents
/// `MFT_ENUM_ADAPTER_LUID` as an input filter for `MFTEnum2` rather than as an
/// attribute on an activation object, and it is absent from every transform on
/// the machine this was written on, so attribution is the vendor's own adapter,
/// preferring the one with the most video memory of its own. That is right
/// whenever a machine has one card per vendor, and a machine with two cards
/// from one vendor and only one of them encoding is beyond what this can tell.
/// The report prints which adapter it picked rather than implying it knew.
///
/// One function because two callers need the same answer: the report says which
/// adapter an encoder is on, and the capability probe creates its Direct3D
/// device on that adapter. A probe that picked differently would measure one
/// GPU and file the answer under another.
#[must_use]
pub(crate) fn encoding_adapter(adapters: &[Adapter], vendor: Vendor) -> Option<&Adapter> {
    adapters
        .iter()
        .filter(|adapter| adapter.vendor() == vendor && adapter.can_host_hardware_encoder())
        .max_by_key(|adapter| adapter.dedicated_video_memory())
}

/// The adapter a recording's frames will be captured on.
///
/// **This is inferred, not measured, and the inference is documented rather than
/// guessed.** `crates/capture/src/windows/device.rs` creates its Direct3D 11
/// device by calling `D3D11CreateDevice` with no adapter and
/// `D3D_DRIVER_TYPE_HARDWARE`, and Microsoft documents that argument as: pass
/// `NULL` "to use the default adapter, which is the first adapter that is
/// enumerated by `IDXGIFactory1::EnumAdapters`". `crate::windows::dxgi::adapters`
/// enumerates with that same call and keeps its order, so the first entry that
/// could host a hardware encoder is the adapter capture will land on.
///
/// Why the answer matters at all: an encoder backend is handed the device
/// *capture* created, and every vendor runtime here refuses a device belonging
/// to another vendor's adapter — AMF answers `AMF_INVALID_ARG` from
/// `AMFContext::InitDX11`, and Quick Sync and NVENC have their own refusals. So
/// on a machine with two vendors' adapters, an encoder that is present, whose
/// runtime loads and whose transforms Windows lists can still be one no
/// recording can use ([issue #443](https://github.com/wildware-uk/clipped/issues/443)).
///
/// Software rasterisers are skipped for the same reason they are skipped
/// everywhere else here: `D3D_DRIVER_TYPE_HARDWARE` will not create a device on
/// one, so capture cannot be there either.
///
/// [`crate::windows::dxgi`] holds the test that checks this inference against
/// the machine it runs on, by creating the device capture creates and asking
/// which adapter it landed on.
#[must_use]
pub(crate) fn capture_adapter(adapters: &[Adapter]) -> Option<&Adapter> {
    adapters
        .iter()
        .find(|adapter| adapter.can_host_hardware_encoder())
}

impl fmt::Display for Adapter {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{} ({})", self.description, self.kind)?;
        match self.driver_version {
            Some(version) => write!(formatter, ", driver {version}"),
            None => formatter.write_str(", driver version not reported"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn adapter(vendor: Vendor, memory: u64, software: bool) -> Adapter {
        Adapter::new(
            AdapterId::from_luid(1, 0),
            "Test Adapter",
            vendor,
            0x2684,
            memory,
            software,
        )
    }

    #[test]
    fn the_kind_is_whether_the_adapter_has_video_memory_of_its_own() {
        assert_eq!(
            adapter(Vendor::Nvidia, 24 * 1024 * 1024 * 1024, false).kind(),
            AdapterKind::OwnVideoMemory
        );
        assert_eq!(
            adapter(Vendor::Amd, 0, false).kind(),
            AdapterKind::SharedVideoMemory
        );
    }

    #[test]
    fn an_integrated_gpu_with_a_memory_carve_out_is_not_called_a_card() {
        // The case that made this enumeration stop saying "discrete": the AMD
        // part in the machine this was written on is integrated and reports two
        // gigabytes of dedicated video memory, so any rule of the form
        // "dedicated memory means discrete" calls it a graphics card. The
        // enumeration therefore says what was measured — it has memory of its
        // own — and leaves the word "discrete" to nobody.
        let carve_out = adapter(Vendor::Amd, 2 * 1024 * 1024 * 1024, false);
        assert_eq!(carve_out.kind(), AdapterKind::OwnVideoMemory);
        assert!(
            !carve_out.to_string().contains("discrete"),
            "the report must not claim to know what it cannot: {carve_out}"
        );
    }

    #[test]
    fn a_software_rasteriser_cannot_host_a_hardware_encoder() {
        let flagged = adapter(Vendor::Other(0x1234), 0, true);
        assert_eq!(flagged.kind(), AdapterKind::Software);
        assert!(!flagged.can_host_hardware_encoder());

        // The Basic Render Driver does not always set the flag, so its vendor
        // identifier is treated as the same answer.
        let basic_render_driver = adapter(Vendor::Microsoft, 0, false);
        assert_eq!(basic_render_driver.kind(), AdapterKind::Software);
        assert!(!basic_render_driver.can_host_hardware_encoder());
    }

    #[test]
    fn a_driver_version_prints_the_way_a_driver_is_quoted() {
        // 32.0.15.7688 packed the way Windows returns it. The second part is
        // zero, which is exactly the case a version that dropped a field would
        // still pass, so it is written out rather than left implicit.
        let raw = (32_u64 << 48) | (15_u64 << 16) | 7688_u64;
        let version = DriverVersion::from_raw(raw);
        assert_eq!(version.parts(), [32, 0, 15, 7688]);
        assert_eq!(version.to_string(), "32.0.15.7688");

        // And one with every part set, so that a mask which happened to work
        // for zeroes does not pass here.
        let full = DriverVersion::from_raw((1_u64 << 48) | (2_u64 << 32) | (3_u64 << 16) | 4);
        assert_eq!(full.to_string(), "1.2.3.4");
    }

    #[test]
    fn a_missing_driver_version_is_reported_as_missing_rather_than_as_zero() {
        let adapter = adapter(Vendor::Nvidia, 1, false);
        assert_eq!(adapter.driver_version(), None);
        assert!(adapter.to_string().contains("driver version not reported"));
    }

    #[test]
    fn an_adapter_identifier_is_the_two_halves_of_a_luid() {
        // The high half is signed and negative here, which is the case a
        // sign-extending shift would get wrong: it must occupy the top 32 bits
        // and leave the low half alone.
        let id = AdapterId::from_luid(0x0000_1234, -1);
        assert_eq!(id.to_string(), "ffffffff00001234");
        assert_eq!(
            AdapterId::from_luid(0x0000_1234, 0).to_string(),
            "0000000000001234"
        );
    }
}

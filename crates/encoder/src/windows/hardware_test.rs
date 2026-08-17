//! Shared scaffolding for the hardware tests of the vendor encoder backends.
//!
//! NVENC and AMF each open a real Direct3D 11 device on their vendor's
//! adapter, feed it real textures, and check the bitstream that comes back.
//! Before this module existed, both `windows::nvenc::tests` and
//! `windows::amf::tests` carried their own copy of that machinery — about 250
//! lines each, drifting the moment one of them changed and the other did not
//! (see [#166](https://github.com/wildware-uk/clipped/issues/166)). This is
//! that machinery, lifted out once and parameterised by the vendor whose
//! adapter to open and the size to encode at.
//!
//! # What stayed behind
//!
//! Only the parts that do not know what encoder they are talking to. Opening
//! a session — `NvencEncoder::open` versus `AmfEncoder::open` — returns a
//! different concrete type per backend, so each backend keeps its own
//! one-line `open_encoder` wrapper rather than this module trying to be
//! generic over it. The body of `encode_and_verify` also stayed behind in
//! both callers, deliberately: AMF checks a packet's reported keyframe flag
//! against the NAL unit types in the bitstream and a timestamp rounded to
//! AMF's own tick, neither of which NVENC's copy does, and folding the
//! scaffolding must not average away what one backend asserts that the other
//! does not (AGENTS.md section 55, and issue #166's own instructions).
//!
//! # What did not move
//!
//! `crate::software`'s tests build a similar `TestGpu`, but for a different
//! job: it falls back to WARP rather than searching for a vendor's adapter,
//! because the software encoder needs no encoding hardware at all, and that
//! is a real behavioural difference rather than incidental duplication. It is
//! also outside `crate::windows`, so folding it in here would be the kind of
//! unrelated refactor AGENTS.md section 40 asks agents not to go looking for.
//! `crates/encoder/src/software/tests.rs` keeps its own copy.
//!
//! # Why a skip is not a pass
//!
//! Every hardware test using this module runs on whatever silicon the
//! machine has, which is not every machine and is nothing on a hosted CI
//! runner. [`skipped`] says why a test did not run, and setting
//! `CLIPPED_REQUIRE_ENCODER=1` turns that skip into a failure, so "the
//! encoder tests passed" cannot quietly mean "the encoder tests did nothing".
//! [`unsupported_here`] is the other kind of absence — a codec this specific
//! card cannot encode — and stays a skip even under that lever, because which
//! codecs a card offers is a fact about the silicon and not about the
//! checkout.

use core::ffi::c_void;
use core::time::Duration;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard};

use windows::core::Interface as _;
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
    D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
    D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};

use crate::codec::{Codec, Resolution, Vendor};
use crate::frame::{DeviceKind, GraphicsDevice};

// ---------------------------------------------------------------------------
// Skipping, and the lever that turns a skip into a failure
// ---------------------------------------------------------------------------

/// The environment variable that turns "this machine could not run the test"
/// from a pass into a failure.
pub(in crate::windows) const REQUIRE_ENCODER: &str = "CLIPPED_REQUIRE_ENCODER";

fn env_is_set(name: &str) -> bool {
    std::env::var_os(name).is_some_and(|value| !value.is_empty())
}

/// Reports that a test could not run here, and returns whether the caller
/// should give up.
///
/// Panics instead of skipping when `CLIPPED_REQUIRE_ENCODER` is set, so a
/// machine that is meant to exercise the encoder cannot quietly stop doing it.
/// Writes through `std::io::stderr()` rather than `eprintln!` because libtest
/// captures the macro: a skip printed with `eprintln!` is invisible in a
/// passing run, which is exactly the failure mode this guards against.
pub(in crate::windows) fn skipped(reason: &str) -> bool {
    assert!(
        !env_is_set(REQUIRE_ENCODER),
        "{REQUIRE_ENCODER} is set, so this must not be skipped: {reason}"
    );
    let _ = writeln!(std::io::stderr(), "SKIPPED (encoder): {reason}");
    true
}

/// Reports a codec a card cannot encode.
///
/// Not a failure even under [`REQUIRE_ENCODER`]: which codecs a card offers is
/// a property of the silicon — AV1 encoding arrived with NVIDIA's Ada
/// generation, and AMD and Intel each have limits of their own — and the same
/// answer reaches a user through `recorder capabilities`. Everything the
/// machine *can* encode is still checked.
pub(in crate::windows) fn unsupported_here(reason: &str) -> bool {
    let _ = writeln!(std::io::stderr(), "SKIPPED (encoder, hardware): {reason}");
    true
}

// ---------------------------------------------------------------------------
// The device and the textures fed through it
// ---------------------------------------------------------------------------

/// A Direct3D 11 device on one vendor's adapter, and the textures the tests
/// feed through it.
///
/// # Ownership
///
/// Owns the device and every texture it creates; both are reference-counted
/// COM interfaces released when this is dropped, which happens after the
/// encoder under test — the encoder is created from it and dropped first in
/// every test.
///
/// It also holds a `SESSIONS` mutex, supplied by the caller, for its whole
/// life, which is what serialises the tests that need the hardware. Each
/// backend keeps its own static rather than sharing one here: how many
/// concurrent sessions a card allows is a property of that vendor's driver,
/// and `crate::windows::tests` locks both of them in a fixed order to measure
/// the two backends' limits in the same run, so collapsing them into one
/// mutex would serialise NVENC hardware tests against AMF ones that touch
/// entirely different silicon.
pub(in crate::windows) struct TestGpu {
    device: ID3D11Device,
    resolution: Resolution,
    _sessions: MutexGuard<'static, ()>,
}

impl TestGpu {
    /// Opens a Direct3D 11 device on `vendor`'s adapter, holding `sessions`
    /// for as long as the result is alive.
    ///
    /// Returns [`None`] on a machine with no adapter from that vendor, unless
    /// `CLIPPED_REQUIRE_ENCODER` is set, in which case the absence is a
    /// failure. That is the lever that stops "the encoder tests passed" from
    /// quietly meaning "the encoder tests did nothing".
    pub(in crate::windows) fn open(
        vendor: Vendor,
        resolution: Resolution,
        sessions: &'static Mutex<()>,
    ) -> Option<Self> {
        match Self::try_open(vendor, resolution, sessions) {
            Ok(gpu) => Some(gpu),
            Err(reason) => {
                skipped(&reason);
                None
            }
        }
    }

    fn try_open(
        vendor: Vendor,
        resolution: Resolution,
        sessions: &'static Mutex<()>,
    ) -> Result<Self, String> {
        // A test that panicked while encoding poisoned the lock; the hardware
        // is no worse for it, and refusing to run every later test because an
        // earlier one failed would hide the rest of the suite behind the
        // first failure.
        let sessions = sessions.lock().unwrap_or_else(|held| held.into_inner());

        let adapter = vendor_adapter(vendor)?;
        let mut device: Option<ID3D11Device> = None;

        // SAFETY: `adapter` is a live DXGI adapter, the driver type must be
        // UNKNOWN when an adapter is named, the module handle is unused for
        // that driver type, the feature level list and the out-parameters are
        // live locals, and `D3D11_SDK_VERSION` is the constant the header
        // requires. On success `device` holds one reference, released on
        // drop.
        unsafe {
            D3D11CreateDevice(
                &adapter,
                D3D_DRIVER_TYPE_UNKNOWN,
                windows::Win32::Foundation::HMODULE::default(),
                D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                Some(&[D3D_FEATURE_LEVEL_11_0]),
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                None,
            )
        }
        .map_err(|error| format!("no Direct3D 11 device on the {vendor} adapter: {error}"))?;

        device
            .map(|device| Self {
                device,
                resolution,
                _sessions: sessions,
            })
            .ok_or_else(|| "D3D11CreateDevice reported success without a device".to_owned())
    }

    /// This device, as the crate's own borrowed handle.
    pub(in crate::windows) fn graphics_device(&self) -> GraphicsDevice {
        // SAFETY: the device is alive for as long as `self` is, and
        // everything opened from it is dropped inside the test that opened
        // it.
        unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.device.as_raw()) }
    }

    /// A texture holding a moving pattern, so that successive frames differ
    /// and the encoder has something to predict.
    pub(in crate::windows) fn pattern_texture(&self, index: usize) -> ID3D11Texture2D {
        let width = self.resolution.width as usize;
        let height = self.resolution.height as usize;
        let mut pixels = vec![0u8; width * height * 4];
        let offset = (index * 11) % width;

        for y in 0..height {
            for x in 0..width {
                let at = (y * width + x) * 4;
                let bar = (x + offset) % 128 < 64;
                // BGRA, which is what `DXGI_FORMAT_B8G8R8A8_UNORM` stores.
                pixels[at] = if bar { 200 } else { 20 };
                pixels[at + 1] = ((y * 255) / height) as u8;
                pixels[at + 2] = ((x * 255) / width) as u8;
                pixels[at + 3] = 255;
            }
        }

        self.texture_from(&pixels)
    }

    /// A texture of one solid colour, given as red, green, blue.
    pub(in crate::windows) fn solid_texture(&self, colour: [u8; 3]) -> ID3D11Texture2D {
        self.texture_from(&solid_pixels(colour, self.resolution))
    }

    /// Overwrites a texture in place, the way a capture frame pool overwrites
    /// a recycled surface.
    pub(in crate::windows) fn overwrite(&self, texture: &ID3D11Texture2D, colour: [u8; 3]) {
        let pixels = solid_pixels(colour, self.resolution);

        // SAFETY: the device is live and the immediate context it hands back
        // is released when the local goes out of scope.
        let context = unsafe { self.device.GetImmediateContext() }
            .expect("a device has an immediate context");

        // SAFETY: `texture` was created by this device with `MipLevels: 1`
        // and `ArraySize: 1`, so subresource 0 is the whole of it; the box is
        // null, meaning the whole resource; and `pixels` holds
        // `Width * Height * 4` bytes, which is what the row pitch declares.
        unsafe {
            context.UpdateSubresource(
                texture,
                0,
                None,
                pixels.as_ptr().cast::<c_void>(),
                self.resolution.width * 4,
                0,
            );
            // Make the write reach the GPU now rather than whenever the
            // driver next flushes: the point of the test is that the encoder
            // has finished with the surface before this happens.
            context.Flush();
        }
    }

    /// Uploads pixels into a texture the vendor's encoder can bind.
    fn texture_from(&self, pixels: &[u8]) -> ID3D11Texture2D {
        let description = D3D11_TEXTURE2D_DESC {
            Width: self.resolution.width,
            Height: self.resolution.height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC {
                Count: 1,
                Quality: 0,
            },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let initial = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast::<c_void>(),
            SysMemPitch: self.resolution.width * 4,
            SysMemSlicePitch: 0,
        };

        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: the description and the initial data are live locals, the
        // pixel buffer is `Width * Height * 4` bytes as `SysMemPitch`
        // declares, and `texture` is a live out-parameter.
        unsafe {
            self.device
                .CreateTexture2D(&description, Some(&initial), Some(&mut texture))
        }
        .expect("a texture can be created");

        texture.expect("CreateTexture2D reported success without a texture")
    }
}

/// One solid colour, given as red, green, blue, as the BGRA bytes
/// `DXGI_FORMAT_B8G8R8A8_UNORM` stores.
fn solid_pixels(colour: [u8; 3], resolution: Resolution) -> Vec<u8> {
    let mut pixels = vec![0u8; (resolution.width as usize) * (resolution.height as usize) * 4];
    for pixel in pixels.chunks_exact_mut(4) {
        pixel[0] = colour[2];
        pixel[1] = colour[1];
        pixel[2] = colour[0];
        pixel[3] = 255;
    }
    pixels
}

/// The first adapter from `vendor` in the machine.
fn vendor_adapter(vendor: Vendor) -> Result<IDXGIAdapter, String> {
    // SAFETY: the factory is created and released by this function and its
    // caller; nothing outlives it.
    let factory: IDXGIFactory1 =
        unsafe { CreateDXGIFactory1() }.map_err(|error| format!("no DXGI factory: {error}"))?;

    for index in 0.. {
        // SAFETY: enumeration stops at the first failure, which is how DXGI
        // reports the end of the list.
        let Ok(adapter) = (unsafe { factory.EnumAdapters(index) }) else {
            break;
        };
        // SAFETY: `adapter` is live and `GetDesc` fills a live local.
        let Ok(description) = (unsafe { adapter.GetDesc() }) else {
            continue;
        };
        if Vendor::from_pci_id(description.VendorId) == vendor {
            return Ok(adapter);
        }
    }

    Err(format!("there is no {vendor} adapter in this machine"))
}

// ---------------------------------------------------------------------------
// Development tools and temporary files
// ---------------------------------------------------------------------------

/// Looks for a development tool, beside the pinned FFmpeg first and then on
/// the path.
///
/// `FFMPEG_DIR` is set for every process Cargo runs by `.cargo/config.toml`,
/// so a checkout that has run `scripts/fetch-ffmpeg.ps1` — which is every
/// checkout that can build `clipped-muxer` — has `ffprobe.exe` and
/// `ffmpeg.exe` here without anybody adding them to `PATH`.
pub(in crate::windows) fn tool(name: &str) -> Option<PathBuf> {
    let file = format!("{name}{}", if cfg!(windows) { ".exe" } else { "" });

    let bundled = std::env::var_os("FFMPEG_DIR")
        .map(|directory| PathBuf::from(directory).join("bin").join(&file))
        .filter(|candidate| candidate.is_file());
    if bundled.is_some() {
        return bundled;
    }

    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|directory| directory.join(&file))
            .find(|candidate| candidate.is_file())
    })
}

/// A file in the temporary directory that deletes itself.
///
/// The tests write real bitstreams to disk so that `ffprobe` can read them,
/// and leaving them behind would fill a developer's temporary directory a
/// megabyte at a time.
pub(in crate::windows) struct TempFile {
    path: PathBuf,
}

impl TempFile {
    pub(in crate::windows) fn new(prefix: &str, extension: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_nanos());
        let path = std::env::temp_dir().join(format!(
            "{prefix}-{}-{unique}.{extension}",
            std::process::id()
        ));
        Self { path }
    }

    pub(in crate::windows) fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// The file extension that tells `ffprobe` what an elementary stream is.
pub(in crate::windows) const fn extension(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "obu",
    }
}

// ---------------------------------------------------------------------------
// Checking what came out
// ---------------------------------------------------------------------------

/// Splits an Annex B stream into its NAL units.
pub(in crate::windows) fn annex_b_units(stream: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut index = 0;
    while index + 3 < stream.len() {
        if stream[index] == 0 && stream[index + 1] == 0 && stream[index + 2] == 1 {
            starts.push(index + 3);
            index += 3;
        } else if index + 4 < stream.len()
            && stream[index] == 0
            && stream[index + 1] == 0
            && stream[index + 2] == 0
            && stream[index + 3] == 1
        {
            starts.push(index + 4);
            index += 4;
        } else {
            index += 1;
        }
    }

    starts
        .iter()
        .enumerate()
        .map(|(position, start)| {
            let end = starts.get(position + 1).map_or(stream.len(), |next| *next);
            &stream[*start..end]
        })
        .filter(|unit| !unit.is_empty())
        .collect()
}

/// Parses the bitstream far enough to know it is the codec it claims to be.
///
/// Deliberately not a decode: this runs on every machine with the relevant
/// vendor's GPU, including ones with no FFmpeg, and a stream with no
/// parameter sets or no keyframe is broken whatever a decoder would say about
/// it. AMF has no AV1 backend (issue #165), so only NVENC's tests reach the
/// AV1 arm; keeping it here rather than dropping it means an AMF AV1 backend
/// would be checked correctly from the day it exists, instead of silently
/// being parsed as Annex B.
pub(in crate::windows) fn check_structure(codec: Codec, stream: &[u8]) {
    match codec {
        Codec::H264 | Codec::Hevc => {
            let units = annex_b_units(stream);
            assert!(
                units.len() >= 3,
                "{codec} produced {} NAL units, which is not a stream",
                units.len()
            );

            let types: Vec<u8> = units
                .iter()
                .map(|unit| match codec {
                    // H.264: five bits of type in the low bits of one header
                    // byte. HEVC: six bits, one bit up, in a two-byte header.
                    Codec::H264 => unit[0] & 0x1F,
                    _ => (unit[0] >> 1) & 0x3F,
                })
                .collect();

            let (sequence_header, keyframe): (&[u8], &[u8]) = match codec {
                Codec::H264 => (&[7], &[5]),
                _ => (&[33], &[19, 20]),
            };
            assert!(
                types.iter().any(|kind| sequence_header.contains(kind)),
                "{codec} produced no sequence parameter set, so no decoder can start"
            );
            assert!(
                types.iter().any(|kind| keyframe.contains(kind)),
                "{codec} produced no keyframe"
            );
        }
        Codec::Av1 => {
            // The low-overhead OBU stream AV1 uses: every unit starts with a
            // header byte whose top bit is zero and whose type is in bits
            // 3-6.
            assert!(stream.len() > 16, "AV1 produced {} bytes", stream.len());
            assert_eq!(
                stream[0] & 0x80,
                0,
                "the first OBU header has its forbidden bit set, so this is not an OBU stream"
            );
            let first_type = (stream[0] >> 3) & 0x0F;
            assert!(
                // A temporal delimiter (2) or a sequence header (1).
                first_type == 1 || first_type == 2,
                "an AV1 stream starts with a temporal delimiter or a sequence header, not OBU \
                 type {first_type}"
            );
        }
    }
}

/// Hands the file to `ffprobe` and asserts on what it says, when `ffprobe` is
/// on the path.
pub(in crate::windows) fn probe(codec: Codec, resolution: Resolution, frames: usize, path: &Path) {
    let Some(ffprobe) = tool("ffprobe") else {
        // The in-process parse checks a NAL type; it does not check that
        // anything can decode the stream, which is the acceptance criterion
        // for the NVENC and AMF backends (issues #15 and #16). So this is a
        // skip of the acceptance criterion itself, not of an extra.
        skipped(
            "ffprobe is not beside the pinned FFmpeg or on the path, so nothing checked that the \
             stream is decodable. Run scripts/fetch-ffmpeg.ps1.",
        );
        return;
    };

    let output = Command::new(ffprobe)
        .args([
            "-v",
            "error",
            "-count_frames",
            "-select_streams",
            "v:0",
            "-show_entries",
            "stream=codec_name,width,height,pix_fmt,nb_read_frames",
            "-of",
            "default=noprint_wrappers=1",
        ])
        .arg(path)
        .output()
        .expect("ffprobe runs");

    let report = String::from_utf8_lossy(&output.stdout);
    eprintln!("ffprobe {}:\n{report}", path.display());
    assert!(
        output.status.success(),
        "ffprobe could not read the stream: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let expected_codec = match codec {
        Codec::H264 => "h264",
        Codec::Hevc => "hevc",
        Codec::Av1 => "av1",
    };
    assert!(
        report.contains(&format!("codec_name={expected_codec}")),
        "ffprobe reports a different codec:\n{report}"
    );
    assert!(
        report.contains(&format!("width={}", resolution.width)),
        "ffprobe reports a different width:\n{report}"
    );
    assert!(
        report.contains(&format!("height={}", resolution.height)),
        "ffprobe reports a different height:\n{report}"
    );
    assert!(
        report.contains("pix_fmt=yuv420p"),
        "ffprobe reports a pixel format this build does not produce:\n{report}"
    );
    assert!(
        report.contains(&format!("nb_read_frames={frames}")),
        "ffprobe counted a different number of frames from the {frames} submitted:\n{report}"
    );
}

/// Decodes an H.264 stream with FFmpeg and returns the middle pixel of each
/// frame as red, green, blue.
///
/// The middle of the picture, away from any edge the codec may have padded.
pub(in crate::windows) fn decode_middle_pixels(
    ffmpeg: &Path,
    stream: &[u8],
    frames: usize,
    resolution: Resolution,
    prefix: &str,
) -> Vec<[u8; 3]> {
    let file = TempFile::new(prefix, "h264");
    std::fs::write(file.path(), stream).expect("the stream can be written");

    let decoded = TempFile::new(prefix, "rgb");
    let status = Command::new(ffmpeg)
        .args(["-y", "-v", "error", "-i"])
        .arg(file.path())
        .args(["-pix_fmt", "rgb24", "-f", "rawvideo"])
        .arg(decoded.path())
        .status()
        .expect("ffmpeg runs");
    assert!(status.success(), "ffmpeg could not decode the stream");

    let raw = std::fs::read(decoded.path()).expect("the decoded frames can be read");
    let frame_bytes = (resolution.width as usize) * (resolution.height as usize) * 3;
    assert_eq!(
        raw.len(),
        frame_bytes * frames,
        "the decoder did not produce one frame per submitted frame"
    );

    (0..frames)
        .map(|index| {
            let pixel = index * frame_bytes
                + ((resolution.height as usize / 2) * resolution.width as usize
                    + resolution.width as usize / 2)
                    * 3;
            [raw[pixel], raw[pixel + 1], raw[pixel + 2]]
        })
        .collect()
}

/// Fails unless a decoded colour is the one that was encoded, allowing for the
/// rounding a trip through 4:2:0 costs.
pub(in crate::windows) fn assert_colour_close(got: [u8; 3], expected: [u8; 3], because: &str) {
    for channel in 0..3 {
        let difference = i32::from(got[channel]) - i32::from(expected[channel]);
        assert!(
            difference.abs() <= 12,
            "decoded {got:?} rather than {expected:?}: {because}"
        );
    }
}

/// Prints the encode latency, which issue #15 asks to be measured.
///
/// Printed rather than asserted on: a threshold here would be a test that
/// fails on somebody else's slower GPU (AGENTS.md section 25). Run with
/// `cargo test -- --nocapture` to see it.
pub(in crate::windows) fn report_latency(
    codec: Codec,
    resolution: Resolution,
    latencies: &[Duration],
    bytes: usize,
) {
    if latencies.is_empty() {
        return;
    }

    let mut sorted: Vec<Duration> = latencies.to_vec();
    sorted.sort_unstable();
    let total: Duration = sorted.iter().sum();
    let mean = total / u32::try_from(sorted.len()).unwrap_or(1);
    let median = sorted[sorted.len() / 2];
    let p95 = sorted[sorted.len() * 95 / 100];
    let worst = *sorted.last().expect("the list is not empty");

    eprintln!(
        "{codec} {resolution}: {} frames, submit-to-packet mean {:.2} ms, median {:.2} ms, \
         p95 {:.2} ms, worst {:.2} ms, {} kB of bitstream",
        sorted.len(),
        mean.as_secs_f64() * 1000.0,
        median.as_secs_f64() * 1000.0,
        p95.as_secs_f64() * 1000.0,
        worst.as_secs_f64() * 1000.0,
        bytes / 1024
    );
}

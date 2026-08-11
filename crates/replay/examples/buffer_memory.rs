//! Measures what a replay buffer costs, by filling one with real encoded
//! video.
//!
//! The memory figures in `docs/replay-buffer.md` and on
//! [issue #35](https://github.com/wildware-uk/clipped/issues/35) came from this
//! program. It is an example rather than a test because it needs an NVIDIA GPU,
//! takes minutes at the longer durations and allocates gigabytes at the longest
//! — none of which belongs in `cargo test`.
//!
//! # What it does
//!
//! Opens a Direct3D 11 device on the NVIDIA adapter, opens an NVENC session
//! against it, and encodes frames of moving noise into a [`ReplayBuffer`] until
//! the buffer has been full and evicting for a while. Noise rather than a test
//! pattern because a constant-bitrate encoder given easy content spends less
//! than it was configured to, and a measurement of a buffer that was never
//! filled at the stated bitrate is a measurement of nothing.
//!
//! The frames carry media timestamps derived from the frame number, not from a
//! clock, so a thirty-minute buffer is filled with thirty minutes of *video* as
//! fast as the encoder will produce it rather than in thirty minutes of
//! waiting. Nothing about what the buffer holds depends on how long it took to
//! arrive: the retention rule is measured in media time, and the bytes are the
//! bytes.
//!
//! # What it reports
//!
//! The buffer's own accounting, and the process working set around it. The
//! second is what makes the first credible: a buffer that says it holds 670 MiB
//! while the process grew by 1.4 GiB would be a buffer with an accounting bug.
//! The working-set figures are taken with `GetProcessMemoryInfo`, before and
//! after the fill, so the *difference* is what the buffer cost — the encoder
//! session, the source textures and the runtime are all in the baseline.
//!
//! ```text
//! cargo run --release --example buffer_memory -- --window 30
//! cargo run --release --example buffer_memory -- --window 300
//! cargo run --release --example buffer_memory -- --window 1800
//! ```

#[cfg(not(windows))]
fn main() {
    eprintln!("buffer_memory needs Direct3D 11 and NVENC, so it only runs on Windows");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn core::error::Error>> {
    windows_only::run()
}

#[cfg(windows)]
mod windows_only {
    use core::ffi::c_void;
    use core::time::Duration;
    use std::time::Instant;

    use clap::Parser;
    use clipped_encoder::{
        BitRate, Codec, DeviceKind, EncoderConfig, FrameRate, GraphicsDevice, KeyframeInterval,
        NvencEncoder, RateControl, Resolution, SourceFrame, SourceTexture, SurfaceFormat,
        SurfaceKind, VideoEncoder,
    };
    use clipped_replay::{ReplayBuffer, ReplayConfig};
    use windows::core::Interface;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET,
        D3D11_BIND_SHADER_RESOURCE, D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION,
        D3D11_SUBRESOURCE_DATA, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    use windows::Win32::Graphics::Dxgi::{CreateDXGIFactory1, IDXGIAdapter, IDXGIFactory1};
    use windows::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS, PROCESS_MEMORY_COUNTERS_EX,
    };
    use windows::Win32::System::Threading::GetCurrentProcess;

    /// NVIDIA's PCI vendor identifier, which is how the NVENC-capable adapter
    /// is found on a machine with more than one GPU.
    const NVIDIA: u32 = 0x10DE;

    /// How many distinct noise frames are cycled through the encoder.
    ///
    /// Enough that consecutive frames never repeat within a group of pictures,
    /// few enough that the textures themselves are a fixed 63 MiB at 1080p and
    /// so cancel out of the before-and-after working set.
    const TEXTURES: usize = 8;

    #[derive(Debug, Parser)]
    #[command(
        about = "Fills a replay buffer with real encoded video and reports what it cost",
        long_about = None
    )]
    struct Args {
        /// How much video the buffer keeps, in seconds (30 to 1800).
        #[arg(long, default_value_t = 30)]
        window: u64,

        /// Picture width.
        #[arg(long, default_value_t = 1920)]
        width: u32,

        /// Picture height.
        #[arg(long, default_value_t = 1080)]
        height: u32,

        /// Frames a second.
        #[arg(long, default_value_t = 60)]
        fps: u32,

        /// Bitrate in bits a second. The default is what `clipped-session`
        /// gives a 1080p60 recording: width x height x rate x 0.15.
        #[arg(long)]
        bitrate: Option<u32>,

        /// Segment length and keyframe interval, in seconds.
        #[arg(long, default_value_t = 2)]
        segment: u64,

        /// Codec to encode with.
        #[arg(long, default_value = "h264")]
        codec: CodecArg,

        /// How much longer than the window to keep encoding, as a percentage,
        /// so that the buffer is measured while it is evicting rather than
        /// while it is still filling.
        #[arg(long, default_value_t = 20)]
        overfill: u64,
    }

    #[derive(Debug, Clone, Copy, clap::ValueEnum)]
    enum CodecArg {
        H264,
        Hevc,
        Av1,
    }

    impl From<CodecArg> for Codec {
        fn from(codec: CodecArg) -> Self {
            match codec {
                CodecArg::H264 => Self::H264,
                CodecArg::Hevc => Self::Hevc,
                CodecArg::Av1 => Self::Av1,
            }
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn core::error::Error>> {
        let args = Args::parse();
        let codec = Codec::from(args.codec);
        let resolution = Resolution::new(args.width, args.height);
        let rate = FrameRate::new(args.fps, 1).ok_or("a frame rate of zero is not a frame rate")?;
        let bits = args.bitrate.unwrap_or_else(|| default_bitrate(&args));
        let bitrate = BitRate::bits_per_second(bits).ok_or("a bitrate of zero is not a bitrate")?;
        let segment = Duration::from_secs(args.segment);

        let replay = ReplayConfig::new(Duration::from_secs(args.window), bitrate)?
            .with_segment_duration(segment)?;
        let buffer = ReplayBuffer::new(replay);

        let gpu = Gpu::open()?;
        let config = EncoderConfig::new(codec, resolution, rate, RateControl::constant(bitrate))
            .with_keyframe_interval(KeyframeInterval::every(segment, rate))
            .with_source_format(SurfaceFormat::Bgra8Unorm);
        let mut encoder = NvencEncoder::open(&gpu.graphics_device(), config)?;

        let textures: Vec<ID3D11Texture2D> = (0..TEXTURES)
            .map(|index| gpu.noise_texture(args.width, args.height, index))
            .collect::<Result<_, _>>()?;

        println!("configuration");
        println!("  encoder          NVIDIA NVENC, {codec}");
        println!(
            "  picture          {}x{} at {} fps",
            args.width, args.height, args.fps
        );
        println!("  bitrate          {bitrate} (constant)");
        println!("  buffer           {replay}");
        println!("  keyframes        every {} s", args.segment);

        // Everything that is not the buffer is already allocated: the encoder
        // session, the textures, the runtime. What grows from here is the
        // buffer.
        let baseline = memory()?;

        let frames = args.window * u64::from(args.fps) * (100 + args.overfill) / 100;
        let started = Instant::now();
        let mut peak = baseline;
        let mut submitted = 0u64;

        for frame in 0..frames {
            let at = Duration::from_nanos(frame * 1_000_000_000 / u64::from(args.fps));
            let texture = &textures[(frame as usize) % TEXTURES];

            // SAFETY: the texture is owned by `gpu`, was created by the device
            // the encoder was opened against, is a live `ID3D11Texture2D`, and
            // outlives this call — `textures` is dropped after the encoder.
            // `submit` is documented to read it during the call and never
            // afterwards.
            let source = unsafe {
                SourceFrame::new(
                    SourceTexture::new(SurfaceKind::D3d11Texture2D, texture.as_raw()),
                    SurfaceFormat::Bgra8Unorm,
                    resolution,
                    at,
                )
            };

            encoder.submit(&source)?;
            submitted += 1;
            while let Some(packet) = encoder.next_packet()? {
                buffer.push(&packet);
            }

            // Sampling every second of video rather than every frame: reading
            // the working set is a syscall, and the figure moves in pages.
            if frame % u64::from(args.fps) == 0 {
                peak = peak.max(memory()?);
                report_progress(frame, frames, started);
            }
        }

        encoder.finish()?;
        while let Some(packet) = encoder.next_packet()? {
            buffer.push(&packet);
        }
        let filled = memory()?.max(peak);
        let elapsed = started.elapsed();

        // What a save costs while the encoder is still running, which is the
        // other half of the memory question.
        let leasing = Instant::now();
        let lease = buffer.lease_last(Duration::from_secs(args.window))?;
        let leased = leasing.elapsed();
        let after_lease = memory()?;

        let stats = buffer.stats();
        println!();
        println!("what was encoded");
        println!("  frames submitted {submitted}");
        println!(
            "  media time       {:.1} s",
            frames as f64 / f64::from(args.fps)
        );
        println!("  wall clock       {:.1} s", elapsed.as_secs_f64());
        println!(
            "  achieved bitrate {:.2} Mbit/s",
            achieved_bitrate(&stats, &buffer)
        );
        println!();
        println!("what the buffer holds");
        println!("  segments         {}", stats.segments_held());
        println!("  packets          {}", stats.packets_held());
        println!(
            "  covered          {:.1} s",
            stats
                .covered()
                .map_or(0.0, |range| range.length().as_secs_f64())
        );
        println!("  bytes held       {}", megabytes(stats.bytes_held()));
        println!("  peak bytes held  {}", megabytes(stats.peak_bytes_held()));
        println!(
            "  ceiling          {}",
            megabytes(buffer.config().memory_ceiling())
        );
        println!("  evicted (window) {}", stats.segments_evicted_for_window());
        println!(
            "  evicted (ceiling) {}",
            stats.segments_evicted_over_ceiling()
        );
        println!(
            "  cut short        {}",
            stats.segments_sealed_at_the_ceiling()
        );
        println!(
            "  discarded (ceiling) {}",
            stats.packets_discarded_over_ceiling()
        );
        println!();
        println!("what the process holds");
        println!("  working set before {}", megabytes(baseline.working_set));
        println!("  working set after  {}", megabytes(filled.working_set));
        println!(
            "  working set growth {}",
            megabytes(filled.working_set.saturating_sub(baseline.working_set))
        );
        println!("  private before     {}", megabytes(baseline.private));
        println!("  private after      {}", megabytes(filled.private));
        println!(
            "  private growth     {}",
            megabytes(filled.private.saturating_sub(baseline.private))
        );
        println!();
        println!("a save of the whole window");
        println!("  segments leased  {}", lease.len());
        println!("  bytes leased     {}", megabytes(lease.byte_len() as u64));
        println!("  complete         {}", lease.is_complete());
        println!("  time to lease    {:.3} ms", leased.as_secs_f64() * 1000.0);
        println!("  working set      {}", megabytes(after_lease.working_set));

        drop(lease);
        encoder.shut_down();
        Ok(())
    }

    /// What `clipped-session` gives a recording of this size and rate.
    fn default_bitrate(args: &Args) -> u32 {
        let bits = f64::from(args.width) * f64::from(args.height) * f64::from(args.fps) * 0.15;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            bits.clamp(2_000_000.0, 120_000_000.0) as u32
        }
    }

    /// The bitrate the buffer's contents actually work out at.
    fn achieved_bitrate(stats: &clipped_replay::ReplayStats, buffer: &ReplayBuffer) -> f64 {
        let seconds = stats
            .covered()
            .map_or(0.0, |range| range.length().as_secs_f64());
        if seconds <= 0.0 {
            return 0.0;
        }

        #[allow(clippy::cast_precision_loss)]
        let bytes = (stats.bytes_held() - stats.bytes_retained_for_a_save()) as f64;
        let _ = buffer;
        bytes * 8.0 / seconds / 1_000_000.0
    }

    fn report_progress(frame: u64, frames: u64, started: Instant) {
        let percent = frame * 100 / frames.max(1);
        if frame > 0 && percent % 10 == 0 && frame % (frames.max(1) / 10).max(1) < 60 {
            println!(
                "  ... {percent}% after {:.0} s",
                started.elapsed().as_secs_f64()
            );
        }
    }

    #[allow(clippy::cast_precision_loss)]
    fn megabytes(bytes: u64) -> String {
        format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
    }

    /// A reading of what this process is occupying.
    #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
    struct Memory {
        working_set: u64,
        private: u64,
    }

    impl Memory {
        fn max(self, other: Self) -> Self {
            Self {
                working_set: self.working_set.max(other.working_set),
                private: self.private.max(other.private),
            }
        }
    }

    /// This process's resident and private memory, from `GetProcessMemoryInfo`.
    fn memory() -> Result<Memory, Box<dyn core::error::Error>> {
        let mut counters = PROCESS_MEMORY_COUNTERS_EX::default();

        // SAFETY: `GetCurrentProcess` returns a pseudo-handle that needs no
        // release, the pointer is a live local of the extended counters
        // structure, and the size passed is that structure's own size — which
        // is how the function knows which of the two layouts it was given.
        unsafe {
            GetProcessMemoryInfo(
                GetCurrentProcess(),
                std::ptr::from_mut(&mut counters).cast::<PROCESS_MEMORY_COUNTERS>(),
                u32::try_from(size_of::<PROCESS_MEMORY_COUNTERS_EX>())?,
            )?;
        }

        Ok(Memory {
            working_set: counters.WorkingSetSize as u64,
            private: counters.PrivateUsage as u64,
        })
    }

    /// The Direct3D 11 device the frames live on.
    struct Gpu {
        device: ID3D11Device,
    }

    impl Gpu {
        fn open() -> Result<Self, Box<dyn core::error::Error>> {
            let adapter = nvidia_adapter()?;
            let mut device: Option<ID3D11Device> = None;

            // SAFETY: `adapter` is a live DXGI adapter, the driver type must be
            // UNKNOWN when an adapter is named, the module handle is unused for
            // that driver type, and the feature levels and out-parameter are
            // live locals. On success `device` holds one reference, released on
            // drop.
            unsafe {
                D3D11CreateDevice(
                    &adapter,
                    D3D_DRIVER_TYPE_UNKNOWN,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    Some(&[D3D_FEATURE_LEVEL_11_0]),
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
            }?;

            Ok(Self {
                device: device.ok_or("D3D11CreateDevice reported success without a device")?,
            })
        }

        fn graphics_device(&self) -> GraphicsDevice {
            // SAFETY: the device is alive for as long as `self` is, and the
            // encoder opened from it is dropped before it.
            unsafe { GraphicsDevice::new(DeviceKind::D3d11, self.device.as_raw()) }
        }

        /// A texture of pseudo-random noise, which is what makes a
        /// constant-bitrate encoder actually spend its bitrate.
        fn noise_texture(
            &self,
            width: u32,
            height: u32,
            seed: usize,
        ) -> Result<ID3D11Texture2D, Box<dyn core::error::Error>> {
            let mut pixels = vec![0u8; (width as usize) * (height as usize) * 4];
            let mut state = 0x9E37_79B9_u32.wrapping_add(seed as u32 * 0x85EB_CA6B);
            for byte in &mut pixels {
                // xorshift32: not a good random number generator and an
                // entirely adequate source of incompressible bytes.
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                *byte = (state >> 24) as u8;
            }

            let description = D3D11_TEXTURE2D_DESC {
                Width: width,
                Height: height,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC {
                    Count: 1,
                    Quality: 0,
                },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let data = D3D11_SUBRESOURCE_DATA {
                pSysMem: pixels.as_ptr().cast::<c_void>(),
                SysMemPitch: width * 4,
                SysMemSlicePitch: 0,
            };

            let mut texture: Option<ID3D11Texture2D> = None;
            // SAFETY: the description and the initial data are live locals, the
            // data holds `Width * Height * 4` bytes as the pitch declares, and
            // the out-parameter is a live local that takes the one reference the
            // call returns.
            unsafe {
                self.device
                    .CreateTexture2D(&description, Some(&data), Some(&mut texture))
            }?;

            Ok(texture.ok_or("CreateTexture2D reported success without a texture")?)
        }
    }

    /// The NVIDIA adapter, or a message saying there is not one.
    fn nvidia_adapter() -> Result<IDXGIAdapter, Box<dyn core::error::Error>> {
        // SAFETY: the factory is a COM object released when the local is
        // dropped, and `EnumAdapters` fills a live out-parameter.
        let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1() }?;

        for index in 0.. {
            // SAFETY: `factory` is live; the call returns an error once the
            // index runs past the last adapter, which ends the loop.
            let Ok(adapter) = (unsafe { factory.EnumAdapters(index) }) else {
                break;
            };

            // SAFETY: `adapter` is a live DXGI adapter.
            let description = unsafe { adapter.GetDesc() }?;
            if description.VendorId == NVIDIA {
                return Ok(adapter);
            }
        }

        Err("no NVIDIA adapter on this machine, so NVENC cannot be measured".into())
    }
}

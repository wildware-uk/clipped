//! Measures what taking a screenshot costs the thread that captures frames.
//!
//! [Issue #67](https://github.com/wildware-uk/clipped/issues/67) asks how a
//! screenshot avoids costing the game a frame, and the answer is only worth
//! anything with numbers behind it (AGENTS.md section 19). This is where the
//! figures in `docs/capture-pipeline.md` come from.
//!
//! # What it measures, and what it deliberately does not
//!
//! The two halves of [`D3d11StillCopier`], separately, because they are paid at
//! different moments and only one of them can stall:
//!
//! - **`begin`** — `CopyResource` into a staging texture plus a `Flush`. Queued
//!   for the GPU; the thread does not wait for it.
//! - **`poll`/`finish`** — the map and the memory copy, on a later frame. This
//!   is the one that would stall if it were done immediately, which is the
//!   whole reason the copy is in two halves.
//!
//! It does **not** open a window, start a capture or record anything. The
//! source is a texture created with initial data on a real Direct3D device,
//! which is the same kind of resource a capture backend hands over and is what
//! makes this runnable on a shared machine and in a diagnosis (AGENTS.md
//! section 25). What it therefore does not measure is the compositor: a real
//! frame arrives in the backend's own pool, and the copy out of it is the same
//! call on the same kind of resource.
//!
//! # Running it
//!
//! ```text
//! cargo run -p clipped-capture --example still_cost -- --width 2560 --height 1440
//! ```

#[cfg(not(windows))]
fn main() {
    eprintln!("this example measures a Direct3D 11 copy and only runs on Windows");
}

#[cfg(windows)]
fn main() {
    windows_only::run();
}

#[cfg(windows)]
mod windows_only {
    use std::time::{Duration, Instant};

    use clipped_capture::windows::D3d11StillCopier;
    use clipped_capture::{
        CaptureTimestamp, CapturedFrame, FrameFormat, FrameSize, FrameTexture, PixelFormat,
        SourceClock, TextureKind,
    };
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_HARDWARE, D3D_DRIVER_TYPE_WARP};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11CreateDevice, ID3D11Device, ID3D11Texture2D, D3D11_BIND_SHADER_RESOURCE,
        D3D11_CREATE_DEVICE_BGRA_SUPPORT, D3D11_SDK_VERSION, D3D11_SUBRESOURCE_DATA,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

    /// How many screenshots are taken, after a discarded first one.
    const SAMPLES: usize = 60;

    pub(super) fn run() {
        let (width, height) = size_from_arguments();
        let Some((device, adapter)) = device() else {
            eprintln!("no Direct3D 11 device on this machine; nothing to measure");
            return;
        };

        let texture = painted(&device, width, height);
        let format = FrameFormat::new(
            FrameSize::new(width, height).expect("a size given on the command line is not zero"),
            PixelFormat::Bgra8Unorm,
        );

        let mut copier = D3d11StillCopier::new();
        let mut issue = Vec::with_capacity(SAMPLES);
        let mut read = Vec::with_capacity(SAMPLES);
        let mut polls = 0_u32;
        let mut bytes = 0_usize;

        // One discarded run: the first screenshot creates the staging texture,
        // and a figure that included that would be a figure for something
        // nobody experiences twice.
        take(&mut copier, &texture, format);

        for _ in 0..SAMPLES {
            let started = Instant::now();
            begin(&mut copier, &texture, format);
            issue.push(started.elapsed());

            // The frame or two a recording spends capturing before it looks
            // again. Without it this would measure the blocking map, which is
            // the thing the design exists to avoid.
            std::thread::sleep(Duration::from_millis(8));

            let started = Instant::now();
            loop {
                polls += 1;
                match copier.poll().expect("the copy is readable") {
                    Some(still) => {
                        bytes = still.byte_count();
                        break;
                    }
                    None => std::thread::yield_now(),
                }
            }
            read.push(started.elapsed());
        }

        issue.sort_unstable();
        read.sort_unstable();

        println!("adapter          {adapter}");
        println!(
            "frame            {width}x{height} BGRA8, {} kB",
            bytes / 1024
        );
        println!("samples          {SAMPLES}");
        println!("polls per copy   {:.2}", f64::from(polls) / SAMPLES as f64);
        report("begin (queue)", &issue);
        report("poll  (read)", &read);
    }

    /// Starts a copy of the texture as though a backend had handed it over.
    fn begin(copier: &mut D3d11StillCopier, texture: &ID3D11Texture2D, format: FrameFormat) {
        // SAFETY: `texture` is a live `ID3D11Texture2D` owned by the caller for
        // longer than the `FrameTexture` and `CapturedFrame` built around it,
        // both of which are dropped at the end of this function.
        let borrowed =
            unsafe { FrameTexture::new(TextureKind::D3d11Texture2D, texture_pointer(texture)) };
        let frame = CapturedFrame::new(
            borrowed,
            format,
            CaptureTimestamp::from_source(SourceClock::PerformanceCounter, 0),
        );
        copier.begin(&frame).expect("the copy is issued");
    }

    /// One whole screenshot, blocking, to warm the staging texture up.
    fn take(copier: &mut D3d11StillCopier, texture: &ID3D11Texture2D, format: FrameFormat) {
        begin(copier, texture, format);
        copier.finish().expect("the copy is read back");
    }

    fn report(label: &str, samples: &[Duration]) {
        let median = samples[samples.len() / 2];
        let worst = samples[samples.len() - 1];
        let total: Duration = samples.iter().sum();
        println!(
            "{label:16} median {:>7.3} ms   mean {:>7.3} ms   worst {:>7.3} ms",
            median.as_secs_f64() * 1_000.0,
            total.as_secs_f64() * 1_000.0 / samples.len() as f64,
            worst.as_secs_f64() * 1_000.0
        );
    }

    /// `--width` and `--height`, or 2560x1440.
    ///
    /// Parsed by hand rather than with `clap` because two integers do not
    /// justify a dependency in an example (AGENTS.md section 10).
    fn size_from_arguments() -> (u32, u32) {
        let mut width = 2560;
        let mut height = 1440;
        let mut arguments = std::env::args().skip(1);
        while let Some(argument) = arguments.next() {
            let value = arguments.next().and_then(|value| value.parse().ok());
            match (argument.as_str(), value) {
                ("--width", Some(value)) => width = value,
                ("--height", Some(value)) => height = value,
                _ => eprintln!("usage: still_cost [--width N] [--height N]"),
            }
        }
        (width.max(2), height.max(2))
    }

    /// The raw pointer a `FrameTexture` wraps.
    fn texture_pointer(texture: &ID3D11Texture2D) -> *mut core::ffi::c_void {
        use windows::core::Interface;
        texture.as_raw()
    }

    /// A hardware device where there is one, and WARP where there is not.
    ///
    /// The adapter is named in the output because a readback figure without one
    /// is a number nobody can compare against (AGENTS.md section 19).
    fn device() -> Option<(ID3D11Device, &'static str)> {
        for (kind, name) in [
            (D3D_DRIVER_TYPE_HARDWARE, "hardware"),
            (D3D_DRIVER_TYPE_WARP, "WARP (software)"),
        ] {
            let mut device: Option<ID3D11Device> = None;
            // SAFETY: every pointer argument is either absent or the address of
            // a live local `Option<ID3D11Device>`, which is the representation
            // windows-rs uses for an out parameter of that type.
            let created = unsafe {
                D3D11CreateDevice(
                    None,
                    kind,
                    HMODULE::default(),
                    D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                    None,
                    D3D11_SDK_VERSION,
                    Some(&mut device),
                    None,
                    None,
                )
            };
            if created.is_ok() {
                if let Some(device) = device {
                    return Some((device, name));
                }
            }
        }
        None
    }

    /// A texture of the given size, filled with something other than one colour.
    fn painted(device: &ID3D11Device, width: u32, height: u32) -> ID3D11Texture2D {
        let pixels: Vec<u32> = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x ^ y) | 0xFF00_0000))
            .collect();

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
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let data = D3D11_SUBRESOURCE_DATA {
            pSysMem: pixels.as_ptr().cast(),
            SysMemPitch: width * 4,
            SysMemSlicePitch: 0,
        };
        let mut texture: Option<ID3D11Texture2D> = None;
        // SAFETY: `description` and `data` are live locals; `pixels` holds
        // exactly `width * height` four-byte pixels at the pitch `data`
        // declares and outlives this call, which is the only time Direct3D
        // reads it.
        unsafe {
            device
                .CreateTexture2D(
                    &raw const description,
                    Some(&raw const data),
                    Some(&raw mut texture),
                )
                .expect("a BGRA8 texture with initial data");
        }
        texture.expect("CreateTexture2D returned success and a texture")
    }
}

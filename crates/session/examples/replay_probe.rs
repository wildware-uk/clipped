//! Records a real window with a replay buffer attached, and says what the
//! buffer ended up holding.
//!
//! This is how [`clipped_session::record_with_replay`] is exercised end to end
//! before there is a command that turns a replay buffer on
//! ([issue #38](https://github.com/wildware-uk/clipped/issues/38)). Everything
//! in it is real: the Windows Graphics Capture backend, a hardware encoder
//! session, the Matroska writer, and the buffer being filled from the same
//! packets the file is written from.
//!
//! It is an example rather than a test because it needs a GPU, a desktop
//! session and a window to point at — the same reasons `tests/capture` is
//! `#[ignore]`d — and because the thing it proves is a wiring, not a rule.
//!
//! ```text
//! clipped-recorder list-windows
//! cargo run --release -p clipped-session --example replay_probe -- \
//!     --window 0x000104ac --width 2560 --height 1440 --seconds 45
//! ```
//!
//! The window handle, width and height are what `clipped-recorder list-windows`
//! prints. The recording is written to a temporary file and deleted, because
//! what is being looked at is the buffer.

#[cfg(not(windows))]
fn main() {
    eprintln!("replay_probe records a window, which only this crate's Windows build can do");
    std::process::exit(1);
}

#[cfg(windows)]
fn main() -> Result<(), Box<dyn core::error::Error>> {
    windows_only::run()
}

#[cfg(windows)]
mod windows_only {
    use core::sync::atomic::{AtomicBool, Ordering};
    use core::time::Duration;
    use std::sync::Arc;
    use std::thread;
    use std::time::Instant;

    use clap::Parser;
    use clipped_encoder::BitRate;
    use clipped_replay::{ReplayBuffer, ReplayConfig};
    use clipped_session::{
        record_with_replay, CaptureTargetSettings, RecordingSettings, StopSignal,
    };

    #[derive(Debug, Parser)]
    #[command(
        about = "Records a window with a replay buffer attached and reports what the buffer held",
        long_about = None
    )]
    struct Args {
        /// The window to record, as `clipped-recorder list-windows` prints it.
        #[arg(long, value_parser = handle)]
        window: u64,

        /// The window's client width in physical pixels.
        #[arg(long)]
        width: u32,

        /// The window's client height in physical pixels.
        #[arg(long)]
        height: u32,

        /// How long to record for.
        #[arg(long, default_value_t = 45)]
        seconds: u64,

        /// How much video the replay buffer keeps.
        #[arg(long, default_value_t = 30)]
        replay_seconds: u64,

        /// How much of the buffer to lease at the end, standing in for a
        /// hotkey press.
        #[arg(long, default_value_t = 15)]
        save_seconds: u64,

        /// Where to write the recording. Deleted afterwards unless `--keep`.
        #[arg(long)]
        output: Option<std::path::PathBuf>,

        /// Keeps the recording rather than deleting it.
        #[arg(long)]
        keep: bool,
    }

    /// A window handle, in decimal or as `0x`-prefixed hexadecimal.
    fn handle(text: &str) -> Result<u64, String> {
        let trimmed = text.trim_start_matches("0x").trim_start_matches("0X");
        let radix = if trimmed.len() == text.len() { 10 } else { 16 };
        u64::from_str_radix(trimmed, radix).map_err(|error| format!("{text}: {error}"))
    }

    /// Stops the recording once the clock runs out.
    #[derive(Debug)]
    struct Deadline(Arc<AtomicBool>);

    impl StopSignal for Deadline {
        fn is_requested(&self) -> bool {
            self.0.load(Ordering::Relaxed)
        }
    }

    pub(super) fn run() -> Result<(), Box<dyn core::error::Error>> {
        let args = Args::parse();

        let output = args.output.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!("clipped-replay-probe-{}.mkv", std::process::id()))
        });
        let settings = RecordingSettings::new(
            CaptureTargetSettings::window(args.window, args.width, args.height),
            output.clone(),
        );

        // The rate `clipped-session` gives a recording of this size, which is
        // what the buffer has to be sized from: it is a consumer of the packets
        // that rate produces (`crates/session/src/encoding.rs`).
        let bits =
            f64::from(args.width) * f64::from(args.height) * f64::from(settings.framerate()) * 0.15;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let bitrate = BitRate::bits_per_second(bits.clamp(2_000_000.0, 120_000_000.0) as u32)
            .ok_or("a bitrate of zero is not a bitrate")?;

        let replay = ReplayConfig::new(Duration::from_secs(args.replay_seconds), bitrate)?;
        let buffer = ReplayBuffer::new(replay);
        println!("replay buffer: {replay}");

        let stop = Arc::new(AtomicBool::new(false));
        let timer = {
            let stop = Arc::clone(&stop);
            let seconds = args.seconds;
            thread::Builder::new()
                .name("replay-probe-timer".to_owned())
                .spawn(move || {
                    thread::sleep(Duration::from_secs(seconds));
                    stop.store(true, Ordering::Relaxed);
                })?
        };

        let started = Instant::now();
        let report = record_with_replay(&settings, &Deadline(Arc::clone(&stop)), &buffer)?;
        timer.join().map_err(|_| "the timer thread panicked")?;

        println!();
        println!("{report}");
        println!();

        let stats = buffer.stats();
        println!(
            "replay buffer after {:.1} s",
            started.elapsed().as_secs_f64()
        );
        println!("  segments held      {}", stats.segments_held());
        println!("  packets held       {}", stats.packets_held());
        println!("  bytes held         {}", stats.bytes_held());
        println!("  peak bytes held    {}", stats.peak_bytes_held());
        println!("  ceiling            {}", buffer.config().memory_ceiling());
        println!(
            "  covered            {:.1} s",
            stats
                .covered()
                .map_or(0.0, |range| range.length().as_secs_f64())
        );
        println!(
            "  evicted (window)   {}",
            stats.segments_evicted_for_window()
        );
        println!(
            "  evicted (ceiling)  {}",
            stats.segments_evicted_over_ceiling()
        );
        println!(
            "  discarded pre-key  {}",
            stats.packets_discarded_before_first_keyframe()
        );
        println!(
            "  cut short          {}",
            stats.segments_sealed_at_the_ceiling()
        );
        println!(
            "  discarded (ceiling) {}",
            stats.packets_discarded_over_ceiling()
        );

        // The hotkey press, after the fact.
        let lease = buffer.lease_last(Duration::from_secs(args.save_seconds))?;
        let keyframes = lease
            .segments()
            .filter(|segment| {
                segment
                    .packets()
                    .next()
                    .is_some_and(|packet| packet.is_keyframe())
            })
            .count();

        println!();
        println!("a save of the last {} s", args.save_seconds);
        println!("  segments leased    {}", lease.len());
        println!("  beginning on a key {keyframes}");
        println!("  packets            {}", lease.packets().count());
        println!("  bytes              {}", lease.byte_len());
        println!("  requested          {}", lease.requested());
        println!("  covered            {}", lease.covered());
        println!("  complete           {}", lease.is_complete());
        println!(
            "  leading slack      {:.3} s",
            lease.leading_slack().as_secs_f64()
        );
        println!(
            "  trailing slack     {:.3} s",
            lease.trailing_slack().as_secs_f64()
        );

        drop(lease);
        if args.keep {
            println!();
            println!("recording kept at {}", output.display());
        } else {
            std::fs::remove_file(&output)?;
        }

        Ok(())
    }
}

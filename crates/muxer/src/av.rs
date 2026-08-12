//! The FFmpeg objects this crate owns, and the rules for releasing them.
//!
//! Two things in here, both of them wanted by more than one caller: a container
//! context, and the one packet a loop reuses. [`crate::writer`] holds them to
//! write a recording; [`crate::remux`] holds a second pair to copy one into
//! another container. Neither has its own copy, which is the point of the module
//! (AGENTS.md section 55) — the ownership rules for an `AVFormatContext` are
//! subtle enough that two of them would eventually differ.
//!
//! Every type here releases what it owns in `Drop`, so that a caller cleans up
//! by returning rather than by remembering to (AGENTS.md section 58).

use core::ptr::{self, NonNull};
use std::ffi::{c_int, CString};

use rusty_ffmpeg::ffi;

use crate::error::{AvError, MuxError};

/// A container being written, and the file it is writing to.
///
/// Both are released here, in one place, so that every early return in a
/// caller's set-up cleans up by returning.
pub(crate) struct OutputContext(NonNull<ffi::AVFormatContext>);

impl OutputContext {
    /// Allocates an output context for the named muxer.
    ///
    /// The muxer is named rather than guessed from the file extension: what
    /// container a file is written in is a decision, not a consequence of what
    /// somebody typed after the dot.
    pub(crate) fn allocate(muxer: &core::ffi::CStr) -> Result<Self, MuxError> {
        let mut context: *mut ffi::AVFormatContext = ptr::null_mut();

        // SAFETY: `avformat_alloc_output_context2` writes an allocated context
        // through its first argument, which is a live local. The format is
        // named by the NUL-terminated argument rather than by a filename, so the
        // remaining two arguments are legitimately null. On failure it leaves
        // the pointer null, which is checked below before it is used.
        let code = unsafe {
            ffi::avformat_alloc_output_context2(
                &mut context,
                ptr::null(),
                muxer.as_ptr(),
                ptr::null(),
            )
        };

        match NonNull::new(context) {
            // A negative code with a context allocated should not happen, but
            // trusting the pointer over the code would be a leak at best.
            Some(context) if code >= 0 => Ok(Self(context)),
            Some(context) => {
                // SAFETY: `context` was allocated by the call above and has not
                // been handed to anything else, so freeing it here is the only
                // release of it.
                unsafe { ffi::avformat_free_context(context.as_ptr()) };
                Err(MuxError::Ffmpeg {
                    operation: "allocating an output container context",
                    source: AvError::new(code),
                })
            }
            None => Err(MuxError::Ffmpeg {
                operation: "allocating an output container context",
                source: AvError::new(code),
            }),
        }
    }

    /// The context, for passing to FFmpeg.
    pub(crate) fn as_ptr(&self) -> *mut ffi::AVFormatContext {
        self.0.as_ptr()
    }

    /// Opens `url` for writing and attaches it to the context.
    pub(crate) fn open_output(&mut self, url: &str) -> Result<(), MuxError> {
        let Ok(url) = CString::new(url) else {
            // Only reachable from a path containing a NUL, which no filesystem
            // permits; the caller has a byte string that is not a path.
            return Err(MuxError::Ffmpeg {
                operation: "opening the output file",
                source: AvError::new(-(ffi::EINVAL as i32)),
            });
        };

        // SAFETY: `avio_open` writes the opened context through its first
        // argument, which points at the context's own `pb` field, and reads
        // `url` as a NUL-terminated string that outlives the call. `pb` is null
        // beforehand — nothing else assigns it — so nothing is overwritten and
        // leaked. Ownership of what it writes passes to this value, which
        // closes it in `Drop`.
        let code = unsafe {
            ffi::avio_open(
                &mut (*self.as_ptr()).pb,
                url.as_ptr(),
                ffi::AVIO_FLAG_WRITE as c_int,
            )
        };

        if code < 0 {
            return Err(MuxError::Ffmpeg {
                operation: "opening the output file",
                source: AvError::new(code),
            });
        }

        // SAFETY: the context is live and `url` is null on a context this crate
        // has just allocated, so nothing is overwritten and leaked. `av_strdup`
        // copies the NUL-terminated string with FFmpeg's allocator and returns
        // either null or a buffer the context then owns and frees in
        // `avformat_free_context`.
        //
        // libavformat documents `url` as "set by the user for output", and a
        // muxer that has to re-open its own output reads it: MP4's `faststart`
        // moves the index to the front of the finished file by opening it a
        // second time, and with no URL to open it fails at the trailer — after
        // the whole file has been written. A null copy is left null rather than
        // treated as a failure, because the only muxer that needs it says so
        // loudly when it is missing and the recording path does not use it at
        // all.
        unsafe {
            let context = self.as_ptr();
            if (*context).url.is_null() {
                (*context).url = ffi::av_strdup(url.as_ptr());
            }
        }

        Ok(())
    }

    /// The time base the muxer fixed a stream at, read back after the header
    /// was written.
    ///
    /// Every muxer replaces the caller's hint with the unit it actually counts
    /// in — a millisecond for Matroska, the sample rate or the frame rate for
    /// MP4 — so a caller that assumed its own hint had survived would time every
    /// packet it wrote by the wrong clock.
    pub(crate) fn stream_time_base(&self, stream_index: c_int) -> Option<ffi::AVRational> {
        // SAFETY: the context is live and `stream_index` came from
        // `avformat_new_stream` on this same context, so it indexes a stream the
        // context still owns. `nb_streams` is checked anyway, because a wrong
        // index here would read arbitrary memory rather than fail.
        unsafe {
            let context = self.as_ptr();
            let index = usize::try_from(stream_index).ok()?;
            if index >= (*context).nb_streams as usize {
                return None;
            }
            Some((**(*context).streams.add(index)).time_base)
        }
    }
}

impl Drop for OutputContext {
    fn drop(&mut self) {
        // SAFETY: this value owns both resources and nothing else has a pointer
        // to either. `avio_closep` flushes and closes the file and nulls the
        // field, so a context whose output was never opened — an error between
        // allocating and opening — skips it and frees only the context.
        unsafe {
            let context = self.as_ptr();
            if !(*context).pb.is_null() {
                ffi::avio_closep(&mut (*context).pb);
            }
            ffi::avformat_free_context(context);
        }
    }
}

/// A file being read, opened by libavformat's demuxers.
///
/// Opened for reading and nothing else: `avformat_open_input` never writes to
/// what it opens, which is what makes remuxing safe to point at a recording
/// somebody cannot replace (AGENTS.md section 56).
pub(crate) struct InputContext(NonNull<ffi::AVFormatContext>);

impl InputContext {
    /// Opens `url` and reads enough of it to describe its streams.
    ///
    /// `operation` names what the caller was doing, for the error message.
    pub(crate) fn open(url: &str) -> Result<Self, AvError> {
        let Ok(url) = CString::new(url) else {
            // Only reachable from a path containing a NUL, which no filesystem
            // permits.
            return Err(AvError::new(-(ffi::EINVAL as i32)));
        };

        let mut context: *mut ffi::AVFormatContext = ptr::null_mut();

        // SAFETY: `avformat_open_input` allocates a context and writes it
        // through its first argument, which is a live local. `url` is
        // NUL-terminated and outlives the call. Passing null for the format and
        // the options asks it to probe the file and to take no options. On
        // failure it leaves the pointer null, which is why the code is checked
        // first.
        let code = unsafe {
            ffi::avformat_open_input(&mut context, url.as_ptr(), ptr::null(), ptr::null_mut())
        };
        if code < 0 {
            return Err(AvError::new(code));
        }

        let Some(context) = NonNull::new(context) else {
            // A success code with no context is not a documented outcome, but
            // dereferencing null would be the alternative.
            return Err(AvError::new(-(ffi::EINVAL as i32)));
        };
        let opened = Self(context);

        // SAFETY: the context is live and owned here. `avformat_find_stream_info`
        // reads far enough into the file to fill in each stream's parameters,
        // and takes no options.
        let code = unsafe { ffi::avformat_find_stream_info(opened.as_ptr(), ptr::null_mut()) };
        if code < 0 {
            return Err(AvError::new(code));
        }

        Ok(opened)
    }

    /// The context, for passing to FFmpeg.
    pub(crate) fn as_ptr(&self) -> *mut ffi::AVFormatContext {
        self.0.as_ptr()
    }

    /// How many streams the file declares.
    pub(crate) fn stream_count(&self) -> usize {
        // SAFETY: the context is live and `nb_streams` is a plain integer field
        // filled in while the file was opened.
        unsafe { (*self.as_ptr()).nb_streams as usize }
    }

    /// One of the file's streams.
    ///
    /// Returns [`None`] past the end rather than reading arbitrary memory.
    pub(crate) fn stream(&self, index: usize) -> Option<*mut ffi::AVStream> {
        if index >= self.stream_count() {
            return None;
        }
        // SAFETY: the context is live and `index` was just bounded by
        // `nb_streams`, so it indexes a stream the context owns.
        Some(unsafe { *(*self.as_ptr()).streams.add(index) })
    }

    /// How many chapters the file declares.
    pub(crate) fn chapter_count(&self) -> usize {
        // SAFETY: as for `stream_count`.
        unsafe { (*self.as_ptr()).nb_chapters as usize }
    }
}

impl Drop for InputContext {
    fn drop(&mut self) {
        let mut context = self.as_ptr();
        // SAFETY: this value owns the context and nothing else holds a pointer
        // to it. `avformat_close_input` closes the file, frees the context and
        // nulls the local pointer.
        unsafe { ffi::avformat_close_input(&mut context) };
    }
}

/// The one packet structure a loop reuses for every packet it handles.
///
/// A recorder writes one of these per frame for hours and a remux reads one per
/// frame of the file it is copying, so it is allocated once (AGENTS.md section
/// 18). What it holds depends on the caller: [`crate::writer`] borrows the
/// caller's bytes into it, while [`crate::remux`] lets `av_read_frame` fill it
/// with a reference of FFmpeg's own. `Drop` releases whichever it is.
pub(crate) struct PacketSlot(NonNull<ffi::AVPacket>);

impl PacketSlot {
    pub(crate) fn allocate() -> Result<Self, MuxError> {
        // SAFETY: `av_packet_alloc` takes no arguments and returns either null
        // or a packet this value then owns.
        let packet = unsafe { ffi::av_packet_alloc() };
        NonNull::new(packet).map(Self).ok_or(MuxError::Ffmpeg {
            operation: "allocating a packet",
            source: AvError::new(-(ffi::ENOMEM as i32)),
        })
    }

    pub(crate) fn as_ptr(&self) -> *mut ffi::AVPacket {
        self.0.as_ptr()
    }
}

impl Drop for PacketSlot {
    fn drop(&mut self) {
        let mut packet = self.as_ptr();
        // SAFETY: this value owns the packet and nothing else holds a pointer
        // to it. `av_packet_free` unreferences whatever the packet holds and
        // nulls the local pointer.
        unsafe { ffi::av_packet_free(&mut packet) };
    }
}

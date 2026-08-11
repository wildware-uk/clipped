//! Where one message ends and the next begins.
//!
//! A frame is a four-byte little-endian length followed by exactly that many
//! bytes of UTF-8 JSON. Nothing about a message's *content* delimits it: JSON
//! has no terminator, a newline is legal inside a string, and a reader that
//! looked for one would be a reader a peer could confuse by putting a newline
//! in a window title.
//!
//! # The length prefix is not trusted
//!
//! A length prefix is an instruction from the other end of a pipe to allocate
//! memory. [`MAX_FRAME_BYTES`] is checked **before** anything is allocated and
//! before a single payload byte is read, so a peer that declares four gigabytes
//! gets [`FrameError::TooLarge`] and a closed connection rather than the
//! recorder's address space. The recorder is the process that must not fall
//! over (AGENTS.md section 17), and it is reachable by anything running as the
//! user — a buggy script, not only the desktop application.
//!
//! # Reading and writing are over `Read` and `Write`
//!
//! Deliberately, so that the framing can be tested against a byte buffer and a
//! deliberately hostile reader without a pipe, a process or a platform in the
//! way. The transport's only job is to produce something that implements those
//! two traits.

use std::fmt;
use std::io::{self, Read, Write};

use serde::de::DeserializeOwned;
use serde::Serialize;

/// The largest frame either side will send or accept, in bytes.
///
/// One mebibyte, which is roughly two orders of magnitude above the largest
/// message this protocol has — a status event is a few hundred bytes — and far
/// below anything that would matter to a process that also holds encoder
/// buffers. It is a bound on damage, not a budget to spend: a message anywhere
/// near it means something is wrong, and the connection is closed rather than
/// resynchronised.
pub const MAX_FRAME_BYTES: u32 = 1024 * 1024;

/// The length prefix, in bytes.
const LENGTH_PREFIX_BYTES: usize = 4;

/// Why a frame could not be read or written.
#[derive(Debug)]
pub enum FrameError {
    /// The peer closed the connection at a frame boundary, or vanished part way
    /// through a frame.
    ///
    /// Not a fault. The desktop application is closed by the user all the time,
    /// and a recorder that treated that as an error would fill its log with the
    /// user going about their day.
    Disconnected,
    /// The peer declared a frame larger than [`MAX_FRAME_BYTES`]. Nothing was
    /// allocated and nothing was read.
    TooLarge {
        /// What the length prefix claimed.
        declared: u32,
    },
    /// The bytes inside a frame were not a message of the expected shape.
    Malformed(serde_json::Error),
    /// The connection itself failed.
    Io(io::Error),
}

impl FrameError {
    /// Whether this is the peer having gone away rather than a fault.
    #[must_use]
    pub const fn is_disconnect(&self) -> bool {
        matches!(self, Self::Disconnected)
    }
}

impl fmt::Display for FrameError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Disconnected => formatter.write_str("the other end of the connection closed"),
            Self::TooLarge { declared } => write!(
                formatter,
                "a frame of {declared} bytes was announced and the limit is {MAX_FRAME_BYTES}"
            ),
            Self::Malformed(error) => write!(formatter, "the frame was not a message: {error}"),
            Self::Io(error) => write!(formatter, "the connection failed: {error}"),
        }
    }
}

impl std::error::Error for FrameError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Malformed(error) => Some(error),
            Self::Io(error) => Some(error),
            Self::Disconnected | Self::TooLarge { .. } => None,
        }
    }
}

impl From<io::Error> for FrameError {
    /// Turns the several ways a peer can disappear into one of them.
    ///
    /// A Windows named pipe whose client has closed fails a read with
    /// `ERROR_BROKEN_PIPE` rather than returning end of file, and a client that
    /// was killed part way through a write leaves a truncated frame, which
    /// surfaces as [`io::ErrorKind::UnexpectedEof`]. All of them mean the same
    /// thing to a caller, and none of them is worth an error-level log line.
    fn from(error: io::Error) -> Self {
        match error.kind() {
            io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::UnexpectedEof => Self::Disconnected,
            _ => Self::Io(error),
        }
    }
}

/// Writes one message as one frame.
///
/// # Errors
///
/// [`FrameError::TooLarge`] if the message would exceed [`MAX_FRAME_BYTES`] —
/// refused here rather than sent for the peer to refuse, so that the side with
/// the context is the side that reports it. [`FrameError::Malformed`] if the
/// value cannot be represented as JSON, and [`FrameError::Disconnected`] or
/// [`FrameError::Io`] if the connection failed.
pub fn write_message<W: Write, T: Serialize + ?Sized>(
    writer: &mut W,
    message: &T,
) -> Result<(), FrameError> {
    let payload = serde_json::to_vec(message).map_err(FrameError::Malformed)?;

    let length = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { declared: length });
    }

    // One write for the prefix and one for the payload would be two chances for
    // a peer to see half a frame, and on a pipe it is also two system calls per
    // message for no gain.
    let mut frame = Vec::with_capacity(LENGTH_PREFIX_BYTES + payload.len());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&payload);

    writer.write_all(&frame)?;
    writer.flush()?;
    Ok(())
}

/// Reads one message from one frame.
///
/// # Errors
///
/// [`FrameError::Disconnected`] when the peer closed at a frame boundary or
/// vanished inside one, [`FrameError::TooLarge`] when the length prefix exceeds
/// [`MAX_FRAME_BYTES`] — with nothing allocated and no payload byte read —
/// [`FrameError::Malformed`] when the payload is not the expected message, and
/// [`FrameError::Io`] for anything else the connection did.
pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> Result<T, FrameError> {
    let mut prefix = [0_u8; LENGTH_PREFIX_BYTES];
    read_prefix(reader, &mut prefix)?;

    let length = u32::from_le_bytes(prefix);
    // Before the allocation, and before the read. This order is the point of
    // the function.
    if length > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { declared: length });
    }
    // A zero-length frame needs no special case: it allocates nothing, reads
    // nothing, and `serde_json` reports an empty payload as the malformed
    // message it is.
    let mut payload = vec![0_u8; length as usize];
    reader.read_exact(&mut payload)?;

    serde_json::from_slice(&payload).map_err(FrameError::Malformed)
}

/// Reads the length prefix, telling a clean close apart from a truncated one.
///
/// A peer that closes between frames is expected; end of file on the very first
/// byte is what that looks like, and it is not an error. End of file *inside*
/// the prefix is a peer that died mid-write, which is
/// [`FrameError::Disconnected`] as well — there is nothing a caller could do
/// differently, and the two are not distinguishable in a useful way.
fn read_prefix<R: Read>(
    reader: &mut R,
    prefix: &mut [u8; LENGTH_PREFIX_BYTES],
) -> Result<(), FrameError> {
    let mut filled = 0;
    while filled < prefix.len() {
        match reader.read(&mut prefix[filled..]) {
            Ok(0) => return Err(FrameError::Disconnected),
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(FrameError::from(error)),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Serialize, Deserialize)]
    struct Sample {
        text: String,
    }

    fn sample() -> Sample {
        Sample {
            text: "hello".to_owned(),
        }
    }

    #[test]
    fn a_message_survives_a_round_trip_through_a_frame() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &sample()).expect("it writes");

        let mut reader = buffer.as_slice();
        let read: Sample = read_message(&mut reader).expect("it reads back");
        assert_eq!(read, sample());
        assert!(reader.is_empty(), "the frame's bounds should be exact");
    }

    #[test]
    fn two_messages_in_one_buffer_are_read_back_as_two() {
        // The whole point of framing: a reader must not need the messages to
        // arrive in separate writes, because a pipe gives no such guarantee.
        let mut buffer = Vec::new();
        write_message(&mut buffer, &sample()).expect("it writes");
        write_message(
            &mut buffer,
            &Sample {
                text: "second".to_owned(),
            },
        )
        .expect("it writes");

        let mut reader = buffer.as_slice();
        assert_eq!(
            read_message::<_, Sample>(&mut reader).expect("the first"),
            sample()
        );
        assert_eq!(
            read_message::<_, Sample>(&mut reader)
                .expect("the second")
                .text,
            "second"
        );
    }

    #[test]
    fn a_close_at_a_frame_boundary_is_a_disconnect_and_not_an_error() {
        let mut reader: &[u8] = &[];
        let error = read_message::<_, Sample>(&mut reader).expect_err("there is nothing there");
        assert!(error.is_disconnect(), "unexpected error: {error}");
    }

    #[test]
    fn a_peer_that_died_half_way_through_a_frame_is_a_disconnect() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &sample()).expect("it writes");
        buffer.truncate(buffer.len() - 3);

        let mut reader = buffer.as_slice();
        let error = read_message::<_, Sample>(&mut reader).expect_err("the frame is truncated");
        assert!(error.is_disconnect(), "unexpected error: {error}");
    }

    /// A reader that hands over a length prefix and then fails loudly if
    /// anybody tries to read the payload it promised.
    struct PrefixOnly {
        prefix: Vec<u8>,
        read_past_the_prefix: bool,
    }

    impl Read for PrefixOnly {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.prefix.is_empty() {
                self.read_past_the_prefix = true;
                return Ok(0);
            }
            let taken = self.prefix.len().min(buffer.len());
            buffer[..taken].copy_from_slice(&self.prefix[..taken]);
            self.prefix.drain(..taken);
            Ok(taken)
        }
    }

    #[test]
    fn an_enormous_length_prefix_is_refused_before_anything_is_allocated_or_read() {
        // The regression this guards: a peer sends 0xFFFFFFFF and the reader
        // obligingly asks for four gigabytes. The assertion that nothing was
        // read past the prefix is what proves the check happens first, since
        // "did not allocate" cannot be observed directly.
        let mut reader = PrefixOnly {
            prefix: u32::MAX.to_le_bytes().to_vec(),
            read_past_the_prefix: false,
        };

        let error = read_message::<_, Sample>(&mut reader).expect_err("that is far too large");
        match error {
            FrameError::TooLarge { declared } => assert_eq!(declared, u32::MAX),
            other => panic!("unexpected error: {other}"),
        }
        assert!(
            !reader.read_past_the_prefix,
            "the payload must not be touched once the length has been refused"
        );
    }

    #[test]
    fn a_length_one_byte_over_the_limit_is_refused_too() {
        let mut reader = PrefixOnly {
            prefix: (MAX_FRAME_BYTES + 1).to_le_bytes().to_vec(),
            read_past_the_prefix: false,
        };
        assert!(matches!(
            read_message::<_, Sample>(&mut reader),
            Err(FrameError::TooLarge { .. })
        ));
        assert!(!reader.read_past_the_prefix);
    }

    #[test]
    fn a_frame_that_is_not_json_is_malformed_rather_than_a_disconnect() {
        let payload = b"{not json";
        let mut buffer = (payload.len() as u32).to_le_bytes().to_vec();
        buffer.extend_from_slice(payload);

        let mut reader = buffer.as_slice();
        let error = read_message::<_, Sample>(&mut reader).expect_err("that is not a message");
        assert!(matches!(error, FrameError::Malformed(_)), "{error}");
        assert!(!error.is_disconnect());
    }

    #[test]
    fn an_empty_frame_is_malformed() {
        let buffer = 0_u32.to_le_bytes().to_vec();
        let mut reader = buffer.as_slice();
        assert!(matches!(
            read_message::<_, Sample>(&mut reader),
            Err(FrameError::Malformed(_))
        ));
    }

    #[test]
    fn a_message_larger_than_the_limit_is_refused_by_the_sender() {
        // Refused here rather than sent, so the side that knows what it was
        // trying to say is the side that reports it.
        let oversized = Sample {
            text: "x".repeat(MAX_FRAME_BYTES as usize),
        };
        let mut buffer = Vec::new();
        assert!(matches!(
            write_message(&mut buffer, &oversized),
            Err(FrameError::TooLarge { .. })
        ));
        assert!(buffer.is_empty(), "nothing should have reached the peer");
    }
}

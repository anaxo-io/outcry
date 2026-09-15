//! Error type.

/// Result alias used throughout the crate.
pub type Result<T> = std::result::Result<T, Error>;

/// Something went wrong creating, attaching to, or using a queue.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The writer got more than a full queue ahead of this reader, so the bytes under
    /// the reader's position have been overwritten.
    ///
    /// Nothing was returned. The reader's position is now meaningless; call
    /// [`Consumer::resync`](crate::Consumer::resync) to jump to the head of the stream
    /// and accept the loss.
    #[error(
        "overrun: the writer is {behind} bytes ahead of this reader, more than the queue holds"
    )]
    Overrun {
        /// How far the writer's reservation is ahead of the reader, in bytes.
        behind: u64,
    },

    /// The caller's buffer cannot hold the next frame. The reader did not advance.
    #[error("buffer of {provided} bytes cannot hold a {needed}-byte frame")]
    BufferTooSmall {
        /// Size of the frame waiting to be read.
        needed: usize,
        /// Size of the buffer the caller passed.
        provided: usize,
    },

    /// The message is larger than a queue of this capacity can carry.
    #[error("message of {len} bytes exceeds the maximum of {max} for this queue")]
    MessageTooLarge {
        /// Length the caller asked to write.
        len: usize,
        /// Largest payload this queue accepts.
        max: usize,
    },

    /// The capacity is not a power of two, or too small to hold a frame.
    #[error("capacity {0} must be a power of two and at least {min}", min = crate::layout::MIN_CAPACITY)]
    BadCapacity(u64),

    /// The mapped file does not contain a queue this crate understands.
    #[error("not an outcry queue: {0}")]
    BadHeader(&'static str),

    /// The queue file could not be created, opened, sized or mapped.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

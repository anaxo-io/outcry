//! On-disk (on-page) layout.
//!
//! ```text
//! offset 0    RawHeader      magic, version, capacity          (64 bytes, one cache line)
//! offset 64   published      AtomicU64, written by the producer (own cache line)
//! offset 128  reserved       AtomicU64, written by the producer (own cache line)
//! offset 192  buffer[capacity]
//! ```
//!
//! Every position in the queue is a monotonically increasing byte offset. The ring is
//! implicit: `offset & (capacity - 1)` is the index into `buffer`. Frames are an 8-byte
//! length header followed by the payload padded to 8 bytes, and a frame never straddles
//! the end of the buffer: when one would, the producer writes a pad frame that covers the
//! remainder and continues from the start.

use bytemuck::{Pod, Zeroable};

/// Positions, lengths and the capacity are 64-bit on the page. A narrower `usize` would
/// truncate them on the way into the pointer arithmetic, so the crate does not build
/// where that is possible.
const _: () = assert!(usize::BITS >= 64);

/// Bytes at the front of the mapping identifying it as ours.
pub const MAGIC: u64 = u64::from_le_bytes(*b"OUTCRY\x00\x01");
/// Layout version. Bump when the on-page format changes.
pub const VERSION: u32 = 1;

/// Size of a cache line on every platform this targets.
pub const CACHE_LINE: usize = 64;
/// Frames and their payloads are padded to this.
pub const FRAME_ALIGN: usize = 8;
/// Size of the length prefix in front of each payload.
pub const FRAME_HEADER: usize = 8;
/// Length value marking a pad frame that runs to the end of the buffer.
pub const PAD: u64 = u64::MAX;

/// Offset of the `published` counter.
pub const PUBLISHED_OFFSET: usize = CACHE_LINE;
/// Offset of the `reserved` counter.
pub const RESERVED_OFFSET: usize = 2 * CACHE_LINE;
/// Offset of the first buffer byte.
pub const BUFFER_OFFSET: usize = 3 * CACHE_LINE;

/// Smallest capacity accepted, so at least a few frames fit.
pub const MIN_CAPACITY: u64 = 4096;

/// How far ahead the producer reserves before touching the `reserved` cache line again.
///
/// The producer publishes `reserved` in blocks of this size rather than per message, so
/// readers see a slightly larger in-progress region than reality. That is safe — the
/// overrun check is conservative in the reader's favour — and it keeps the writer off a
/// shared cache line on the hot path. The cost is that a reader is treated as overrun
/// once it is `capacity - reserve_block` behind rather than a full `capacity`, so the
/// block is a fixed fraction of the ring: a reader always keeps 15/16 of it as slack.
#[inline]
pub const fn reserve_block(capacity: u64) -> u64 {
    let b = capacity / 16;
    if b < FRAME_ALIGN as u64 {
        FRAME_ALIGN as u64
    } else {
        b
    }
}

/// Fixed part of the header. Plain old data so it can be checked with `bytemuck`.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable, Debug, PartialEq, Eq)]
pub struct RawHeader {
    /// [`MAGIC`].
    pub magic: u64,
    /// [`VERSION`].
    pub version: u32,
    /// [`FRAME_ALIGN`], recorded so a reader can refuse a mismatched layout.
    pub frame_align: u32,
    /// Buffer size in bytes; a power of two.
    pub capacity: u64,
    /// Reserved for future use; zero.
    pub _reserved: [u64; 5],
}

const _: () = assert!(std::mem::size_of::<RawHeader>() == CACHE_LINE);

/// Round `v` up to a multiple of `align`, which must be a power of two.
#[inline]
pub const fn align_up(v: u64, align: u64) -> u64 {
    (v + (align - 1)) & !(align - 1)
}

/// Bytes a payload of `len` occupies on the ring, header included.
#[inline]
pub const fn frame_size(len: usize) -> u64 {
    FRAME_HEADER as u64 + align_up(len as u64, FRAME_ALIGN as u64)
}

/// Whether `capacity` is acceptable.
pub const fn capacity_ok(capacity: u64) -> bool {
    capacity >= MIN_CAPACITY && capacity.is_power_of_two()
}

/// Largest payload a queue of `capacity` can carry.
///
/// A frame must fit in the ring with room for a pad frame in front of it, and must be
/// strictly smaller than the ring so a single write can never lap the reader that is
/// consuming it.
pub const fn max_payload(capacity: u64) -> usize {
    (capacity / 2 - FRAME_HEADER as u64 - FRAME_HEADER as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_is_one_cache_line() {
        assert_eq!(std::mem::size_of::<RawHeader>(), 64);
        assert_eq!(std::mem::align_of::<RawHeader>(), 8);
    }

    #[test]
    fn align_up_rounds() {
        assert_eq!(align_up(0, 8), 0);
        assert_eq!(align_up(1, 8), 8);
        assert_eq!(align_up(8, 8), 8);
        assert_eq!(align_up(9, 8), 16);
        assert_eq!(align_up(4097, 4096), 8192);
    }

    #[test]
    fn frame_size_includes_header_and_padding() {
        assert_eq!(frame_size(0), 8);
        assert_eq!(frame_size(1), 16);
        assert_eq!(frame_size(8), 16);
        assert_eq!(frame_size(73), 8 + 80);
    }

    #[test]
    fn reserve_block_is_a_fraction_of_the_ring() {
        assert_eq!(reserve_block(4096), 256);
        assert_eq!(reserve_block(8 << 20), 512 * 1024);
        assert!(reserve_block(MIN_CAPACITY) < MIN_CAPACITY / 2);
    }

    #[test]
    fn capacity_rules() {
        assert!(capacity_ok(4096));
        assert!(capacity_ok(1 << 23));
        assert!(!capacity_ok(4095));
        assert!(!capacity_ok(6000));
        assert!(!capacity_ok(2048));
    }
}

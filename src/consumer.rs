//! A reader. Any number may exist; each sees every frame.

use std::sync::atomic::{fence, Ordering};
use std::sync::Arc;

use crate::copy;
use crate::error::{Error, Result};
use crate::layout::{frame_size, FRAME_HEADER, PAD};
use crate::mapping::Mapping;

/// Reads frames from a queue. Independent of every other consumer.
///
/// A consumer created after the producer has written sees only what is written from
/// then on: it starts at the current head, not at zero.
pub struct Consumer {
    map: Arc<Mapping>,
    /// Bytes consumed so far.
    local: u64,
    /// Last value of `published` observed; re-read only when caught up.
    cached_published: u64,
    /// Set by an overrun; cleared by `resync`.
    overrun: bool,
}

impl Consumer {
    pub(crate) fn new(map: Arc<Mapping>) -> Self {
        let head = map.published().load(Ordering::Acquire);
        Self {
            map,
            local: head,
            cached_published: head,
            overrun: false,
        }
    }

    /// Bytes consumed so far. Compare with [`Producer::position`](crate::Producer::position)
    /// to see how far behind this reader is.
    pub fn position(&self) -> u64 {
        self.local
    }

    /// Whether the last read hit an overrun and the reader has not resynced since.
    pub fn is_overrun(&self) -> bool {
        self.overrun
    }

    /// Jump to the head of the stream, abandoning everything between.
    ///
    /// The only way out of [`Error::Overrun`]. Returns how many bytes were skipped.
    pub fn resync(&mut self) -> u64 {
        let head = self.map.published().load(Ordering::Acquire);
        let skipped = head.saturating_sub(self.local);
        self.local = head;
        self.cached_published = head;
        self.overrun = false;
        skipped
    }

    /// Read the next frame into `out`.
    ///
    /// Returns `Ok(None)` if nothing new has been published, `Ok(Some(len))` with the
    /// payload's length after copying it into `out[..len]`, or an error:
    ///
    /// - [`Error::BufferTooSmall`] — `out` cannot hold the frame; nothing was consumed.
    /// - [`Error::Overrun`] — the writer lapped this reader. `out` holds garbage and the
    ///   position is invalid until [`resync`](Self::resync).
    ///
    /// Never blocks.
    pub fn try_read(&mut self, out: &mut [u8]) -> Result<Option<usize>> {
        if self.overrun {
            return Err(self.overrun_error());
        }

        loop {
            // Only touch the shared `published` line when we have run out of what we
            // last saw. While the writer is ahead, this costs nothing.
            if self.local == self.cached_published {
                self.cached_published = self.map.published().load(Ordering::Acquire);
                if self.local == self.cached_published {
                    return Ok(None);
                }
            }

            let cap = self.map.capacity();
            let mask = self.map.mask();
            let base = self.map.buffer_ptr();
            let start = self.local;
            let idx = (start & mask) as usize;

            // Check before reading anything: has the writer's reservation passed one
            // full ring beyond where we are about to read?
            let reserved = self.map.reserved().load(Ordering::Acquire);
            if reserved.wrapping_sub(start) > cap {
                return Err(self.mark_overrun(reserved));
            }

            // SAFETY: `idx` is 8-aligned and within the ring.
            let header = unsafe { copy::read_word(base.add(idx)) };

            if header == PAD {
                // The rest of this lap is padding; continue from the start of the next.
                self.local = (start | mask) + 1;
                continue;
            }

            // Validate the length before doing arithmetic on it: `frame_size` rounds up
            // to the frame alignment, which overflows near `u64::MAX`, and a length that
            // cannot be real means we read a torn, stale or corrupt header.
            let len = header as usize;
            if len > crate::layout::max_payload(cap) {
                return Err(self.mark_overrun(reserved));
            }
            let size = frame_size(len);
            // The producer pads rather than straddle the end of the ring, so a frame that
            // does not fit between here and the end was never written by one. Without
            // this the copy below runs past the mapping.
            if size > cap - idx as u64 {
                return Err(self.mark_overrun(reserved));
            }
            if len > out.len() {
                return Err(Error::BufferTooSmall {
                    needed: len,
                    provided: out.len(),
                });
            }

            // SAFETY: the frame lies within the ring by the producer's construction; the
            // bytes may be concurrently overwritten, which the check below detects.
            unsafe { copy::read(&mut out[..len], base.add(idx + FRAME_HEADER)) };

            // Check again, against the *start* of what we read: if the reservation has
            // passed `start + cap` at any point during the copy, some of those bytes may
            // have been rewritten under us and `out` cannot be trusted.
            //
            // The fence matters. An acquire *load* orders only what comes after it; without
            // the fence the copy above may legally be sunk below this check, and the check
            // would then be testing nothing. With it, any data load that observed a byte
            // of an overwriting frame synchronises with the writer's release fence, so the
            // reservation covering that frame is visible here.
            //
            // The original checks against the advanced position, which leaves a window of
            // one frame in which an overwrite of the frame's first bytes goes undetected.
            fence(Ordering::Acquire);
            let reserved = self.map.reserved().load(Ordering::Acquire);
            if reserved.wrapping_sub(start) > cap {
                return Err(self.mark_overrun(reserved));
            }

            self.local = start + size;
            return Ok(Some(len));
        }
    }

    fn mark_overrun(&mut self, reserved: u64) -> Error {
        self.overrun = true;
        Error::Overrun {
            behind: reserved.wrapping_sub(self.local),
        }
    }

    fn overrun_error(&self) -> Error {
        Error::Overrun {
            behind: self
                .map
                .reserved()
                .load(Ordering::Acquire)
                .wrapping_sub(self.local),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::layout::{FRAME_HEADER, PAD};

    const CAP: u64 = 4096;

    /// An anonymous queue whose counters have been set by hand, as a corrupted or hostile
    /// peer could leave them. Anonymous rather than file-backed so that these run under
    /// Miri, which is the only tool that sees the undefined behaviour these guards exist
    /// to prevent: the reads in question land inside the mapping, where a sanitiser
    /// cannot tell them from valid ones.
    fn crafted(published: u64) -> Arc<Mapping> {
        let map = Mapping::anon(CAP).unwrap();
        map.published().store(published, Ordering::Release);
        map.reserved().store(published, Ordering::Release);
        Arc::new(map)
    }

    fn publish(map: &Mapping, to: u64) {
        map.reserved().store(to, Ordering::Release);
        map.published().store(to, Ordering::Release);
    }

    #[test]
    fn an_absurd_length_is_refused_before_it_is_arithmetic() {
        let map = crafted(0);
        let mut consumer = Consumer::new(Arc::clone(&map));

        // One below the pad marker: `frame_size` would overflow rounding it up.
        map.poke_ring(0, u64::MAX - 1);
        publish(&map, 16);

        let mut buf = [0u8; 64];
        assert!(matches!(
            consumer.try_read(&mut buf),
            Err(Error::Overrun { .. })
        ));
    }

    #[test]
    fn a_frame_running_past_the_ring_end_is_refused() {
        // Eight bytes from the end: only a length word fits, so a compliant producer
        // would have written a pad frame here.
        let map = crafted(CAP - 8);
        let mut consumer = Consumer::new(Arc::clone(&map));

        // A peer claims a 16-byte frame anyway. Its payload would start at ring index
        // `CAP`, one word past the end of the mapping.
        map.poke_ring((CAP - 8) as usize, 1);
        publish(&map, CAP + 8);

        let mut buf = [0u8; 64];
        assert!(matches!(
            consumer.try_read(&mut buf),
            Err(Error::Overrun { .. })
        ));
    }

    #[test]
    fn a_length_beyond_the_maximum_payload_is_refused() {
        let map = crafted(0);
        let mut consumer = Consumer::new(Arc::clone(&map));

        map.poke_ring(0, crate::layout::max_payload(CAP) as u64 + 1);
        publish(&map, CAP / 2);

        let mut buf = [0u8; 4096];
        assert!(matches!(
            consumer.try_read(&mut buf),
            Err(Error::Overrun { .. })
        ));
    }

    #[test]
    fn a_pad_word_at_the_end_of_the_ring_wraps_without_reading_past_it() {
        let map = crafted(CAP - 8);
        let mut consumer = Consumer::new(Arc::clone(&map));

        // Pad to the end of this lap, then a real empty frame at the start of the next.
        map.poke_ring((CAP - 8) as usize, PAD);
        map.poke_ring(0, 0);
        publish(&map, CAP + FRAME_HEADER as u64);

        let mut buf = [0u8; 64];
        assert_eq!(consumer.try_read(&mut buf).unwrap(), Some(0));
    }

    #[test]
    fn a_well_formed_frame_is_still_accepted() {
        // The control: the guards must refuse corruption without refusing real frames.
        let map = crafted(0);
        let mut consumer = Consumer::new(Arc::clone(&map));

        map.poke_ring(0, 8);
        map.poke_ring(FRAME_HEADER, u64::from_ne_bytes(*b"abcdefgh"));
        publish(&map, 16);

        let mut buf = [0u8; 64];
        assert_eq!(consumer.try_read(&mut buf).unwrap(), Some(8));
        assert_eq!(&buf[..8], b"abcdefgh");
    }
}

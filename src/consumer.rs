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

            let len = header as usize;
            let size = frame_size(len);
            if len > out.len() {
                return Err(Error::BufferTooSmall {
                    needed: len,
                    provided: out.len(),
                });
            }
            if size > cap || len > crate::layout::max_payload(cap) {
                // A length that cannot be real means we read a torn or stale header.
                return Err(self.mark_overrun(reserved));
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
            // reservation covering that frame is visible here. In `fast-copy` mode the
            // loads are not atomic and cannot take part in that synchronisation; the fence
            // still pins the copy above the check at the compiler level, and the hardware
            // does the rest on every platform this runs on. That is the trade the feature
            // buys.
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

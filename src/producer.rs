//! The single writer.

use std::sync::atomic::{fence, Ordering};
use std::sync::Arc;

use crate::copy;
use crate::error::{Error, Result};
use crate::layout::{align_up, frame_size, max_payload, reserve_block, FRAME_HEADER, PAD};
use crate::mapping::Mapping;

/// Writes frames to a queue. There must be exactly one per queue.
///
/// The queue cannot enforce that across processes; two producers on one queue corrupt
/// it. Within a process, [`Queue::producer`](crate::Queue::producer) hands out at most
/// one.
pub struct Producer {
    map: Arc<Mapping>,
    /// Bytes published so far. Only this struct advances it.
    local: u64,
    /// Where `reserved` in shared memory currently points; stored ahead in blocks.
    cached_reserved: u64,
    /// Staging area for `write_with`, grown on demand and never shrunk, so the hot path
    /// does not allocate.
    scratch: Vec<u8>,
}

impl Producer {
    pub(crate) fn new(map: Arc<Mapping>) -> Self {
        let local = map.published().load(Ordering::Acquire);
        let cached_reserved = map.reserved().load(Ordering::Acquire);
        Self {
            map,
            local,
            cached_reserved,
            scratch: Vec::new(),
        }
    }

    /// Largest payload this queue accepts.
    pub fn max_payload(&self) -> usize {
        max_payload(self.map.capacity())
    }

    /// Bytes published so far. Monotonic; readers compare their own position to it.
    pub fn position(&self) -> u64 {
        self.local
    }

    /// Append one frame.
    ///
    /// Never blocks and never waits for readers. A reader that is more than a full queue
    /// behind will find its next read overrun. Allocation-free.
    pub fn write(&mut self, payload: &[u8]) -> Result<()> {
        self.write_frame(payload)
    }

    /// Append one frame of `len` bytes, letting `fill` write the payload in place.
    ///
    /// `fill` receives exactly `len` zeroed bytes of a buffer owned by the producer, so
    /// a serialiser can target it directly instead of building a `Vec` first. The buffer
    /// is reused across calls; only the first call at a given size allocates.
    pub fn write_with(&mut self, len: usize, fill: impl FnOnce(&mut [u8])) -> Result<()> {
        let max = self.max_payload();
        if len > max {
            return Err(Error::MessageTooLarge { len, max });
        }
        if self.scratch.len() < len {
            self.scratch.resize(len, 0);
        }
        let mut scratch = std::mem::take(&mut self.scratch);
        scratch[..len].fill(0);
        fill(&mut scratch[..len]);
        let result = self.write_frame(&scratch[..len]);
        self.scratch = scratch;
        result
    }

    fn write_frame(&mut self, payload: &[u8]) -> Result<()> {
        let len = payload.len();
        let max = self.max_payload();
        if len > max {
            return Err(Error::MessageTooLarge { len, max });
        }

        let cap = self.map.capacity();
        let mask = self.map.mask();
        let size = frame_size(len);

        // A frame never straddles the end of the ring. If this one would, emit a pad
        // frame covering the remainder and continue from the start.
        let mut start = self.local;
        let idx = start & mask;
        let room = cap - idx;
        let pad = if size > room { room } else { 0 };
        let end = start + pad + size;

        // Reserve. Readers use `reserved` to detect that they have been lapped, so it
        // must be visible before any byte of the frame is written. Storing it in blocks
        // keeps the writer off this cache line on most messages.
        if end > self.cached_reserved {
            self.cached_reserved = align_up(end, reserve_block(cap));
            self.map
                .reserved()
                .store(self.cached_reserved, Ordering::Release);
        }
        // A release *store* orders the stores before it, not the data stores after it.
        // This fence is what guarantees a reader that observes any byte of this frame
        // will, after its own acquire fence, also observe the reservation that covers
        // it — the property the overrun check depends on. (Boehm, "Can seqlocks get
        // along with programming language memory models?", §4.)
        fence(Ordering::Release);

        let base = self.map.buffer_ptr();

        if pad != 0 {
            // SAFETY: `idx` is 8-aligned and in bounds; we are the sole producer.
            unsafe { copy::write_word(base.add(idx as usize), PAD) };
            start += pad;
        }

        let fidx = (start & mask) as usize;
        // SAFETY: the frame fits between `fidx` and the end of the ring by construction
        // above; we are the sole producer; `local`..`end` is reserved.
        unsafe {
            copy::write_word(base.add(fidx), len as u64);
            copy::write(base.add(fidx + FRAME_HEADER), payload);
        }

        // Publish. Release orders every store above before this one.
        self.local = end;
        self.map.published().store(end, Ordering::Release);
        Ok(())
    }
}

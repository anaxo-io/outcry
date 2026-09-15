//! The bytes behind a queue: a file in shared memory, or anonymous memory in one process.

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};

use memmap2::MmapMut;

use crate::error::{Error, Result};
use crate::layout::{
    self, RawHeader, BUFFER_OFFSET, FRAME_ALIGN, MAGIC, PUBLISHED_OFFSET, RESERVED_OFFSET, VERSION,
};

/// A mapped queue region. Shared between the [`Queue`](crate::Queue) handle and every
/// producer or consumer made from it.
pub(crate) struct Mapping {
    /// Held only to keep the mapping alive; never dereferenced after construction.
    _map: MmapMut,
    /// The one pointer every access goes through. Taken from `as_mut_ptr()` while the
    /// mapping was uniquely borrowed, so it carries write permission. Re-borrowing the
    /// bytes through `&self._map[..]` would produce a read-only pointer, and writing
    /// through that is undefined behaviour — Miri catches it, hardware does not.
    base: *mut u8,
    capacity: u64,
}

// SAFETY: `base` is a raw pointer, which is why these are not automatic. Every access to
// the mapped bytes goes through atomics or through the copy routines in `copy.rs`, which
// are the documented concurrency boundary of the crate; the pointer itself is never
// mutated after construction.
unsafe impl Send for Mapping {}
unsafe impl Sync for Mapping {}

impl Mapping {
    fn total_len(capacity: u64) -> usize {
        BUFFER_OFFSET + capacity as usize
    }

    /// Create a fresh queue file at `path`, truncating any existing one.
    pub(crate) fn create(path: &Path, capacity: u64) -> Result<Self> {
        if !layout::capacity_ok(capacity) {
            return Err(Error::BadCapacity(capacity));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)?;
        file.set_len(Self::total_len(capacity) as u64)?;
        // SAFETY: the file is ours and sized; nothing else has mapped it yet. A concurrent
        // truncation by another process would be a SIGBUS, which is the documented
        // hazard of file-backed mappings and out of scope.
        let mut map = unsafe { MmapMut::map_mut(&file)? };
        map.fill(0);
        let base = map.as_mut_ptr();
        let this = Self {
            _map: map,
            base,
            capacity,
        };
        this.write_header();
        Ok(this)
    }

    /// Anonymous memory: a queue visible only inside this process.
    pub(crate) fn anon(capacity: u64) -> Result<Self> {
        if !layout::capacity_ok(capacity) {
            return Err(Error::BadCapacity(capacity));
        }
        let mut map = MmapMut::map_anon(Self::total_len(capacity))?;
        let base = map.as_mut_ptr();
        let this = Self {
            _map: map,
            base,
            capacity,
        };
        this.write_header();
        Ok(this)
    }

    /// Attach to an existing queue file and verify its header.
    pub(crate) fn open(path: &Path) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(path)?;
        let len = file.metadata()?.len() as usize;
        if len < BUFFER_OFFSET {
            return Err(Error::BadHeader("file too small for a header"));
        }
        // SAFETY: as in `create`.
        let mut map = unsafe { MmapMut::map_mut(&file)? };

        let header: RawHeader =
            bytemuck::pod_read_unaligned(&map[..std::mem::size_of::<RawHeader>()]);
        if header.magic != MAGIC {
            return Err(Error::BadHeader("magic mismatch"));
        }
        if header.version != VERSION {
            return Err(Error::BadHeader("layout version mismatch"));
        }
        if header.frame_align != FRAME_ALIGN as u32 {
            return Err(Error::BadHeader("frame alignment mismatch"));
        }
        if !layout::capacity_ok(header.capacity) {
            return Err(Error::BadHeader("capacity is not a power of two"));
        }
        if len < Self::total_len(header.capacity) {
            return Err(Error::BadHeader("file shorter than its declared capacity"));
        }
        let base = map.as_mut_ptr();
        Ok(Self {
            _map: map,
            base,
            capacity: header.capacity,
        })
    }

    fn write_header(&self) {
        let header = RawHeader {
            magic: 0, // written last, below, so a reader never sees a half-written header
            version: VERSION,
            frame_align: FRAME_ALIGN as u32,
            capacity: self.capacity,
            _reserved: [0; 5],
        };
        let bytes = bytemuck::bytes_of(&header);
        // SAFETY: `base` points at a mapping of at least `BUFFER_OFFSET` bytes, we hold
        // the only handle, and nothing else has been given a pointer yet.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), self.base, bytes.len()) };
        // Publish the magic with a release store so the rest of the header is visible to
        // anyone who observes it.
        self.magic().store(MAGIC, Ordering::Release);
    }

    fn atomic_at(&self, offset: usize) -> &AtomicU64 {
        debug_assert!(offset + 8 <= BUFFER_OFFSET);
        // SAFETY: the offset is 8-aligned (all three are multiples of 64 into a page-aligned
        // mapping), in bounds, `AtomicU64` has the same layout as `u64`, and `base` carries
        // write permission. The mapping outlives the returned reference.
        unsafe { &*(self.base.add(offset) as *const AtomicU64) }
    }

    pub(crate) fn magic(&self) -> &AtomicU64 {
        self.atomic_at(0)
    }
    pub(crate) fn published(&self) -> &AtomicU64 {
        self.atomic_at(PUBLISHED_OFFSET)
    }
    pub(crate) fn reserved(&self) -> &AtomicU64 {
        self.atomic_at(RESERVED_OFFSET)
    }
    pub(crate) fn capacity(&self) -> u64 {
        self.capacity
    }
    pub(crate) fn mask(&self) -> u64 {
        self.capacity - 1
    }

    /// Pointer to the first buffer byte. All ring arithmetic is relative to this.
    pub(crate) fn buffer_ptr(&self) -> *mut u8 {
        // SAFETY: in bounds; the mapping is `BUFFER_OFFSET + capacity` bytes.
        unsafe { self.base.add(BUFFER_OFFSET) }
    }
}

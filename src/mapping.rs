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
    map: MmapMut,
    capacity: u64,
}

// SAFETY: every access to the mapped bytes goes through atomics or through the copy
// routines in `copy.rs`, which are the documented concurrency boundary of the crate.
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
        let mut this = Self { map, capacity };
        this.write_header();
        Ok(this)
    }

    /// Anonymous memory: a queue visible only inside this process.
    pub(crate) fn anon(capacity: u64) -> Result<Self> {
        if !layout::capacity_ok(capacity) {
            return Err(Error::BadCapacity(capacity));
        }
        let map = MmapMut::map_anon(Self::total_len(capacity))?;
        let mut this = Self { map, capacity };
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
        let map = unsafe { MmapMut::map_mut(&file)? };

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
        Ok(Self {
            map,
            capacity: header.capacity,
        })
    }

    fn write_header(&mut self) {
        let header = RawHeader {
            magic: 0, // written last, below, so a reader never sees a half-written header
            version: VERSION,
            frame_align: FRAME_ALIGN as u32,
            capacity: self.capacity,
            _reserved: [0; 5],
        };
        self.map[..std::mem::size_of::<RawHeader>()].copy_from_slice(bytemuck::bytes_of(&header));
        // Publish the magic with a release store so the rest of the header is visible to
        // anyone who observes it.
        self.magic().store(MAGIC, Ordering::Release);
    }

    fn atomic_at(&self, offset: usize) -> &AtomicU64 {
        let ptr = self.map[offset..offset + 8].as_ptr();
        debug_assert_eq!(ptr as usize % 8, 0);
        // SAFETY: the offset is 8-aligned (all three are multiples of 64 into a page-aligned
        // mapping), in bounds, and `AtomicU64` has the same layout as `u64`. The mapping
        // outlives the returned reference.
        unsafe { &*(ptr as *const AtomicU64) }
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
        self.map[BUFFER_OFFSET..].as_ptr() as *mut u8
    }
}

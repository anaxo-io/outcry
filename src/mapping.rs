//! The bytes behind a queue: a file in shared memory, or anonymous memory in one process.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

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
    /// Value of [`RawHeader::instance`] for the queue this maps.
    instance: u64,
    /// The open file this maps, kept alive for the lock in [`Mapping::claim_producer`].
    /// `None` for anonymous memory, which no other process can reach.
    file: Option<File>,
}

/// A fresh identifier for a queue: nanoseconds since the epoch. Two queues created at the
/// same path are always separated by at least a file creation and an 8 MiB zero fill, so
/// consecutive values cannot collide.
fn new_instance() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or_default()
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

    /// Create a fresh queue file at `path`, atomically replacing any existing one.
    ///
    /// The new queue is built in a temporary file beside the target and `rename`d into
    /// place. The rename is atomic, so a concurrent `open` sees either the old queue or a
    /// complete new one; and it gives the new queue a new inode, so a reader mapped to the
    /// previous file keeps valid pages. Truncating in place instead would leave every such
    /// reader pointing past end of file, and the first byte it touched would raise
    /// `SIGBUS` — a signal, not an error it could handle.
    pub(crate) fn create(path: &Path, capacity: u64) -> Result<Self> {
        if !layout::capacity_ok(capacity) {
            return Err(Error::BadCapacity(capacity));
        }
        let tmp = Self::temp_path(path)?;
        let built = Self::build(&tmp, capacity).and_then(|this| {
            std::fs::rename(&tmp, path)?;
            Ok(this)
        });
        if built.is_err() {
            // Nothing is published at `path`, so leave no half-built file behind either.
            let _ = std::fs::remove_file(&tmp);
        }
        built
    }

    /// A sibling of `path`, in the same directory so that `rename` stays within one
    /// filesystem and therefore stays atomic.
    fn temp_path(path: &Path) -> Result<PathBuf> {
        let dir = match path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let name = path.file_name().ok_or_else(|| {
            Error::Io(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "queue path does not name a file",
            ))
        })?;
        Ok(dir.join(format!(
            ".{}.{}.{}.tmp",
            name.to_string_lossy(),
            std::process::id(),
            new_instance()
        )))
    }

    /// Build a complete queue in a file that nothing else has seen yet.
    fn build(path: &Path, capacity: u64) -> Result<Self> {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .open(path)?;
        file.set_len(Self::total_len(capacity) as u64)?;
        // SAFETY: the file is ours, freshly created under a name nothing else knows, and
        // sized; nothing else has mapped it.
        let mut map = unsafe { MmapMut::map_mut(&file)? };
        map.fill(0);
        let base = map.as_mut_ptr();
        let this = Self {
            _map: map,
            base,
            capacity,
            instance: new_instance(),
            file: Some(file),
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
            instance: new_instance(),
            file: None,
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
        let base = map.as_mut_ptr();

        // The magic is the creator's release store; load it with acquire before reading
        // anything else, so the rest of the header is visible if it is there at all.
        // Everything below goes through `base` rather than `&map[..]`: a shared reborrow
        // of the mapping would strip the pointer's write permission.
        //
        // SAFETY: offset 0 of a page-aligned mapping of at least `BUFFER_OFFSET` bytes is
        // an 8-aligned in-bounds word, and `AtomicU64` has the same layout as `u64`.
        let magic = unsafe { (*(base as *const AtomicU64)).load(Ordering::Acquire) };
        if magic != MAGIC {
            return Err(Error::BadHeader("magic mismatch"));
        }
        // SAFETY: the mapping is at least `BUFFER_OFFSET` bytes, which is larger than
        // `RawHeader`, and `RawHeader` is `Pod`: every bit pattern is a valid value.
        let header: RawHeader = unsafe { base.cast::<RawHeader>().read_unaligned() };
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
        let this = Self {
            _map: map,
            base,
            capacity: header.capacity,
            instance: header.instance,
            file: Some(file),
        };

        // The counters are live state, not layout, and everything downstream trusts
        // them: a producer starts writing at `published` and a consumer starts reading
        // there. An unaligned position makes every frame word an unaligned atomic, which
        // is undefined behaviour reached through an entirely safe call. Refuse any pair
        // the producer could not have left behind.
        let published = this.published().load(Ordering::Acquire);
        let reserved = this.reserved().load(Ordering::Acquire);
        if !published.is_multiple_of(FRAME_ALIGN as u64)
            || !reserved.is_multiple_of(FRAME_ALIGN as u64)
        {
            return Err(Error::BadHeader("position is not frame-aligned"));
        }
        if reserved < published || reserved - published > header.capacity {
            return Err(Error::BadHeader(
                "reservation does not cover the published end",
            ));
        }
        Ok(this)
    }

    fn write_header(&self) {
        let header = RawHeader {
            magic: 0, // written last, below, so a reader never sees a half-written header
            version: VERSION,
            frame_align: FRAME_ALIGN as u32,
            capacity: self.capacity,
            instance: self.instance,
            _reserved: [0; 4],
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
    pub(crate) fn instance(&self) -> u64 {
        self.instance
    }

    /// Take the exclusive producer claim on this queue, if it is free.
    ///
    /// An advisory lock on the queue file, which is what makes the claim visible to other
    /// processes: two `Queue::open` calls on one file, in one process or in several, see
    /// each other here. The kernel releases the lock when the holding process dies, so a
    /// writer that crashed leaves nothing to clean up — the next one simply succeeds.
    ///
    /// The lock belongs to the file, not the path. After [`Mapping::create`] replaces a
    /// queue the new file is a different inode with its own lock, so a restarting writer
    /// that creates a fresh queue never contends with the old one. That is correct: they
    /// are two queues, and nothing interleaves.
    ///
    /// An anonymous mapping has no file and returns `true`; nothing outside this process
    /// can reach it, so the handle's own flag is guard enough.
    pub(crate) fn claim_producer(&self) -> bool {
        match &self.file {
            Some(file) => file.try_lock().is_ok(),
            None => true,
        }
    }

    /// Give the claim back, so a later producer on the same handle can take it.
    pub(crate) fn release_producer(&self) {
        if let Some(file) = &self.file {
            let _ = file.unlock();
        }
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

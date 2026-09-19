//! One writer, many readers, shared memory, nobody waits.
//!
//! `outcry` is a broadcast queue: a single producer appends variable-length frames and
//! any number of consumers each read every frame, in order, in their own time. Readers
//! never slow the writer down. A reader that falls more than a full queue behind is
//! **overrun** — it is told so, and it can resync to the head — but the writer never
//! knew it was there. The name is from *open outcry*: one voice on the floor, everyone
//! hears it, nobody repeats it for you.
//!
//! It is an implementation in Rust of the `FastQueue` design David Gross presented in
//! [*When Nanoseconds Matter: Ultrafast Trading Systems in C++*][talk] (CppCon 2024),
//! including its three cache-line optimisations, with one deliberate difference: the
//! reader's copy is sound rather than a data race caught after the fact. See [`copy`] for
//! why that costs nothing here.
//!
//! [talk]: https://www.youtube.com/watch?v=sX2nF1fW7kI
//!
//! # Example
//!
//! ```
//! use outcry::Queue;
//!
//! let queue = Queue::anon(1 << 20)?;          // 1 MiB, this process only
//! let mut producer = queue.producer()?;
//! let mut consumer = queue.consumer();
//!
//! producer.write(b"hello, floor")?;
//!
//! let mut buf = [0u8; 64];
//! let n = consumer.try_read(&mut buf)?.expect("one frame is waiting");
//! assert_eq!(&buf[..n], b"hello, floor");
//! assert_eq!(consumer.try_read(&mut buf)?, None);
//! # Ok::<(), outcry::Error>(())
//! ```
//!
//! Across processes, the producer calls [`Queue::create`] with a path (under `/dev/shm`
//! on Linux) and consumers call [`Queue::open`] on the same path.
//!
//! # What it is not
//!
//! Not for multiple writers, not a work queue (every reader gets every frame), not
//! durable, not cross-host. If you need any of those, this is the wrong tool and the
//! README says which is the right one.

#![warn(missing_docs)]
#![deny(rustdoc::broken_intra_doc_links)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod consumer;
pub mod copy;
pub mod error;
pub mod layout;
mod mapping;
pub mod producer;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub use consumer::Consumer;
pub use error::{Error, Result};
pub use producer::Producer;

/// A handle to a queue. Clone it freely; make producers and consumers from it.
#[derive(Clone)]
pub struct Queue {
    map: Arc<mapping::Mapping>,
    producer_taken: Arc<AtomicBool>,
}

impl Queue {
    /// Create a queue in a file at `path`, replacing any existing file.
    ///
    /// Put it in shared memory — `/dev/shm/<name>` on Linux — so the mapping never
    /// touches disk. `capacity` is the ring size in bytes; a power of two, at least
    /// [`layout::MIN_CAPACITY`].
    pub fn create(path: impl AsRef<Path>, capacity: u64) -> Result<Self> {
        Ok(Self::wrap(mapping::Mapping::create(
            path.as_ref(),
            capacity,
        )?))
    }

    /// Attach to a queue another process created at `path`.
    ///
    /// Fails if the file is not an `outcry` queue of this layout version.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        Ok(Self::wrap(mapping::Mapping::open(path.as_ref())?))
    }

    /// A queue in anonymous memory, visible only inside this process.
    pub fn anon(capacity: u64) -> Result<Self> {
        Ok(Self::wrap(mapping::Mapping::anon(capacity)?))
    }

    fn wrap(map: mapping::Mapping) -> Self {
        Self {
            map: Arc::new(map),
            producer_taken: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Ring size in bytes.
    pub fn capacity(&self) -> u64 {
        self.map.capacity()
    }

    /// Identifier assigned when this queue was created.
    ///
    /// [`Queue::create`] replaces the file at a path atomically rather than truncating
    /// it, so a reader that was attached across a writer restart keeps a valid mapping of
    /// the *previous* queue — one that is intact and will never change again. From inside
    /// that mapping, a replaced writer and a merely quiet one look identical. Comparing
    /// this value with the one currently at the path tells them apart:
    ///
    /// ```no_run
    /// # use outcry::Queue;
    /// # fn check(mine: &Queue, path: &str) -> Result<(), outcry::Error> {
    /// if Queue::open(path)?.instance() != mine.instance() {
    ///     // The writer restarted. Re-open and make a new consumer; anything the
    ///     // replacement queue published before now is already gone.
    /// }
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Nanoseconds since the epoch at the moment of creation, so it is also readable as a
    /// timestamp. [`Queue::anon`] assigns one too, though nothing can observe it from
    /// another handle.
    pub fn instance(&self) -> u64 {
        self.map.instance()
    }

    /// The producer. At most one per handle; a second call returns an error.
    ///
    /// This guards against two writers in one process. It cannot see another process:
    /// only one process may ever call this for a given file, by convention.
    pub fn producer(&self) -> Result<Producer> {
        if self.producer_taken.swap(true, Ordering::AcqRel) {
            return Err(Error::Io(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "this queue already has a producer",
            )));
        }
        Ok(Producer::new(Arc::clone(&self.map)))
    }

    /// A new consumer, starting at the current head of the stream.
    pub fn consumer(&self) -> Consumer {
        Consumer::new(Arc::clone(&self.map))
    }
}

//! Malformed shared state must not reach the unsafe code.
//!
//! Every test here crafts a queue file the way a hostile or corrupted peer could, then
//! drives the safe API over it. The unsafe blocks in `producer.rs` and `consumer.rs`
//! assume aligned, in-bounds protocol state; these tests are what establishes that
//! `Queue::open` and `Consumer::try_read` refuse anything else before that assumption
//! is used.
//!
//! File-backed, so this file is not run under Miri.

use std::fs::OpenOptions;
use std::os::unix::fs::FileExt;
use std::path::{Path, PathBuf};

use outcry::layout::{BUFFER_OFFSET, PUBLISHED_OFFSET, RESERVED_OFFSET};
use outcry::{Error, Queue};

const CAP: u64 = 4096;

/// A real queue file whose counters have been overwritten behind the crate's back.
fn crafted(dir: &Path, published: u64, reserved: u64) -> PathBuf {
    let path = dir.join("q");
    drop(Queue::create(&path, CAP).unwrap());
    poke(&path, PUBLISHED_OFFSET as u64, published);
    poke(&path, RESERVED_OFFSET as u64, reserved);
    path
}

/// Write one word into the file at `offset`, bypassing the queue entirely.
fn poke(path: &Path, offset: u64, value: u64) {
    let file = OpenOptions::new().write(true).open(path).unwrap();
    file.write_all_at(&value.to_le_bytes(), offset).unwrap();
}

/// Write one word into the ring at `idx`.
fn poke_ring(path: &Path, idx: u64, value: u64) {
    poke(path, BUFFER_OFFSET as u64 + idx, value);
}

#[test]
fn open_rejects_unaligned_published() {
    let dir = tempfile::tempdir().unwrap();
    let path = crafted(dir.path(), 1, 8);
    assert!(
        matches!(Queue::open(&path), Err(Error::BadHeader(_))),
        "an unaligned position makes every frame word an unaligned atomic"
    );
}

#[test]
fn open_rejects_unaligned_reserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = crafted(dir.path(), 0, 4);
    assert!(matches!(Queue::open(&path), Err(Error::BadHeader(_))));
}

#[test]
fn open_rejects_published_ahead_of_reserved() {
    let dir = tempfile::tempdir().unwrap();
    let path = crafted(dir.path(), 64, 0);
    assert!(
        matches!(Queue::open(&path), Err(Error::BadHeader(_))),
        "the producer reserves before it publishes; the reverse is corruption"
    );
}

#[test]
fn open_rejects_a_reservation_more_than_a_ring_ahead() {
    let dir = tempfile::tempdir().unwrap();
    let path = crafted(dir.path(), 0, CAP + 8);
    assert!(
        matches!(Queue::open(&path), Err(Error::BadHeader(_))),
        "no compliant producer reserves a full ring past what it published"
    );
}

#[test]
fn open_accepts_counters_mid_stream() {
    let dir = tempfile::tempdir().unwrap();
    let path = crafted(dir.path(), 8 * CAP + 64, 8 * CAP + 320);
    assert!(
        Queue::open(&path).is_ok(),
        "validation must not reject a queue that has simply been running"
    );
}

#[test]
fn a_frame_running_past_the_ring_end_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    // A consumer parked eight bytes from the end of the ring: only a length word fits
    // there, so a compliant producer would have written a pad frame.
    let path = crafted(dir.path(), CAP - 8, CAP - 8);
    let queue = Queue::open(&path).unwrap();
    let mut consumer = queue.consumer();

    // A peer claims a 16-byte frame at that position anyway. Its payload starts at ring
    // index `CAP`, one byte past the mapping.
    poke_ring(&path, CAP - 8, 1);
    poke(&path, RESERVED_OFFSET as u64, CAP + 8);
    poke(&path, PUBLISHED_OFFSET as u64, CAP + 8);

    let mut buf = [0u8; 64];
    assert!(
        matches!(consumer.try_read(&mut buf), Err(Error::Overrun { .. })),
        "a frame that does not fit between its start and the ring end is corruption"
    );
}

#[test]
fn an_absurd_length_is_refused_before_it_is_arithmetic() {
    let dir = tempfile::tempdir().unwrap();
    let path = crafted(dir.path(), 0, 0);
    let queue = Queue::open(&path).unwrap();
    let mut consumer = queue.consumer();

    // `PAD` is handled; one below it is not, and rounding it up to the frame alignment
    // overflows.
    poke_ring(&path, 0, u64::MAX - 1);
    poke(&path, RESERVED_OFFSET as u64, 16);
    poke(&path, PUBLISHED_OFFSET as u64, 16);

    let mut buf = [0u8; 64];
    assert!(
        matches!(consumer.try_read(&mut buf), Err(Error::Overrun { .. })),
        "a length no frame could have must be rejected before frame_size() sees it"
    );
}

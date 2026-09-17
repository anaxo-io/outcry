//! Moving bytes into and out of the ring.
//!
//! This is the crate's concurrency boundary, and the one place where the C++ original
//! and this crate part ways.
//!
//! In the original, the reader `memcpy`s a frame that the writer may be overwriting at
//! the same moment, and detects the collision afterwards through the overrun check. That
//! works on real hardware. It is also a data race in the abstract machine — the talk says
//! so on its own slide — and a data race is undefined behaviour in Rust exactly as in C++.
//!
//! So every byte of the ring is read and written through `AtomicU64` with `Relaxed`
//! ordering. Concurrent atomic accesses are never a data race, and the acquire/release on
//! the counters orders them. Correct by construction, at one atomic per 8 bytes — which on
//! x86-64 is one `mov` per 8 bytes.
//!
//! The `memcpy` version was implemented, benchmarked and removed. It was *slower* at every
//! reader count measured — 0.65x the throughput of this one with a single reader, 0.80x
//! with twelve — because at these payload sizes `copy_nonoverlapping` calls a generic
//! `memcpy` that dispatches on length, while this loop inlines to a handful of plain
//! `mov`s: a `Relaxed` load or store of a `u64` on x86-64 is exactly that. The race bought
//! nothing, so there is no trade to offer. The README has the numbers.

use std::sync::atomic::{AtomicU64, Ordering};

/// Copy `src` into the ring at `dst`, which must be 8-aligned with `src.len()` rounded up
/// to 8 available.
///
/// # Safety
/// `dst` must point into a live mapping with at least `align_up(src.len(), 8)` bytes, and
/// the caller must be the queue's single producer.
#[inline]
pub(crate) unsafe fn write(dst: *mut u8, src: &[u8]) {
    // SAFETY: preconditions are the function's documented contract.
    unsafe {
        let words = dst as *mut AtomicU64;
        let mut chunks = src.chunks_exact(8);
        let mut i = 0;
        for chunk in &mut chunks {
            let v = u64::from_ne_bytes(chunk.try_into().unwrap());
            (*words.add(i)).store(v, Ordering::Relaxed);
            i += 1;
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut last = [0u8; 8];
            last[..rest.len()].copy_from_slice(rest);
            (*words.add(i)).store(u64::from_ne_bytes(last), Ordering::Relaxed);
        }
    }
}

/// Copy `dst.len()` bytes out of the ring at `src`, which must be 8-aligned.
///
/// # Safety
/// `src` must point into a live mapping with at least `align_up(dst.len(), 8)` bytes.
/// The bytes may be concurrently overwritten by the producer; the caller must validate
/// the read afterwards with the overrun check and discard `dst` on failure.
#[inline]
pub(crate) unsafe fn read(dst: &mut [u8], src: *const u8) {
    // SAFETY: preconditions are the function's documented contract.
    unsafe {
        let words = src as *const AtomicU64;
        let mut chunks = dst.chunks_exact_mut(8);
        let mut i = 0;
        for chunk in &mut chunks {
            let v = (*words.add(i)).load(Ordering::Relaxed);
            chunk.copy_from_slice(&v.to_ne_bytes());
            i += 1;
        }
        let rest = chunks.into_remainder();
        if !rest.is_empty() {
            let v = (*words.add(i)).load(Ordering::Relaxed);
            rest.copy_from_slice(&v.to_ne_bytes()[..rest.len()]);
        }
    }
}

/// Write one 8-byte header word.
///
/// # Safety
/// `dst` must be an 8-aligned pointer into a live mapping.
#[inline]
pub(crate) unsafe fn write_word(dst: *mut u8, v: u64) {
    // SAFETY: preconditions are the function's documented contract.
    unsafe {
        (*(dst as *mut AtomicU64)).store(v, Ordering::Relaxed);
    }
}

/// Read one 8-byte header word.
///
/// # Safety
/// `src` must be an 8-aligned pointer into a live mapping.
#[inline]
pub(crate) unsafe fn read_word(src: *const u8) -> u64 {
    // SAFETY: preconditions are the function's documented contract.
    unsafe { (*(src as *const AtomicU64)).load(Ordering::Relaxed) }
}

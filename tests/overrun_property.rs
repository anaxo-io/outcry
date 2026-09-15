//! The property that justifies the crate: a reader that has been lapped never receives a
//! corrupt frame. Either it gets the frame that was written, or it gets `Overrun`.
//!
//! The producer runs on another thread writing self-describing frames (sequence number +
//! a checksum of the body) at full speed into a small ring while the reader deliberately
//! dawdles. Every frame the reader accepts is verified against its own checksum; any
//! mismatch is a corruption that slipped past the overrun check.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

use outcry::{Error, Queue};
use proptest::prelude::*;

fn checksum(seq: u64, body: &[u8]) -> u64 {
    body.iter()
        .fold(seq.wrapping_mul(0x9E37_79B9_7F4A_7C15), |h, &b| {
            (h ^ b as u64).wrapping_mul(0x100_0000_01B3)
        })
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    #[test]
    fn lapped_reader_never_sees_a_corrupt_frame(
        body_len in 1usize..200,
        stall_every in 1u64..64,
        messages in 5_000u64..40_000,
    ) {
        const CAP: u64 = 4096;
        let q = Queue::anon(CAP).unwrap();
        let done = Arc::new(AtomicBool::new(false));
        // The writer's position, so the reader can stall until it is provably lapped.
        let writer_pos = Arc::new(AtomicU64::new(0));

        let writer = {
            let q = q.clone();
            let done = Arc::clone(&done);
            let writer_pos = Arc::clone(&writer_pos);
            std::thread::spawn(move || {
                let mut p = q.producer().unwrap();
                let mut frame = vec![0u8; 16 + body_len];
                for seq in 0..messages {
                    for (i, b) in frame[16..].iter_mut().enumerate() {
                        *b = (seq as usize + i) as u8;
                    }
                    let sum = checksum(seq, &frame[16..]);
                    frame[..8].copy_from_slice(&seq.to_le_bytes());
                    frame[8..16].copy_from_slice(&sum.to_le_bytes());
                    p.write(&frame).unwrap();
                    writer_pos.store(p.position(), Ordering::Release);
                }
                done.store(true, Ordering::Release);
            })
        };

        let mut c = q.consumer();
        let mut buf = vec![0u8; 256];
        let mut accepted = 0u64;
        let mut overruns = 0u64;
        let mut last_seq: Option<u64> = None;

        loop {
            match c.try_read(&mut buf) {
                Ok(Some(n)) => {
                    prop_assert_eq!(n, 16 + body_len, "frame length changed");
                    let seq = u64::from_le_bytes(buf[..8].try_into().unwrap());
                    let sum = u64::from_le_bytes(buf[8..16].try_into().unwrap());
                    prop_assert_eq!(sum, checksum(seq, &buf[16..n]),
                        "CORRUPT frame accepted at seq {} after {} overruns", seq, overruns);
                    if let Some(prev) = last_seq {
                        prop_assert!(seq > prev, "frames out of order: {} after {}", seq, prev);
                    }
                    last_seq = Some(seq);
                    accepted += 1;
                    // Every `stall_every` frames, stop reading until the writer is two
                    // full rings ahead. The next read is then guaranteed to land in a
                    // region the writer is actively rewriting.
                    if accepted % stall_every == 0 && !done.load(Ordering::Acquire) {
                        let target = c.position() + 2 * CAP;
                        while writer_pos.load(Ordering::Acquire) < target && !done.load(Ordering::Acquire) {
                            std::hint::spin_loop();
                        }
                    }
                }
                Ok(None) => {
                    if done.load(Ordering::Acquire) {
                        // Drain anything published after we last looked.
                        match c.try_read(&mut buf) {
                            Ok(None) => break,
                            _ => continue,
                        }
                    }
                    std::hint::spin_loop();
                }
                Err(Error::Overrun { .. }) => {
                    overruns += 1;
                    c.resync();
                }
                Err(e) => prop_assert!(false, "unexpected error {}", e),
            }
        }
        writer.join().unwrap();

        // The stalls guarantee the reader was lapped, so the property was exercised.
        prop_assert!(accepted > 0);
        prop_assert!(overruns > 0, "the reader stalled until lapped {} times yet was never overrun", accepted / stall_every);
    }
}

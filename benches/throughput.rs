//! Reproduce the measurement from the talk: 73-byte messages, an 8 MiB queue, N readers,
//! messages per second.
//!
//! ```bash
//! cargo bench --bench throughput                     # sound copy (default)
//! cargo bench --bench throughput --features fast-copy
//! OUTCRY_READERS=1,2,4,8 OUTCRY_MESSAGES=5000000 cargo bench --bench throughput
//! ```
//!
//! Each reader runs on its own thread, spinning on `try_read`. The producer writes
//! `OUTCRY_MESSAGES` frames as fast as it can and never waits. Three figures come out:
//!
//! - `writer msg/s`: the producer's rate. This is what the talk reports.
//! - `slowest reader msg/s`: frames received per second by the reader that received the
//!   fewest. If this is below the writer's rate, readers are being lapped.
//! - `overruns`: how many times any reader was lapped. Zero means every reader saw every
//!   frame; the writer figure is then a sustainable broadcast rate. Non-zero means the
//!   writer outran the readers and the honest capacity of the queue is the reader figure.
//!
//! Every frame a reader accepts is checked for in-order sequence; a corrupt or reordered
//! frame fails the run. Thread start-up is inside the timed region, as in the original.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use outcry::Queue;

const MSG: usize = 73;
const CAPACITY: u64 = 8 << 20;

fn env_or<T: std::str::FromStr>(key: &str, default: T) -> T {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

struct Run {
    writer_rate: f64,
    slowest_reader_rate: f64,
    overruns: u64,
}

fn run(readers: usize, messages: u64) -> Run {
    let queue = Queue::anon(CAPACITY).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let t0 = Instant::now();

    let handles: Vec<_> = (0..readers)
        .map(|_| {
            let mut consumer = queue.consumer();
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut buf = [0u8; 256];
                let mut received = 0u64;
                let mut overruns = 0u64;
                let mut last: Option<u64> = None;
                loop {
                    match consumer.try_read(&mut buf) {
                        Ok(Some(n)) => {
                            assert_eq!(n, MSG);
                            let seq = u64::from_le_bytes(buf[..8].try_into().unwrap());
                            if let Some(prev) = last {
                                assert!(
                                    seq > prev,
                                    "reader saw seq {seq} after {prev}: reordered or corrupt"
                                );
                            }
                            last = Some(seq);
                            received += 1;
                            if seq + 1 == messages {
                                break;
                            }
                        }
                        Ok(None) => {
                            if stop.load(Ordering::Acquire) {
                                // Drain whatever was published after our last look.
                                if let Ok(None) = consumer.try_read(&mut buf) {
                                    break;
                                }
                            }
                            std::hint::spin_loop();
                        }
                        Err(outcry::Error::Overrun { .. }) => {
                            overruns += 1;
                            consumer.resync();
                            if stop.load(Ordering::Acquire) {
                                break;
                            }
                        }
                        Err(e) => panic!("{e}"),
                    }
                }
                (received, t0.elapsed(), overruns)
            })
        })
        .collect();

    let mut producer = queue.producer().unwrap();
    let mut frame = [0xABu8; MSG];

    let start = Instant::now();
    for seq in 0..messages {
        frame[..8].copy_from_slice(&seq.to_le_bytes());
        producer.write(&frame).unwrap();
    }
    let elapsed = start.elapsed();
    stop.store(true, Ordering::Release);

    let mut overruns = 0;
    let mut slowest = f64::INFINITY;
    for h in handles {
        let (received, t, o) = h.join().unwrap();
        overruns += o;
        slowest = slowest.min(received as f64 / t.as_secs_f64());
    }

    Run {
        writer_rate: messages as f64 / elapsed.as_secs_f64(),
        slowest_reader_rate: slowest,
        overruns,
    }
}

fn main() {
    let readers: Vec<usize> = std::env::var("OUTCRY_READERS")
        .ok()
        .map(|s| s.split(',').filter_map(|x| x.trim().parse().ok()).collect())
        .unwrap_or_else(|| vec![1, 2, 3, 4, 6, 8]);
    let messages: u64 = env_or("OUTCRY_MESSAGES", 2_000_000);
    let mode = if cfg!(feature = "fast-copy") {
        "fast-copy"
    } else {
        "sound"
    };
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);

    println!("outcry throughput — {MSG}-byte messages, {} MiB queue, copy mode: {mode}, {cores} logical cores", CAPACITY >> 20);
    println!(
        "{:>8} {:>16} {:>22} {:>10}",
        "readers", "writer msg/s", "slowest reader msg/s", "overruns"
    );
    for &r in &readers {
        if r + 1 > cores && cores > 0 {
            println!("{r:>8}   (skipped: needs {} cores, have {cores})", r + 1);
            continue;
        }
        // Warm up once, then measure.
        let _ = run(r, messages / 10);
        let m = run(r, messages);
        println!(
            "{r:>8} {:>16.0} {:>22.0} {:>10}",
            m.writer_rate, m.slowest_reader_rate, m.overruns
        );
    }
}

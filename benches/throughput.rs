//! Reproduce the measurement from the talk: 73-byte messages, an 8 MiB queue, N readers,
//! messages per second.
//!
//! ```bash
//! cargo bench --bench throughput
//! OUTCRY_READERS=1,2,4,8 OUTCRY_MESSAGES=5000000 cargo bench --bench throughput
//! OUTCRY_PIN=0,1,2,3 cargo bench --bench throughput     # writer on 0, readers on 1..
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

mod common;
use common::{describe_pinning, env_list, env_or, pin_list, pin_to};

const MSG: usize = 73;
const CAPACITY: u64 = 8 << 20;

struct Run {
    writer_rate: f64,
    slowest_reader_rate: f64,
    overruns: u64,
}

fn run(readers: usize, messages: u64, pins: &Option<Vec<usize>>) -> Run {
    let queue = Queue::anon(CAPACITY).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let t0 = Instant::now();

    let handles: Vec<_> = (0..readers)
        .map(|i| {
            let mut consumer = queue.consumer();
            let stop = Arc::clone(&stop);
            let pin = pins.as_ref().map(|p| p[i + 1]);
            std::thread::spawn(move || {
                if let Some(core) = pin {
                    pin_to(core);
                }
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
                                // Consecutive, not merely increasing: between overruns
                                // every reader must see every frame exactly once.
                                assert_eq!(
                                    seq,
                                    prev + 1,
                                    "reader saw seq {seq} after {prev}: dropped, repeated or corrupt"
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
                            // Resyncing skips frames on purpose, so the sequence restarts
                            // from wherever the head now is.
                            last = None;
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

    if let Some(p) = pins {
        pin_to(p[0]);
    }
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
    let readers = env_list("OUTCRY_READERS").unwrap_or_else(|| vec![1, 2, 3, 4, 6, 8, 12]);
    let messages: u64 = env_or("OUTCRY_MESSAGES", 3_000_000);
    let cores = std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(0);
    let pins = pin_list(*readers.iter().max().unwrap_or(&0));

    println!(
        "outcry throughput — {MSG}-byte messages, {} MiB queue, {cores} logical cores, {}",
        CAPACITY >> 20,
        describe_pinning(&pins)
    );
    println!(
        "{:>8} {:>16} {:>22} {:>10}",
        "readers", "writer msg/s", "slowest reader msg/s", "overruns"
    );
    // With `OUTCRY_PIN` the core list is the budget. `available_parallelism` reports the
    // process's affinity mask, which on a machine booted with `isolcpus` counts only the
    // housekeeping cores — it would skip every row the isolated cores exist to run.
    let budget = match &pins {
        Some(p) => p.len(),
        None => cores,
    };
    for &r in &readers {
        if budget > 0 && r + 1 > budget {
            println!("{r:>8}   (skipped: needs {} cores, have {budget})", r + 1);
            continue;
        }
        // Warm up once, then measure.
        let _ = run(r, messages / 10, &pins);
        let m = run(r, messages, &pins);
        println!(
            "{r:>8} {:>16.0} {:>22.0} {:>10}",
            m.writer_rate, m.slowest_reader_rate, m.overruns
        );
    }
}

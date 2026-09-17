//! Writer-to-reader latency: how long after `write` returns does `try_read` hand the frame
//! back on another core.
//!
//! ```bash
//! cargo bench --bench latency
//! OUTCRY_PIN=0,1 cargo bench --bench latency               # writer on 0, reader on 1
//! OUTCRY_INTERVAL_NS=500 OUTCRY_SAMPLES=2000000 cargo bench --bench latency
//! ```
//!
//! Throughput measures a writer that never waits, which mostly measures how fast the
//! readers drain. Latency is the other half: the writer sends one frame every
//! `OUTCRY_INTERVAL_NS` (default 1 µs, about the busiest a market data feed gets), so the
//! reader is spinning idle when each one lands and the figure is the queue's own hand-off
//! cost, not queueing depth. Each frame carries a nanosecond timestamp taken from a clock
//! both threads share; the reader records the difference on arrival.
//!
//! Two things are inside the number and cannot be removed: two `Instant::now()` calls
//! (`clock_gettime` through the vDSO, about 20 ns each on modern Linux) and the cross-core
//! cache line transfer, which is what the queue costs on this topology. Pin both threads
//! (`OUTCRY_PIN`) to make that topology fixed between runs.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use outcry::Queue;

mod common;
use common::{describe_pinning, env_or, pin_list, pin_to};

const MSG: usize = 73;
const CAPACITY: u64 = 8 << 20;

fn main() {
    let samples: usize = env_or("OUTCRY_SAMPLES", 1_000_000);
    let interval = Duration::from_nanos(env_or("OUTCRY_INTERVAL_NS", 1_000));
    let readers: usize = env_or("OUTCRY_READERS", 1);
    let pins = pin_list(readers);

    println!(
        "outcry latency — {MSG}-byte messages, {} MiB queue, {readers} reader(s), one frame every {:?}, {samples} samples, {}",
        CAPACITY >> 20,
        interval,
        describe_pinning(&pins)
    );

    let queue = Queue::anon(CAPACITY).unwrap();
    let epoch = Instant::now();
    let done = Arc::new(AtomicBool::new(false));

    let handles: Vec<_> = (0..readers)
        .map(|i| {
            let mut consumer = queue.consumer();
            let done = Arc::clone(&done);
            let pin = pins.as_ref().map(|p| p[i + 1]);
            std::thread::spawn(move || {
                if let Some(core) = pin {
                    pin_to(core);
                }
                let mut buf = [0u8; 256];
                let mut lat = Vec::with_capacity(samples);
                let mut draining = false;
                loop {
                    match consumer.try_read(&mut buf) {
                        Ok(Some(_)) => {
                            let sent = u64::from_le_bytes(buf[..8].try_into().unwrap());
                            let now = epoch.elapsed().as_nanos() as u64;
                            lat.push(now - sent);
                            if lat.len() == samples {
                                break;
                            }
                        }
                        Ok(None) => {
                            // `done` is stored after the last publish, so once it is seen
                            // the next read is guaranteed to find whatever is left; only
                            // an empty read *after* that means the stream is finished.
                            if draining {
                                break;
                            }
                            draining = done.load(Ordering::Acquire);
                            std::hint::spin_loop();
                        }
                        Err(e) => panic!("reader {i}: {e}"),
                    }
                }
                lat
            })
        })
        .collect();

    if let Some(p) = &pins {
        pin_to(p[0]);
    }
    let mut producer = queue.producer().unwrap();
    let mut frame = [0xABu8; MSG];
    // Let the readers reach their spin loops before the clock matters.
    std::thread::sleep(Duration::from_millis(50));

    let mut next = Instant::now();
    for _ in 0..samples {
        while Instant::now() < next {
            std::hint::spin_loop();
        }
        next += interval;
        let stamp = epoch.elapsed().as_nanos() as u64;
        frame[..8].copy_from_slice(&stamp.to_le_bytes());
        producer.write(&frame).unwrap();
    }
    done.store(true, Ordering::Release);

    println!(
        "{:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}",
        "reader", "p50 ns", "p90 ns", "p99 ns", "p99.9 ns", "p99.99", "max ns"
    );
    for (i, h) in handles.into_iter().enumerate() {
        let mut lat = h.join().unwrap();
        assert_eq!(lat.len(), samples, "reader {i} missed frames");
        lat.sort_unstable();
        let p = |q: f64| lat[((lat.len() as f64 * q) as usize).min(lat.len() - 1)];
        println!(
            "{i:>7} {:>9} {:>9} {:>9} {:>9} {:>9} {:>9}",
            p(0.50),
            p(0.90),
            p(0.99),
            p(0.999),
            p(0.9999),
            lat[lat.len() - 1]
        );
    }
}

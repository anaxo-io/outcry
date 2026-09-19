//! The reader half of the README quick start. Compiled by CI so it cannot rot.
use std::time::{Duration, Instant};

use outcry::Queue;

const PATH: &str = "/dev/shm/prices";

fn main() -> Result<(), outcry::Error> {
    let mut queue = Queue::open(PATH)?;
    // A consumer starts at the current head, so start it before the frames you want:
    // whatever the producer wrote earlier is already gone.
    let mut consumer = queue.consumer();

    let mut buf = [0u8; 256];
    let mut quiet_since = Instant::now();
    loop {
        match consumer.try_read(&mut buf)? {
            // `?` above exits on Overrun; a real reader calls consumer.resync() instead.
            Some(n) => {
                println!("{}", String::from_utf8_lossy(&buf[..n]));
                quiet_since = Instant::now();
            }
            None => {
                // Silence has two causes and they look the same from here: the writer is
                // idle, or it restarted and replaced the queue, leaving us attached to a
                // file that will never change again. Only the path can tell them apart.
                if quiet_since.elapsed() > Duration::from_secs(1) {
                    let current = Queue::open(PATH)?;
                    if current.instance() != queue.instance() {
                        queue = current;
                        consumer = queue.consumer();
                    }
                    quiet_since = Instant::now();
                }
                std::hint::spin_loop();
            }
        }
    }
}

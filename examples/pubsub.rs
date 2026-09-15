//! Two processes talking through `/dev/shm`.
//!
//! ```bash
//! cargo run --example pubsub -- produce      # in one terminal
//! cargo run --example pubsub -- consume      # in one or more others
//! ```

use std::time::{Duration, Instant};

use outcry::{Error, Queue};

const PATH: &str = "/dev/shm/outcry-example";

fn main() -> Result<(), Error> {
    match std::env::args().nth(1).as_deref() {
        Some("produce") => produce(),
        Some("consume") => consume(),
        _ => {
            eprintln!("usage: pubsub produce|consume");
            Ok(())
        }
    }
}

fn produce() -> Result<(), Error> {
    let queue = Queue::create(PATH, 1 << 20)?;
    let mut producer = queue.producer()?;
    println!("producing to {PATH}; Ctrl-C to stop");
    let start = Instant::now();
    let mut seq = 0u64;
    loop {
        let msg = format!("tick {seq} at {:?}", start.elapsed());
        producer.write(msg.as_bytes())?;
        seq += 1;
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn consume() -> Result<(), Error> {
    let queue = Queue::open(PATH)?;
    let mut consumer = queue.consumer();
    println!("consuming from {PATH}; starting at the current head");
    let mut buf = [0u8; 256];
    loop {
        match consumer.try_read(&mut buf) {
            Ok(Some(n)) => println!("{}", String::from_utf8_lossy(&buf[..n])),
            Ok(None) => std::thread::sleep(Duration::from_millis(5)),
            Err(Error::Overrun { behind }) => {
                let skipped = consumer.resync();
                println!("overrun: writer was {behind} bytes ahead; skipped {skipped} bytes");
            }
            Err(e) => return Err(e),
        }
    }
}

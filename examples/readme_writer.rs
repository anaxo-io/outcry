//! The writer half of the README quick start. Compiled by CI so it cannot rot.
use outcry::Queue;

fn main() -> Result<(), outcry::Error> {
    let queue = Queue::create("/dev/shm/prices", 8 << 20)?; // 8 MiB ring
    let mut producer = queue.producer()?;
    for tick in 0..1_000u32 {
        producer.write(format!("BTC-USD {tick}").as_bytes())?;
    }
    Ok(())
}

//! The reader half of the README quick start. Compiled by CI so it cannot rot.
use outcry::Queue;

fn main() -> Result<(), outcry::Error> {
    let queue = Queue::open("/dev/shm/prices")?;
    // A consumer starts at the current head, so start it before the frames you want:
    // whatever the producer wrote earlier is already gone.
    let mut consumer = queue.consumer();

    let mut buf = [0u8; 256];
    loop {
        match consumer.try_read(&mut buf)? {
            // `?` here exits on Overrun;
            Some(n) => println!("{}", String::from_utf8_lossy(&buf[..n])),
            None => std::hint::spin_loop(), // a real reader calls consumer.resync()
        }
    }
}

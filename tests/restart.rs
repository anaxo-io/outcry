//! Restarting a writer must not take its readers down with it.
//!
//! `Queue::create` used to open the existing path with `truncate(true)` and re-extend it.
//! The inode was reused, so every process already mapped to the queue was briefly pointing
//! past end of file, and the first byte it touched raised `SIGBUS` — which kills a process
//! outright: no unwinding, no error, nothing in its log. That is the normal operational
//! case for a long-running deployment, so it is tested here.
//!
//! File-backed, so this file is not run under Miri.

use std::os::unix::fs::MetadataExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use outcry::Queue;

const CAP: u64 = 4096;

#[test]
fn create_replaces_the_file_rather_than_truncating_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");

    let first = Queue::create(&path, CAP).unwrap();
    let ino_before = std::fs::metadata(&path).unwrap().ino();

    let second = Queue::create(&path, CAP).unwrap();
    let ino_after = std::fs::metadata(&path).unwrap().ino();

    // The whole safety property in one assertion: a new inode means readers mapped to the
    // old one still have pages under them. A reused inode means SIGBUS.
    assert_ne!(
        ino_before, ino_after,
        "create must replace the file, not truncate it in place"
    );
    assert_ne!(
        first.instance(),
        second.instance(),
        "each creation is a distinct queue and says so"
    );
}

#[test]
fn a_reader_survives_the_writer_restarting_under_it() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");

    let writer = Queue::create(&path, CAP).unwrap();
    let mut producer = writer.producer().unwrap();
    let reader = Queue::open(&path).unwrap();
    let mut consumer = reader.consumer();

    let mut buf = [0u8; 64];
    producer.write(b"before the restart").unwrap();
    let n = consumer.try_read(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], b"before the restart");

    // The writer restarts against the same path.
    let replacement = Queue::create(&path, CAP).unwrap();

    // The old reader is still alive and still holds a coherent view of the old queue:
    // it is empty, not corrupt, and reading it does not fault.
    assert_eq!(consumer.try_read(&mut buf).unwrap(), None);

    // And the old queue really is intact rather than merely quiet — the old producer can
    // still write into it and the old consumer still sees it. Both sides are looking at a
    // file that no longer has a name.
    producer.write(b"still coherent").unwrap();
    let n = consumer.try_read(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], b"still coherent");

    // Nothing the replacement publishes reaches the old reader.
    let mut new_producer = replacement.producer().unwrap();
    new_producer.write(b"on the new queue").unwrap();
    assert_eq!(consumer.try_read(&mut buf).unwrap(), None);
}

#[test]
fn a_reader_can_tell_it_was_replaced_rather_than_left_idle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");

    let original = Queue::create(&path, CAP).unwrap();
    let reader = Queue::open(&path).unwrap();
    assert_eq!(reader.instance(), original.instance());

    // An idle writer: the instance at the path is still the one we attached to.
    assert_eq!(Queue::open(&path).unwrap().instance(), reader.instance());

    let replacement = Queue::create(&path, CAP).unwrap();

    // A replaced writer: the path now names a different queue, which is the only way to
    // distinguish this from silence.
    assert_ne!(Queue::open(&path).unwrap().instance(), reader.instance());
    assert_eq!(
        Queue::open(&path).unwrap().instance(),
        replacement.instance()
    );
}

#[test]
fn a_spinning_reader_is_not_killed_by_a_concurrent_restart() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");
    let writer = Queue::create(&path, CAP).unwrap();
    let mut producer = writer.producer().unwrap();

    let reader = Queue::open(&path).unwrap();
    let stop = Arc::new(AtomicBool::new(false));
    let reader_stop = Arc::clone(&stop);
    let spinning = std::thread::spawn(move || {
        let mut consumer = reader.consumer();
        let mut buf = [0u8; 64];
        let mut seen = 0u64;
        while !reader_stop.load(Ordering::Acquire) {
            // Touching the mapping is the dangerous act: under the old `create` this is
            // where SIGBUS landed, and a signal would take the whole test binary with it.
            if let Ok(Some(_)) = consumer.try_read(&mut buf) {
                seen += 1;
            }
            std::hint::spin_loop();
        }
        seen
    });

    // Restart repeatedly while the reader hammers its mapping.
    for i in 0..20 {
        producer.write(format!("frame {i}").as_bytes()).unwrap();
        let replacement = Queue::create(&path, CAP).unwrap();
        producer = replacement.producer().unwrap();
        std::mem::forget(replacement);
    }

    stop.store(true, Ordering::Release);
    // Reaching this line at all is the assertion: the thread was never signalled.
    let seen = spinning.join().expect("the reader thread was killed");
    assert!(seen <= 20, "a reader cannot see more than was written");
}

#[test]
fn a_failed_create_leaves_no_temporary_file_behind() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");

    assert!(Queue::create(&path, 4095).is_err(), "capacity is rejected");
    let strays: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert!(
        strays.is_empty(),
        "a rejected create must not litter the directory: {strays:?}"
    );

    Queue::create(&path, CAP).unwrap();
    let names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(names.len(), 1, "only the queue itself remains: {names:?}");
}

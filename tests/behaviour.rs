//! Functional behaviour: round trips, wrap-around, late join, many readers, overrun.

use outcry::{Error, Queue};

const CAP: u64 = 4096;

#[test]
fn round_trip_in_order() {
    let q = Queue::anon(CAP).unwrap();
    let mut p = q.producer().unwrap();
    let mut c = q.consumer();
    let mut buf = [0u8; 128];

    assert_eq!(c.try_read(&mut buf).unwrap(), None);
    p.write(b"one").unwrap();
    p.write(b"").unwrap();
    p.write(&[7u8; 100]).unwrap();

    let n = c.try_read(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], b"one");
    assert_eq!(c.try_read(&mut buf).unwrap(), Some(0));
    let n = c.try_read(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], &[7u8; 100]);
    assert_eq!(c.try_read(&mut buf).unwrap(), None);
}

#[test]
fn only_one_producer_per_handle() {
    let q = Queue::anon(CAP).unwrap();
    let _p = q.producer().unwrap();
    assert!(q.producer().is_err());
    assert!(
        q.clone().producer().is_err(),
        "the guard is shared by clones"
    );
}

#[test]
fn frames_never_straddle_the_end() {
    // Sizes chosen so frames land at every offset relative to the ring end.
    let q = Queue::anon(CAP).unwrap();
    let mut p = q.producer().unwrap();
    let mut c = q.consumer();
    let mut buf = [0u8; 512];

    for i in 0..2000u32 {
        let len = (i * 37 % 300) as usize;
        let payload: Vec<u8> = (0..len).map(|k| (k as u32 ^ i) as u8).collect();
        p.write(&payload).unwrap();
        let n = c
            .try_read(&mut buf)
            .unwrap()
            .expect("frame should be there");
        assert_eq!(&buf[..n], &payload[..], "frame {i} corrupted at the wrap");
    }
}

#[test]
fn late_joiner_starts_at_the_head() {
    let q = Queue::anon(CAP).unwrap();
    let mut p = q.producer().unwrap();
    p.write(b"before").unwrap();
    let mut c = q.consumer();
    let mut buf = [0u8; 64];
    assert_eq!(c.try_read(&mut buf).unwrap(), None, "must not see history");
    p.write(b"after").unwrap();
    let n = c.try_read(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], b"after");
}

#[test]
fn many_readers_see_identical_streams() {
    let q = Queue::anon(CAP).unwrap();
    let mut p = q.producer().unwrap();
    let mut readers: Vec<_> = (0..8).map(|_| q.consumer()).collect();
    let mut buf = [0u8; 64];

    for i in 0..50u8 {
        p.write(&[i; 20]).unwrap();
        for c in readers.iter_mut() {
            let n = c.try_read(&mut buf).unwrap().unwrap();
            assert_eq!(&buf[..n], &[i; 20]);
        }
    }
}

#[test]
fn buffer_too_small_does_not_consume() {
    let q = Queue::anon(CAP).unwrap();
    let mut p = q.producer().unwrap();
    let mut c = q.consumer();
    p.write(&[1u8; 100]).unwrap();

    let mut small = [0u8; 10];
    assert!(matches!(
        c.try_read(&mut small),
        Err(Error::BufferTooSmall {
            needed: 100,
            provided: 10
        })
    ));
    let mut big = [0u8; 100];
    assert_eq!(c.try_read(&mut big).unwrap(), Some(100));
}

#[test]
fn oversized_message_is_refused() {
    let q = Queue::anon(CAP).unwrap();
    let mut p = q.producer().unwrap();
    let too_big = vec![0u8; p.max_payload() + 1];
    assert!(matches!(
        p.write(&too_big),
        Err(Error::MessageTooLarge { .. })
    ));
    let ok = vec![0u8; p.max_payload()];
    p.write(&ok).unwrap();
}

#[test]
fn a_lapped_reader_is_told_and_can_resync() {
    let q = Queue::anon(CAP).unwrap();
    let mut p = q.producer().unwrap();
    let mut c = q.consumer();
    let mut buf = [0u8; 64];

    p.write(b"first").unwrap();
    // Write more than a full ring without the reader keeping up.
    for _ in 0..200 {
        p.write(&[0u8; 40]).unwrap();
    }

    let err = c.try_read(&mut buf).unwrap_err();
    assert!(matches!(err, Error::Overrun { .. }), "got {err}");
    assert!(c.is_overrun());
    // Sticky until resync.
    assert!(matches!(c.try_read(&mut buf), Err(Error::Overrun { .. })));

    let skipped = c.resync();
    assert!(skipped > CAP, "skipped {skipped}");
    assert!(!c.is_overrun());
    assert_eq!(c.try_read(&mut buf).unwrap(), None);

    p.write(b"fresh").unwrap();
    let n = c.try_read(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], b"fresh");
}

#[test]
fn zero_copy_write_with() {
    let q = Queue::anon(CAP).unwrap();
    let mut p = q.producer().unwrap();
    let mut c = q.consumer();
    p.write_with(5, |b| b.copy_from_slice(b"hello")).unwrap();
    let mut buf = [0u8; 8];
    let n = c.try_read(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], b"hello");
}

#[test]
fn file_backed_queue_is_shared_between_mappings() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");
    let producer_side = Queue::create(&path, CAP).unwrap();
    let consumer_side = Queue::open(&path).unwrap();
    assert_eq!(consumer_side.capacity(), CAP);

    let mut p = producer_side.producer().unwrap();
    let mut c = consumer_side.consumer();
    let mut buf = [0u8; 32];
    p.write(b"across the mapping").unwrap();
    let n = c.try_read(&mut buf).unwrap().unwrap();
    assert_eq!(&buf[..n], b"across the mapping");
}

#[test]
fn open_rejects_foreign_files() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("junk");
    std::fs::write(&path, vec![0u8; 8192]).unwrap();
    assert!(matches!(Queue::open(&path), Err(Error::BadHeader(_))));
    std::fs::write(&path, b"short").unwrap();
    assert!(matches!(Queue::open(&path), Err(Error::BadHeader(_))));
}

#[test]
fn bad_capacity_is_refused() {
    assert!(matches!(Queue::anon(4095), Err(Error::BadCapacity(4095))));
    assert!(matches!(Queue::anon(6000), Err(Error::BadCapacity(6000))));
    assert!(matches!(Queue::anon(1024), Err(Error::BadCapacity(1024))));
}

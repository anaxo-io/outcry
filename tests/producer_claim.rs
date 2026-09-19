//! One producer per queue, across handles and across processes.
//!
//! Two writers on one queue corrupt it silently: both advance `published`, their frames
//! interleave, and a reader returns bytes that pass every check it makes and were never
//! written as one frame. No overrun, no error. So the claim is worth testing properly,
//! including from a second process, which is where the flag alone never helped.
//!
//! File-backed, so this file is not run under Miri.

use std::process::Command;

use outcry::Queue;

const CAP: u64 = 4096;

/// Set by the parent of [`child_tries_to_take_the_producer`]; unset in a normal run.
const CHILD_ENV: &str = "OUTCRY_TEST_LOCKED_QUEUE";

#[test]
fn a_second_handle_on_the_same_file_cannot_produce() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");

    let first = Queue::create(&path, CAP).unwrap();
    let _producer = first.producer().unwrap();

    // A separate `open` has its own flag, so before the file lock this succeeded and gave
    // out a second writer on the same ring.
    let second = Queue::open(&path).unwrap();
    assert!(
        second.producer().is_err(),
        "a second handle on the same queue must not get a producer"
    );
}

#[test]
fn dropping_a_producer_frees_the_claim() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");
    let queue = Queue::create(&path, CAP).unwrap();

    let producer = queue.producer().unwrap();
    assert!(queue.producer().is_err());
    drop(producer);

    // Both the handle's flag and the file lock must have been given back.
    let again = queue.producer().expect("the claim is free once dropped");
    let other = Queue::open(&path).unwrap();
    assert!(
        other.producer().is_err(),
        "and the new producer holds it just as firmly"
    );
    drop(again);
    assert!(
        other.producer().is_ok(),
        "a different handle can take it once it is free"
    );
}

#[test]
fn a_refused_claim_does_not_consume_the_handle() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");
    let holder = Queue::create(&path, CAP).unwrap();
    let producer = holder.producer().unwrap();

    let waiting = Queue::open(&path).unwrap();
    assert!(waiting.producer().is_err());
    assert!(
        waiting.producer().is_err(),
        "still refused, still not poisoned"
    );

    drop(producer);
    assert!(
        waiting.producer().is_ok(),
        "the handle that was refused must be able to succeed later"
    );
}

#[test]
fn a_restarting_writer_does_not_contend_with_the_old_queue() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");

    let old = Queue::create(&path, CAP).unwrap();
    let _old_producer = old.producer().unwrap();

    // The replacement is a different file, so it has its own claim. These are two queues
    // and nothing interleaves; refusing here would make a restart impossible.
    let new = Queue::create(&path, CAP).unwrap();
    assert!(
        new.producer().is_ok(),
        "creating a fresh queue must not be blocked by the writer of the old one"
    );
}

#[test]
fn an_anonymous_queue_is_guarded_by_the_handle_alone() {
    let queue = Queue::anon(CAP).unwrap();
    let producer = queue.producer().unwrap();
    assert!(queue.producer().is_err());
    assert!(queue.clone().producer().is_err(), "clones share the flag");
    drop(producer);
    assert!(queue.producer().is_ok(), "and it is released on drop");
}

#[test]
fn a_second_process_cannot_take_the_producer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("q");
    let queue = Queue::create(&path, CAP).unwrap();
    let producer = queue.producer().unwrap();

    // Re-run this test binary, asking only for the child test below. A flag in this
    // process is invisible to it; only the file lock is not.
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_tries_to_take_the_producer"])
        .env(CHILD_ENV, &path)
        .status()
        .expect("spawning the child test binary");
    assert!(
        status.success(),
        "a second process took the producer: {status:?}"
    );

    // And once this process lets go, the next one may have it.
    drop(producer);
    let status = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_tries_to_take_the_producer"])
        .env(CHILD_ENV, &path)
        .env("OUTCRY_TEST_EXPECT_FREE", "1")
        .status()
        .expect("spawning the child test binary");
    assert!(
        status.success(),
        "a released claim must be available to another process: {status:?}"
    );
}

/// The child half of [`a_second_process_cannot_take_the_producer`]. Without the
/// environment variable it does nothing, so a normal run of the suite simply passes it.
#[test]
fn child_tries_to_take_the_producer() {
    let Ok(path) = std::env::var(CHILD_ENV) else {
        return;
    };
    let expect_free = std::env::var("OUTCRY_TEST_EXPECT_FREE").is_ok();
    let queue = Queue::open(&path).expect("the queue file is readable");
    let got = queue.producer().is_ok();
    assert_eq!(
        got,
        expect_free,
        "child expected the claim to be {}",
        if expect_free { "free" } else { "held" }
    );
}

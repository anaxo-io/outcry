# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0] - 2026-09-19

### Fixed

- Two producers could hold one queue. `producer_taken` was a flag on the `Queue` handle,
  so two `Queue::open` calls — in one process or in several — each handed out a writer for
  the same ring. The corruption that follows is silent: both advance `published`, their
  frames interleave, and a reader returns bytes that satisfy every check it makes and were
  never written as one frame. The claim is now an exclusive advisory lock on the queue
  file, which other processes can see and which the kernel releases when the holder dies,
  so a crashed writer leaves nothing to clean up. The lock belongs to the queue rather
  than the path: a writer restarting through `Queue::create` builds a new queue and takes
  its claim without contending with a writer still feeding the old one. (#2)
- Dropping a `Producer` never cleared `producer_taken`, so a handle could produce exactly
  once for its whole life even after the producer was gone. `Producer` now has a `Drop`
  that releases the lock and the flag, and a refused claim no longer consumes the flag
  either.
- `Queue::create` truncated an existing file in place and re-extended it, reusing the
  inode. Every process already mapped to that queue was briefly pointing past end of file,
  and the first byte it touched raised `SIGBUS` — which kills a process outright, with no
  unwinding, no error and nothing in its log. Restarting a writer was therefore fatal to
  every reader, which is the normal operational case for a long-running deployment. The
  new queue is now built in a temporary file beside the target and `rename`d into place:
  atomic, so a concurrent `open` sees either the old queue or a complete new one, and a
  new inode, so existing readers keep valid pages and observe a queue that has stopped
  rather than dying. A create that fails removes its temporary file. (#1)
- `Queue::open` validated the static header and trusted the live counters. A file whose
  `published` was not frame-aligned put a producer or consumer on an odd address, where
  the frame word becomes an unaligned `AtomicU64`: undefined behaviour reached through
  entirely safe calls. `open` now also requires both counters to be frame-aligned and the
  reservation to sit between the published end and one ring beyond it.
- A corrupt length word could drive a read past the end of the mapping. A consumer parked
  near the end of the ring accepted a frame whose payload started past the last byte,
  because the length was bounded against the capacity but not against the distance to the
  ring end. `try_read` now refuses a frame that does not fit between its own start and the
  end of the ring, as an overrun.
- A length just below the pad marker overflowed `align_up` inside `frame_size` before any
  check saw it: a panic in debug, wrapped arithmetic in release. The length is now
  validated against the maximum payload before it is used in arithmetic.
- The magic is read with an acquire load rather than as part of a non-atomic read of the
  whole header, pairing with the release store that publishes it.
- The crate no longer builds where `usize` is narrower than 64 bits, which would truncate
  positions and lengths on the way into the pointer arithmetic.

### Changed

- **MSRV is now 1.89**, up from 1.85, for `File::try_lock` — stabilised in 1.89.0 and used
  for the producer claim above. The alternative was `libc::flock`, which would have kept
  the MSRV at the cost of a new direct dependency and another `unsafe` block in a crate
  whose discipline is that `unsafe` appears in two files for reasons that are written
  down. The CI MSRV job builds against 1.89 to keep the claim honest.
- **The on-page layout version is now 2**, because `RawHeader` gained an `instance` field
  in space that was reserved. Existing queue files no longer open, which for a queue that
  does not outlive a reboot costs little and buys something: a version-1 writer still
  truncates in place, so refusing to interoperate turns a future `SIGBUS` into a
  `BadHeader` at attach time.

### Removed

- The `fast-copy` feature. It replaced the word-wise atomic copy with
  `ptr::copy_nonoverlapping`, which is a data race under the abstract machine caught only
  after the fact — the trade the C++ original makes. Benchmarking it over 7 runs per point
  settled the question: it was slower at every reader count, 0.65x the sound path with one
  reader and 0.80x with twelve. At 73-byte payloads `copy_nonoverlapping` calls a generic
  `memcpy` while the atomic word loop inlines to plain `mov`s, so the data race bought
  nothing at all. The earlier single-sample figures that suggested otherwise were thread
  placement luck on a two-complex CPU. The comparison stays in the README; the code is in
  history.

### Added

- Unit tests in `src/consumer.rs` that craft malformed ring states over anonymous memory,
  so they run under Miri — and a CI step, `cargo miri test --lib`, that runs them. The
  file-backed equivalents in `tests/malformed.rs` cannot: Miri has no file mapping, so
  they only ever showed that an `Err` came back, never that no out-of-bounds pointer was
  formed. Removing the ring-end check makes Miri report undefined behaviour one word past
  the end of the mapping, which is what gives these tests their value. ASan was considered
  and rejected: it guards `malloc` allocations with redzones and knows nothing about the
  logical end of an `mmap` region, so the read in question is invisible to it. (#3)
- `Queue::instance`, the identifier assigned when a queue is created. Because a replaced
  queue leaves its readers mapped to an intact file that will never change again, a
  replaced writer and a merely idle one are indistinguishable from inside the mapping;
  comparing this value with the one currently at the path separates them. It is
  nanoseconds since the epoch, so it doubles as a creation timestamp.
- `benches/latency.rs`: writer-to-reader hand-off latency with a paced writer, reported as
  p50 through p99.99 and max. Throughput measures how fast readers drain; this measures
  what the queue costs per frame.
- `OUTCRY_PIN` for both benchmarks: pin the writer and each reader to named cores. On a
  CPU with more than one L3 the scheduler otherwise decides per run whether writer and
  reader share a cache, and the results differ by 5x; the README tables are now reported
  per placement, as a median of 7 runs with the range. With `OUTCRY_PIN` set, the core
  list is also the core budget: `available_parallelism` reports the process's affinity
  mask, which on a machine booted with `isolcpus` counts only the housekeeping cores and
  would skip every row the isolated cores exist to run.
- `libc` as a **dev-dependency only**, for `sched_setaffinity` behind `OUTCRY_PIN`. It was
  already in the tree transitively through `memmap2`, so this adds nothing to a
  downstream build; the crate itself still depends on `bytemuck`, `memmap2` and
  `thiserror` alone. A pinning crate such as `core_affinity` would have been a new
  dependency for ten lines of code.
- README numbers now come from a rentable, isolated reference machine (Scaleway Elastic
  Metal, Xeon E3-1231 v3, `isolcpus`), with the multi-reader scaling curve kept separate
  and attributed to the desktop that can produce it. Latency is reported for both
  separate-core and SMT-sibling placement: siblings share L1 and are the fastest hand-off
  (96 ns against 149 ns at p50) at the cost of a noisier tail.
- `tests/malformed.rs`: crafts queue files the way a corrupted or hostile peer could —
  unaligned counters, a reversed pair, a reservation a full ring ahead, a frame running
  past the ring end, an absurd length — and drives the safe API over each.

## [0.1.0] - 2026-09-15

Initial release: the `FastQueue` design from David Gross's CppCon 2024 talk *When
Nanoseconds Matter*, in Rust.

### Added

- `Queue::create` / `Queue::open` over a file (`/dev/shm` on Linux) for cross-process use,
  and `Queue::anon` for a single process. A 64-byte header with magic, layout version and
  capacity is checked on attach.
- `Producer`: single writer, variable-length frames, allocation-free `write`, and
  `write_with` for serialising into a producer-owned buffer instead of a fresh `Vec` per
  message. Frames never straddle the end of the ring; a pad frame covers the remainder.
- `Consumer`: any number, each sees every frame from the moment it joins. `try_read` never
  blocks. A reader the writer has lapped gets `Error::Overrun` — never a corrupt frame —
  and `resync` to rejoin at the head.
- All three cache-line optimisations from the talk: the writer publishes its reservation
  in blocks (1/16 of the ring) rather than per message; payloads are 8-byte aligned; the
  reader caches the published position and only re-reads the shared line when caught up.
- Two copy modes. The default copies through `AtomicU64` words and is free of data races
  by construction; the `fast-copy` feature uses `ptr::copy_nonoverlapping`, as the
  original does, and relies on the overrun check to catch a race after the fact.
- A property test that stalls a reader until the writer has lapped it, then verifies every
  accepted frame against its own checksum, in both copy modes.
- A throughput benchmark reproducing the talk's setup: 73-byte messages, 8 MiB ring,
  1–12 readers.

### Differences from the original

- The reader's second overrun check compares against the *start* of the frame it just
  read, not the advanced position. Checking against the advanced position leaves a window
  of one frame in which an overwrite of the frame's first bytes passes undetected.
- Overrun is an error value, not an assertion. The reader decides what to do.
- Late-joining readers start at the current head rather than at zero.

### Known limitations

- One producer per queue is enforced within a process, not across processes. Two
  processes both calling `producer()` on the same file corrupt it. `flock` would close
  this and is not yet used.
- No cross-host transport; no persistence; no work distribution. See the README for what
  to use instead.
- Miri covers the sound copy path on anonymous memory only; it cannot map files.

[Unreleased]: https://github.com/anaxo-io/outcry/compare/v0.2.0...HEAD
[0.2.0]: https://github.com/anaxo-io/outcry/releases/tag/v0.2.0
[0.1.0]: https://github.com/anaxo-io/outcry/releases/tag/v0.1.0

# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.0] - 2026-09-15

Initial release: the `FastQueue` design from David Gross's CppCon 2024 talk *When
Nanoseconds Matter*, in Rust.

### Added

- `Queue::create` / `Queue::open` over a file (`/dev/shm` on Linux) for cross-process use,
  and `Queue::anon` for a single process. A 64-byte header with magic, layout version and
  capacity is checked on attach.
- `Producer`: single writer, variable-length frames, allocation-free `write`, and
  `write_with` for serialising straight into the ring. Frames never straddle the end of
  the ring; a pad frame covers the remainder.
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

[Unreleased]: https://github.com/anaxo-io/outcry/compare/v0.1.0...HEAD
[0.1.0]: https://github.com/anaxo-io/outcry/releases/tag/v0.1.0
